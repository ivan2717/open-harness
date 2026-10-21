use crate::{
    context::{ObservedContext, initial_context},
    contracts::{AppConfig, CreateRunInput, ModelInfo, RunDetail, RunStatus, RunSummary},
    demo::{DemoModel, DemoTools, MODEL_ID, WINDOW_TOKENS},
    event_store::ObservedEventStore,
    projection,
    snapshot::Snapshot,
    time,
};
use agent_core::agent::{Agent, AgentRun, RunStatus as CoreStatus};
use agent_runtime::{AgentRuntime, JsonlEventStore};
use std::{
    collections::HashMap,
    fs, io,
    path::PathBuf,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

type Runtime = AgentRuntime<DemoModel, DemoTools, ObservedContext, ObservedEventStore>;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;

#[derive(Clone)]
pub struct AppState {
    inner: Arc<Service>,
}
struct Service {
    directory: PathBuf,
    token: Option<String>,
    runs: RwLock<HashMap<String, Arc<RunEntry>>>,
    delay: Duration,
}
struct RunEntry {
    runtime: Arc<Runtime>,
    snapshot: Snapshot,
    operations: Mutex<()>,
}

impl AppState {
    pub async fn open(directory: PathBuf, token: Option<String>) -> io::Result<Self> {
        Self::open_with_delay(directory, token, Duration::from_millis(65)).await
    }
    #[doc(hidden)]
    pub async fn open_with_delay(
        directory: PathBuf,
        token: Option<String>,
        delay: Duration,
    ) -> io::Result<Self> {
        fs::create_dir_all(directory.join("runs"))?;
        fs::create_dir_all(directory.join("checkpoints"))?;
        let directory = fs::canonicalize(directory)?;
        let state = Self {
            inner: Arc::new(Service {
                directory,
                token: token.filter(|token| !token.is_empty()),
                runs: RwLock::new(HashMap::new()),
                delay,
            }),
        };
        for entry in fs::read_dir(state.inner.directory.join("runs"))? {
            let path = entry?.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let mut detail: RunDetail =
                serde_json::from_slice(&fs::read(&path)?).map_err(io::Error::other)?;
            if !valid_id(&detail.summary.id) {
                return Err(io::Error::other("invalid persisted run identifier"));
            }
            let store = JsonlEventStore::new(state.inner.directory.join("events"));
            use agent_runtime::EventStore;
            let Some(stored) = store
                .load(&detail.summary.id)
                .await
                .map_err(io::Error::other)?
            else {
                continue;
            };
            let run = AgentRun::restore(stored.run_id, stored.agent_id, stored.events)
                .map_err(io::Error::other)?;
            detail.messages.clear();
            detail.events.clear();
            detail.error = None;
            for event in run.events() {
                projection::apply(&mut detail, event);
            }
            if run.status() == CoreStatus::Running {
                detail.summary.status = RunStatus::Interrupted;
                detail.error =
                    Some("后端在运行期间退出。已恢复提交的历史；请停止此运行或新建会话。".into());
            }
            detail.summary.revision = detail.summary.revision.saturating_add(1);
            let entry = state.make_entry(detail, run.current_step())?;
            state
                .inner
                .runs
                .write()
                .expect("runs lock poisoned")
                .insert(run.id().into(), entry);
        }
        Ok(state)
    }
    pub fn token(&self) -> Option<&str> {
        self.inner.token.as_deref()
    }
    pub fn config(&self) -> AppConfig {
        AppConfig { version: env!("CARGO_PKG_VERSION").into(), models: vec![ModelInfo { id: MODEL_ID.into(), name: "Local demo · 离线演示".into(), description: "确定性本地脚本，使用真实运行循环、工具与压缩管理器。8K 演示窗口用于快速观察压缩；不是外部模型，也不会产生 API 费用。提示以 /ask 开头可演示补充输入。".into(), available: true, context_window: WINDOW_TOKENS }], default_model: MODEL_ID.into(), storage_path: self.inner.directory.display().to_string() }
    }
    pub fn list(&self) -> Vec<RunSummary> {
        let mut runs = self
            .inner
            .runs
            .read()
            .expect("runs lock poisoned")
            .values()
            .map(|entry| entry.snapshot.get().summary)
            .collect::<Vec<_>>();
        runs.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        runs
    }
    pub fn get(&self, id: &str) -> Result<RunDetail, ServiceError> {
        Ok(self.entry(id)?.snapshot.get())
    }
    pub fn subscribe(
        &self,
        id: &str,
    ) -> Result<tokio::sync::watch::Receiver<RunDetail>, ServiceError> {
        Ok(self.entry(id)?.snapshot.subscribe())
    }
    fn entry(&self, id: &str) -> Result<Arc<RunEntry>, ServiceError> {
        if !valid_id(id) {
            return Err(ServiceError::NotFound);
        }
        self.inner
            .runs
            .read()
            .expect("runs lock poisoned")
            .get(id)
            .cloned()
            .ok_or(ServiceError::NotFound)
    }
    fn make_entry(&self, detail: RunDetail, round: u32) -> io::Result<Arc<RunEntry>> {
        let id = detail.summary.id.clone();
        let ask = detail.summary.prompt.trim_start().starts_with("/ask");
        let snapshot = Snapshot::new(
            detail,
            self.inner.directory.join("runs").join(format!("{id}.json")),
        )?;
        let context = ObservedContext::new(
            self.inner
                .directory
                .join("checkpoints")
                .join(format!("{id}.json")),
            snapshot.clone(),
        )?;
        let runtime = AgentRuntime::with_event_store(
            DemoModel::new(round, ask, self.inner.delay),
            DemoTools::new(self.inner.delay.saturating_mul(3)),
            context,
            ObservedEventStore {
                inner: JsonlEventStore::new(self.inner.directory.join("events")),
                snapshot: snapshot.clone(),
            },
            Some(12),
        );
        Ok(Arc::new(RunEntry {
            runtime: Arc::new(runtime),
            snapshot,
            operations: Mutex::new(()),
        }))
    }
    pub async fn create(&self, input: CreateRunInput) -> Result<RunDetail, ServiceError> {
        if input.model_id != MODEL_ID {
            return Err(ServiceError::BadRequest(
                "仅支持已配置的 local-demo 模型".into(),
            ));
        }
        validate_message(&input.prompt)?;
        let id = format!(
            "run-{:x}-{:x}-{:x}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let now = time::now();
        let detail = RunDetail {
            summary: RunSummary {
                id: id.clone(),
                revision: 0,
                title: input.prompt.trim().chars().take(64).collect(),
                prompt: input.prompt.clone(),
                status: RunStatus::Running,
                model_id: input.model_id,
                created_at: now.clone(),
                updated_at: now,
            },
            messages: Vec::new(),
            events: Vec::new(),
            context: Some(initial_context()),
            error: None,
        };
        let entry = self.make_entry(detail, 0)?;
        let run = entry
            .runtime
            .start_run(&agent(), &id, input.prompt)
            .await
            .map_err(ServiceError::internal)?;
        entry.snapshot.event(&run.events()[0])?;
        let snapshot = entry.snapshot.get();
        self.inner
            .runs
            .write()
            .expect("runs lock poisoned")
            .insert(id, entry.clone());
        execute(entry, run);
        Ok(snapshot)
    }
    pub async fn cancel(&self, id: &str) -> Result<RunDetail, ServiceError> {
        let entry = self.entry(id)?;
        let _operation = entry.operations.lock().await;
        let run = entry
            .runtime
            .restore_run(id)
            .await
            .map_err(ServiceError::internal)?
            .ok_or(ServiceError::NotFound)?;
        cancel_restored(&entry, run).await
    }
    pub async fn input(&self, id: &str, message: String) -> Result<RunDetail, ServiceError> {
        validate_message(&message)?;
        let entry = self.entry(id)?;
        let _operation = entry.operations.lock().await;
        let mut run = entry
            .runtime
            .restore_run(id)
            .await
            .map_err(ServiceError::internal)?
            .ok_or(ServiceError::NotFound)?;
        if run.status() != CoreStatus::WaitingForInput {
            return Err(ServiceError::Conflict("此运行当前不在等待输入".into()));
        }
        // The waiting event is published just before the execution guard drops.
        // A fast UI may submit input in that small interval.
        while entry.runtime.is_executing(id) {
            tokio::task::yield_now().await;
        }
        entry
            .runtime
            .submit_input(&mut run, message)
            .await
            .map_err(ServiceError::internal)?;
        for event in run.events() {
            entry.snapshot.event(event)?;
        }
        let snapshot = entry.snapshot.get();
        execute(entry.clone(), run);
        Ok(snapshot)
    }
}

async fn cancel_restored(entry: &RunEntry, mut run: AgentRun) -> Result<RunDetail, ServiceError> {
    if !run.status().is_terminal()
        && let Err(error) = entry.runtime.cancel_run(&mut run).await
    {
        // Completion may win after the API reads Running but before the runtime
        // obtains its cancellation guard. Stopping an already terminal run is
        // successful; never replace its authoritative outcome with Cancelled.
        match entry.runtime.restore_run(run.id()).await {
            Ok(Some(restored)) if restored.status().is_terminal() => run = restored,
            _ => return Err(ServiceError::internal(error)),
        }
    }
    for event in run.events() {
        entry.snapshot.event(event)?;
    }
    Ok(entry.snapshot.get())
}

fn execute(entry: Arc<RunEntry>, mut run: AgentRun) {
    tokio::spawn(async move {
        let mut sink = entry.snapshot.clone();
        let agent = agent();
        let result = entry
            .runtime
            .run_until_blocked_with_stream(&agent, &mut run, &mut sink)
            .await;
        if let Ok(Some(restored)) = entry.runtime.restore_run(run.id()).await {
            for event in restored.events() {
                let _ = entry.snapshot.event(event);
            }
            if !restored.status().is_terminal()
                && restored.status() != CoreStatus::WaitingForInput
                && let Err(error) = result
            {
                let _ = entry.snapshot.update(|detail| {
                    detail.summary.status = RunStatus::Failed;
                    detail.error = Some(error.to_string());
                    detail
                        .messages
                        .retain(|message| message.status != "streaming");
                });
            }
        }
    });
}

fn agent() -> Agent {
    Agent::new(
        "desktop-local-demo",
        "Local demonstration",
        "Offline harness verification",
        "Run the deterministic local diagnostic demonstration. Preserve the original user prompt, complete tool results, and the summary of earlier diagnostic rounds.",
    )
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}
fn validate_message(message: &str) -> Result<(), ServiceError> {
    if message.trim().is_empty() || message.chars().count() > 4000 {
        return Err(ServiceError::BadRequest(
            "输入必须包含 1–4000 个字符".into(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
pub enum ServiceError {
    NotFound,
    BadRequest(String),
    Conflict(String),
    Internal(String),
}
impl ServiceError {
    fn internal(error: impl std::fmt::Display) -> Self {
        Self::Internal(error.to_string())
    }
}
impl From<io::Error> for ServiceError {
    fn from(error: io::Error) -> Self {
        Self::internal(error)
    }
}
impl axum::response::IntoResponse for ServiceError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::StatusCode;
        let (status, message) = match self {
            Self::NotFound => (StatusCode::NOT_FOUND, "Run not found".into()),
            Self::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            Self::Conflict(message) => (StatusCode::CONFLICT, message),
            Self::Internal(message) => {
                tracing::error!(%message, "run service error");
                (StatusCode::INTERNAL_SERVER_ERROR, message)
            }
        };
        (status, axum::Json(serde_json::json!({"error": message}))).into_response()
    }
}
