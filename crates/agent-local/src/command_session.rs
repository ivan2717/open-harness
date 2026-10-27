//! Bounded, conversation-owned command sessions. The registry owns cancellation
//! senders, while workers hold only a Weak<Inner>, so dropping a host cancels its
//! processes rather than keeping the host alive through a reference cycle.
use crate::{CommandRecord, CommandStatus, lock_error, output::CommandOutput, paths};
use std::{
    collections::{BTreeMap, VecDeque},
    io,
    sync::{Arc, Mutex},
};
use tokio::sync::{Notify, watch};

const LIVE_OUTPUT_LIMIT: usize = 256 * 1024;
const SESSION_LIMIT: usize = 50;

#[derive(Default)]
pub(crate) struct CommandSessions(Mutex<BTreeMap<String, Arc<CommandSession>>>);

impl CommandSessions {
    pub fn insert(&self, id: String, session: Arc<CommandSession>) -> io::Result<()> {
        let mut sessions = self.0.lock().map_err(lock_error)?;
        if sessions.len() >= SESSION_LIMIT {
            let obsolete = sessions
                .iter()
                .find_map(|(id, session)| (!session.active()).then(|| id.clone()));
            if let Some(id) = obsolete {
                sessions.remove(&id);
            }
        }
        if sessions.len() >= SESSION_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "too many command sessions",
            ));
        }
        sessions.insert(id, session);
        Ok(())
    }

    pub fn get(&self, id: &str) -> io::Result<Arc<CommandSession>> {
        self.0.lock().map_err(lock_error)?.get(id).cloned().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "command session not found in this conversation; use workspace review for older records")
        })
    }

    pub fn is_active(&self) -> bool {
        self.0
            .lock()
            .is_ok_and(|sessions| sessions.values().any(|session| session.active()))
    }

    pub fn active_sessions(&self) -> Vec<Arc<CommandSession>> {
        self.0
            .lock()
            .map(|sessions| {
                sessions
                    .values()
                    .filter(|session| session.active())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn cancel_all(&self) {
        if let Ok(sessions) = self.0.lock() {
            for session in sessions.values() {
                session.cancel();
            }
        }
    }

    pub fn records(&self, records: &[CommandRecord]) -> Vec<CommandRecord> {
        let Ok(sessions) = self.0.lock() else {
            return records.to_vec();
        };
        records
            .iter()
            .map(|record| {
                let mut record = record.clone();
                if let Some(session) = sessions.get(&record.id)
                    && let Ok(state) = session.state.lock()
                {
                    record.status = state.status.clone();
                    record.exit_code = state.exit_code;
                    record.output = state.archive.text();
                }
                record
            })
            .collect()
    }
}
impl Drop for CommandSessions {
    fn drop(&mut self) {
        self.cancel_all();
    }
}

pub(crate) struct CommandSession {
    pub state: Mutex<SessionState>,
    pub changed: Notify,
    cancellation: watch::Sender<bool>,
}
pub(crate) struct SessionState {
    pub status: String,
    pub exit_code: Option<i32>,
    pub archive: CommandOutput,
    live: VecDeque<u8>,
    total: u64,
}
impl CommandSession {
    pub fn new() -> Self {
        let (cancellation, _) = watch::channel(false);
        Self {
            state: Mutex::new(SessionState {
                status: "awaiting_approval".into(),
                exit_code: None,
                archive: CommandOutput::default(),
                live: VecDeque::new(),
                total: 0,
            }),
            changed: Notify::new(),
            cancellation,
        }
    }
    pub fn active(&self) -> bool {
        self.state
            .lock()
            .is_ok_and(|state| matches!(state.status.as_str(), "running" | "awaiting_approval"))
    }
    pub fn cancel(&self) {
        self.cancellation.send_replace(true);
    }
    pub fn cancellation(&self) -> watch::Receiver<bool> {
        self.cancellation.subscribe()
    }
    pub fn append(&self, bytes: &[u8]) -> io::Result<()> {
        let mut state = self.state.lock().map_err(lock_error)?;
        state.archive.extend_from_slice(bytes);
        state.total = state.total.saturating_add(bytes.len() as u64);
        let keep = bytes.len().min(LIVE_OUTPUT_LIMIT);
        let remove = (state.live.len() + keep).saturating_sub(LIVE_OUTPUT_LIMIT);
        state.live.drain(..remove);
        state.live.extend(&bytes[bytes.len() - keep..]);
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn set_status(&self, status: &str, code: Option<i32>) -> io::Result<()> {
        let mut state = self.state.lock().map_err(lock_error)?;
        state.status = status.into();
        state.exit_code = code;
        drop(state);
        self.changed.notify_waiters();
        Ok(())
    }
    pub fn snapshot(&self, id: &str, cursor: u64) -> io::Result<CommandStatus> {
        let state = self.state.lock().map_err(lock_error)?;
        if cursor > state.total {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "output cursor is beyond this command's output",
            ));
        }
        let oldest = state.total.saturating_sub(state.live.len() as u64);
        let start = cursor.max(oldest);
        let skip = (start - oldest) as usize;
        let bytes: Vec<_> = state
            .live
            .iter()
            .skip(skip)
            .take(paths::OUTPUT_LIMIT)
            .copied()
            .collect();
        // Do not consume a partial UTF-8 code point at a polling boundary.
        let length = match std::str::from_utf8(&bytes) {
            Err(error)
                if error.error_len().is_none()
                    && (matches!(state.status.as_str(), "running" | "awaiting_approval")
                        || start + (bytes.len() as u64) < state.total) =>
            {
                error.valid_up_to()
            }
            _ => bytes.len(),
        };
        Ok(CommandStatus {
            command_id: id.into(),
            status: state.status.clone(),
            exit_code: state.exit_code,
            output: String::from_utf8_lossy(&bytes[..length]).into_owned(),
            next_cursor: start + length as u64,
            output_truncated: cursor < oldest,
            has_more_output: start + (length as u64) < state.total,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_cursors_are_independent_and_eviction_is_explicit() {
        let session = CommandSession::new();
        session.append(b"first").unwrap();
        let first = session.snapshot("id", 0).unwrap();
        assert_eq!(first.output, "first");
        assert_eq!(session.snapshot("id", 0).unwrap().output, "first");
        session.append(b"second").unwrap();
        assert_eq!(
            session.snapshot("id", first.next_cursor).unwrap().output,
            "second"
        );
        session
            .append(&vec![b'x'; LIVE_OUTPUT_LIMIT + 100])
            .unwrap();
        let evicted = session.snapshot("id", first.next_cursor).unwrap();
        assert!(evicted.output_truncated);
        assert_eq!(evicted.output.len(), paths::OUTPUT_LIMIT);
        assert!(evicted.has_more_output);
        assert!(session.snapshot("id", u64::MAX).is_err());
    }
    #[test]
    fn completed_output_remains_paginated_until_all_bytes_are_read() {
        let session = CommandSession::new();
        let expected = "你".repeat(34_000);
        session.append(expected.as_bytes()).unwrap();
        session.set_status("completed", Some(0)).unwrap();
        let first = session.snapshot("id", 0).unwrap();
        assert_eq!(first.status, "completed");
        assert!(first.has_more_output);
        assert!(!first.output_truncated);
        let second = session.snapshot("id", first.next_cursor).unwrap();
        assert!(!second.has_more_output);
        assert!(!second.output_truncated);
        assert_eq!(format!("{}{}", first.output, second.output), expected);
        assert_eq!(second.next_cursor, expected.len() as u64);
    }
    #[test]
    fn partial_utf8_is_not_consumed_until_the_next_chunk() {
        let session = CommandSession::new();
        session.append(&"你".as_bytes()[..2]).unwrap();
        let partial = session.snapshot("id", 0).unwrap();
        assert_eq!(partial.next_cursor, 0);
        session.append(&"你".as_bytes()[2..]).unwrap();
        assert_eq!(session.snapshot("id", 0).unwrap().output, "你");
        session.append(&[0xe4]).unwrap();
        session.set_status("completed", Some(0)).unwrap();
        let terminal = session.snapshot("id", 3).unwrap();
        assert_eq!(terminal.output, "�");
        assert_eq!(terminal.next_cursor, 4);
        assert!(!terminal.has_more_output);
    }
}
