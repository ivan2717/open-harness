use agent_core::agent::RunEventEnvelope;
use tokio::sync::broadcast;

const DEFAULT_SUBSCRIPTION_CAPACITY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunEventNotification {
    pub run_id: String,
    pub agent_id: String,
    pub envelope: RunEventEnvelope,
}

#[derive(Debug)]
pub struct RunEventReceiver {
    receiver: broadcast::Receiver<RunEventNotification>,
    run_id: Option<String>,
}

impl RunEventReceiver {
    pub async fn recv(&mut self) -> Result<RunEventNotification, broadcast::error::RecvError> {
        loop {
            let notification = self.receiver.recv().await?;
            if self.accepts(&notification) {
                return Ok(notification);
            }
        }
    }

    pub fn try_recv(&mut self) -> Result<RunEventNotification, broadcast::error::TryRecvError> {
        loop {
            let notification = self.receiver.try_recv()?;
            if self.accepts(&notification) {
                return Ok(notification);
            }
        }
    }

    fn accepts(&self, notification: &RunEventNotification) -> bool {
        self.run_id
            .as_ref()
            .is_none_or(|run_id| run_id == &notification.run_id)
    }
}

/// A bounded live-event broadcaster.
///
/// Slow receivers get `RecvError::Lagged` and can recover missing events from
/// the configured `EventStore` by sequence number.
#[derive(Debug)]
pub struct RunEventSubscriptions {
    sender: broadcast::Sender<RunEventNotification>,
}

impl Default for RunEventSubscriptions {
    fn default() -> Self {
        Self::new(DEFAULT_SUBSCRIPTION_CAPACITY)
    }
}

impl RunEventSubscriptions {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    pub fn subscribe(&self) -> RunEventReceiver {
        RunEventReceiver {
            receiver: self.sender.subscribe(),
            run_id: None,
        }
    }

    pub(crate) fn subscribe_run(&self, run_id: impl Into<String>) -> RunEventReceiver {
        RunEventReceiver {
            receiver: self.sender.subscribe(),
            run_id: Some(run_id.into()),
        }
    }

    pub(crate) fn publish(&self, notification: RunEventNotification) {
        let _ = self.sender.send(notification);
    }
}
