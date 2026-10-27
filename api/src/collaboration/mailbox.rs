//! Durable, bounded inboxes. Acceptance and acknowledgement are separate:
//! the runtime acknowledges only after committing the receiving event.
use agent_core::AgentMessage;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::watch;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Ledger {
    next_id: u64,
    inboxes: BTreeMap<String, Vec<AgentMessage>>,
    completions: BTreeMap<String, u64>,
    user_stopped: BTreeSet<String>,
    #[serde(default)]
    deliveries: BTreeMap<String, u64>,
}

impl Ledger {
    fn record_delivery(&mut self, recipient: &str) -> io::Result<()> {
        let revision = self.deliveries.entry(recipient.into()).or_default();
        *revision = revision
            .checked_add(1)
            .ok_or_else(|| io::Error::other("mailbox revision exhausted"))?;
        Ok(())
    }
}

pub(crate) struct Mailboxes {
    path: PathBuf,
    ledger: Mutex<Ledger>,
    uncertain: AtomicBool,
    changed: watch::Sender<u64>,
}

impl Mailboxes {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let ledger = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ledger::default(),
            Err(error) => return Err(error),
        };
        let (changed, _) = watch::channel(0);
        Ok(Self {
            path,
            ledger: Mutex::new(ledger),
            uncertain: AtomicBool::new(false),
            changed,
        })
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    pub fn notify(&self) {
        self.changed
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }

    fn update<R>(&self, mutate: impl FnOnce(&mut Ledger) -> io::Result<R>) -> io::Result<R> {
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| io::Error::other("mailbox lock poisoned"))?;
        if self.uncertain.load(Ordering::Acquire) {
            *ledger = serde_json::from_slice(&fs::read(&self.path)?).map_err(io::Error::other)?;
            self.uncertain.store(false, Ordering::Release);
        }
        let mut next = ledger.clone();
        let result = mutate(&mut next)?;
        if let Err(failure) = write_durable(&self.path, &next) {
            if failure.renamed {
                self.uncertain.store(true, Ordering::Release);
                // Rename can succeed while syncing its directory fails. Keep
                // the published generation and reconcile visible state before
                // accepting another write, never reusing its message IDs.
                *ledger = next;
                if let Ok(bytes) = fs::read(&self.path)
                    && let Ok(visible) = serde_json::from_slice(&bytes)
                {
                    *ledger = visible;
                    self.uncertain.store(false, Ordering::Release);
                }
            }
            // Before rename, the authoritative file and accepted ledger were
            // not changed. In particular a failed first write has no file to
            // recover from and must remain retryable once storage is repaired.
            return Err(failure.error);
        }
        *ledger = next;
        drop(ledger);
        self.notify();
        Ok(result)
    }

    pub fn send(&self, recipient: &str, sender: &str, text: String) -> io::Result<String> {
        if text.trim().is_empty() || text.len() > 16 * 1024 {
            return Err(io::Error::other(
                "agent messages must contain 1–16384 bytes",
            ));
        }
        self.update(|ledger| {
            let inbox = ledger.inboxes.entry(recipient.into()).or_default();
            if inbox.len() >= 128
                || inbox.iter().map(|m| m.text.len()).sum::<usize>() + text.len() > 512 * 1024
            {
                return Err(io::Error::other(
                    "recipient inbox is full; wait for it to process messages",
                ));
            }
            ledger.next_id = ledger
                .next_id
                .checked_add(1)
                .ok_or_else(|| io::Error::other("message ID exhausted"))?;
            let id = format!("agent-message-{}", ledger.next_id);
            inbox.push(AgentMessage {
                id: id.clone(),
                sender: sender.into(),
                text,
            });
            ledger.record_delivery(recipient)?;
            Ok(id)
        })
    }

    /// Completion has a deterministic ID and its own reserved capacity. A full
    /// ordinary inbox must never silently discard a child's terminal outcome.
    pub fn completion(
        &self,
        recipient: &str,
        child: &str,
        sequence: u64,
        text: String,
    ) -> io::Result<()> {
        if !self.needs_completion(child, sequence)? {
            return Ok(());
        }
        self.update(|ledger| {
            if ledger
                .completions
                .get(child)
                .is_some_and(|previous| *previous >= sequence)
            {
                return Ok(());
            }
            ledger
                .inboxes
                .entry(recipient.into())
                .or_default()
                .push(AgentMessage {
                    id: format!("agent-result-{child}-{sequence}"),
                    sender: child.into(),
                    text,
                });
            ledger.completions.insert(child.into(), sequence);
            ledger.record_delivery(recipient)?;
            Ok(())
        })
    }

    pub fn pending(&self, recipient: &str) -> io::Result<Vec<AgentMessage>> {
        if self.uncertain.load(Ordering::Acquire) {
            return Err(io::Error::other(
                "message ledger requires recovery after an uncertain write",
            ));
        }
        Ok(self
            .ledger
            .lock()
            .map_err(|_| io::Error::other("mailbox lock poisoned"))?
            .inboxes
            .get(recipient)
            .map(|messages| messages.iter().take(8).cloned().collect())
            .unwrap_or_default())
    }
    /// Receipt activity survives acknowledgements and restart. Global notify
    /// and acknowledgements do not count as new work for this recipient.
    pub fn delivery_revision(&self, recipient: &str) -> io::Result<u64> {
        if self.uncertain.load(Ordering::Acquire) {
            return Err(io::Error::other("message ledger requires recovery"));
        }
        Ok(*self
            .ledger
            .lock()
            .map_err(|_| io::Error::other("mailbox lock poisoned"))?
            .deliveries
            .get(recipient)
            .unwrap_or(&0))
    }
    pub fn needs_completion(&self, child: &str, sequence: u64) -> io::Result<bool> {
        if self.uncertain.load(Ordering::Acquire) {
            return Ok(true);
        }
        Ok(self
            .ledger
            .lock()
            .map_err(|_| io::Error::other("mailbox lock poisoned"))?
            .completions
            .get(child)
            .is_none_or(|previous| *previous < sequence))
    }
    pub fn acknowledge(&self, recipient: &str, ids: &[String]) -> io::Result<()> {
        self.update(|ledger| {
            if let Some(inbox) = ledger.inboxes.get_mut(recipient) {
                inbox.retain(|message| !ids.contains(&message.id));
            }
            Ok(())
        })
    }
    pub fn stop_by_user(&self, id: &str) -> io::Result<()> {
        self.update(|ledger| {
            ledger.user_stopped.insert(id.into());
            Ok(())
        })
    }
    pub fn was_stopped_by_user(&self, id: &str) -> bool {
        self.ledger
            .lock()
            .expect("mailbox lock poisoned")
            .user_stopped
            .contains(id)
    }
}

struct WriteFailure {
    error: io::Error,
    renamed: bool,
}

fn write_durable(path: &Path, value: &Ledger) -> Result<(), WriteFailure> {
    let temporary = path.with_extension("json.tmp");
    let mut renamed = false;
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(value).map_err(io::Error::other)?)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        renamed = true;
        #[cfg(unix)]
        fs::File::open(
            path.parent()
                .ok_or_else(|| io::Error::other("mailbox has no parent directory"))?,
        )?
        .sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result.map_err(|error| WriteFailure { error, renamed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    static SERIAL: AtomicU64 = AtomicU64::new(0);

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "openharness-mailbox-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> PathBuf {
            self.0.join("collaboration.json")
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reopening_preserves_pending_messages_acknowledgements_and_user_stops() {
        let directory = Directory::new();
        let mailboxes = Mailboxes::open(directory.path()).unwrap();
        let first = mailboxes
            .send("parent", "child-a", "first evidence".into())
            .unwrap();
        let second = mailboxes
            .send("parent", "child-b", "second evidence".into())
            .unwrap();
        mailboxes
            .send("peer", "parent", "separate inbox".into())
            .unwrap();
        mailboxes.stop_by_user("child-a").unwrap();
        drop(mailboxes);

        let reopened = Mailboxes::open(directory.path()).unwrap();
        assert_eq!(
            reopened
                .pending("parent")
                .unwrap()
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec![first.as_str(), second.as_str()]
        );
        assert!(reopened.was_stopped_by_user("child-a"));
        reopened.acknowledge("parent", &[first]).unwrap();
        drop(reopened);

        let acknowledged = Mailboxes::open(directory.path()).unwrap();
        let pending = acknowledged.pending("parent").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, second);
        assert_eq!(
            acknowledged.pending("peer").unwrap()[0].text,
            "separate inbox"
        );
        let next = acknowledged
            .send("parent", "child-b", "new evidence".into())
            .unwrap();
        assert_ne!(next, second);
    }

    #[test]
    fn completion_bypasses_full_inbox_and_deduplicates_across_ack_and_reopen() {
        let directory = Directory::new();
        let mailboxes = Mailboxes::open(directory.path()).unwrap();
        for i in 0..128 {
            mailboxes
                .send("parent", "child", format!("evidence {i}"))
                .unwrap();
        }
        assert!(
            mailboxes
                .send("parent", "child", "overflow".into())
                .is_err()
        );
        mailboxes
            .completion("parent", "child", 7, "completed".into())
            .unwrap();
        mailboxes
            .completion("parent", "child", 7, "duplicate".into())
            .unwrap();
        let persisted: Ledger =
            serde_json::from_slice(&fs::read(directory.path()).unwrap()).unwrap();
        assert_eq!(persisted.inboxes["parent"].len(), 129);
        assert_eq!(
            persisted.inboxes["parent"].last().unwrap().text,
            "completed"
        );
        while !mailboxes.pending("parent").unwrap().is_empty() {
            let ids = mailboxes
                .pending("parent")
                .unwrap()
                .into_iter()
                .map(|m| m.id)
                .collect::<Vec<_>>();
            mailboxes.acknowledge("parent", &ids).unwrap();
        }
        drop(mailboxes);

        let reopened = Mailboxes::open(directory.path()).unwrap();
        reopened
            .completion("parent", "child", 7, "duplicate after ack".into())
            .unwrap();
        reopened
            .completion("parent", "child", 6, "older outcome".into())
            .unwrap();
        assert!(reopened.pending("parent").unwrap().is_empty());
        reopened
            .completion("parent", "child", 8, "follow-up completed".into())
            .unwrap();
        let pending = reopened.pending("parent").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "agent-result-child-8");
    }

    #[test]
    fn failed_first_write_can_retry_after_storage_is_repaired() {
        let directory = Directory::new();
        let mailboxes = Mailboxes::open(directory.path()).unwrap();
        let temporary = directory.path().with_extension("json.tmp");
        fs::create_dir(&temporary).unwrap();
        assert!(
            mailboxes
                .send("parent", "child", "not accepted".into())
                .is_err()
        );
        assert!(!directory.path().exists());
        assert!(mailboxes.pending("parent").unwrap().is_empty());
        assert!(!mailboxes.uncertain.load(Ordering::Acquire));

        fs::remove_dir(&temporary).unwrap();
        let accepted = mailboxes
            .send("parent", "child", "accepted after repair".into())
            .unwrap();
        assert_eq!(accepted, "agent-message-1");
        let reopened = Mailboxes::open(directory.path()).unwrap();
        assert_eq!(
            reopened.pending("parent").unwrap()[0].text,
            "accepted after repair"
        );
    }

    #[test]
    fn failed_write_preserves_prior_accepted_records_and_next_id() {
        let directory = Directory::new();
        let mailboxes = Mailboxes::open(directory.path()).unwrap();
        let accepted = mailboxes
            .send("parent", "child", "accepted".into())
            .unwrap();
        let temporary = directory.path().with_extension("json.tmp");
        fs::create_dir(&temporary).unwrap();
        assert!(
            mailboxes
                .acknowledge("parent", std::slice::from_ref(&accepted))
                .is_err()
        );
        assert_eq!(mailboxes.pending("parent").unwrap()[0].id, accepted);
        assert_eq!(
            Mailboxes::open(directory.path())
                .unwrap()
                .pending("parent")
                .unwrap()[0]
                .id,
            accepted
        );
        fs::remove_dir(&temporary).unwrap();
        let next = mailboxes.send("parent", "child", "next".into()).unwrap();
        assert_eq!(next, "agent-message-2");
        assert_eq!(mailboxes.pending("parent").unwrap().len(), 2);
    }

    #[test]
    fn uncertain_published_write_recovers_disk_generation_without_reusing_ids() {
        let directory = Directory::new();
        let mailboxes = Mailboxes::open(directory.path()).unwrap();
        let first = mailboxes.send("parent", "child", "first".into()).unwrap();
        let stale = mailboxes.ledger.lock().unwrap().clone();
        let second = mailboxes
            .send("parent", "child", "published before sync error".into())
            .unwrap();
        // Model the post-rename recovery boundary: memory must not be used
        // until the generation already published on disk has been reloaded.
        *mailboxes.ledger.lock().unwrap() = stale;
        mailboxes.uncertain.store(true, Ordering::Release);
        assert!(mailboxes.pending("parent").is_err());
        let third = mailboxes
            .send("parent", "child", "after recovery".into())
            .unwrap();
        assert_eq!(third, "agent-message-3");
        assert_eq!(
            mailboxes
                .pending("parent")
                .unwrap()
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec![first.as_str(), second.as_str(), third.as_str()]
        );
    }
}
