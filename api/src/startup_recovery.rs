//! Isolate damaged conversations without rewriting or discarding their evidence.
//! The service must still fail when its storage directories cannot be inventoried.
use super::*;
use agent_runtime::EventStore;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStage {
    Snapshot,
    EventLog,
    History,
    Dependency,
    Runtime,
    Settlement,
}

/// Read-only startup diagnostics. Affected runs are not admitted for execution;
/// their original files remain in place for inspection and a later recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryIssue {
    pub run_id: Option<String>,
    pub source_path: String,
    pub stage: RecoveryStage,
    pub message: String,
    pub related_run_id: Option<String>,
}

struct SavedRun {
    path: PathBuf,
    detail: RunDetail,
}

impl AppState {
    pub fn recovery_issues(&self) -> Vec<RecoveryIssue> {
        self.inner
            .recovery_issues
            .read()
            .expect("recovery issues lock poisoned")
            .clone()
    }

    fn recovery_issue(
        &self,
        path: &Path,
        run_id: Option<&str>,
        stage: RecoveryStage,
        message: &str,
        related_run_id: Option<&str>,
    ) {
        // Never echo arbitrary strings from a damaged snapshot. Related IDs
        // must also name an inventoried snapshot file, rather than just look
        // syntactically valid (credentials can look like IDs too).
        let related_run_id = related_run_id.filter(|id| {
            valid_id(id)
                && self
                    .inner
                    .directory
                    .join("runs")
                    .join(format!("{id}.json"))
                    .is_file()
        });
        // Raw parser/runtime errors may include malformed field values or
        // private model configuration. Log only the same controlled diagnostic
        // exposed by the UI, with its source path and recovery stage.
        tracing::warn!(?run_id, ?stage, path = %path.display(), message, "conversation recovery skipped");
        self.inner
            .recovery_issues
            .write()
            .expect("recovery issues lock poisoned")
            .push(RecoveryIssue {
                run_id: run_id.map(str::to_owned),
                source_path: path.display().to_string(),
                stage,
                message: message.into(),
                related_run_id: related_run_id.map(str::to_owned),
            });
    }

    pub(super) async fn recover_runs(&self) -> io::Result<()> {
        // A missing journal directory is diagnosed per run (or is a fresh
        // install). An inaccessible/invalid directory is a global storage error.
        match fs::read_dir(self.inner.directory.join("events")) {
            Ok(entries) => {
                for entry in entries {
                    entry?;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut paths = fs::read_dir(self.inner.directory.join("runs"))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()?;
        paths.sort();
        let mut pending = Vec::new();
        for path in paths {
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let file_id = path
                .file_stem()
                .and_then(|name| name.to_str())
                .filter(|id| valid_id(id));
            let bytes = match fs::symlink_metadata(&path).and_then(|metadata| {
                if metadata.file_type().is_file() {
                    fs::read(&path)
                } else {
                    Err(io::Error::other("run snapshot is not a regular file"))
                }
            }) {
                Ok(bytes) => bytes,
                Err(_) => {
                    self.recovery_issue(
                        &path,
                        file_id,
                        RecoveryStage::Snapshot,
                        "无法读取会话记录；已保留原文件，其他会话仍可使用。",
                        None,
                    );
                    continue;
                }
            };
            let detail = match serde_json::from_slice::<RunDetail>(&bytes) {
                Ok(detail) if Some(detail.summary.id.as_str()) == file_id => detail,
                _ => {
                    self.recovery_issue(
                        &path,
                        file_id,
                        RecoveryStage::Snapshot,
                        "会话记录格式或标识不正确；已保留原文件，未恢复此会话。",
                        None,
                    );
                    continue;
                }
            };
            pending.push(SavedRun { path, detail });
        }

        // Restore parents before children, not merely roots before all children.
        // A missing/broken intermediate parent cannot turn its descendants into
        // independently authorized tasks. Repeating passes also detects cycles.
        while !pending.is_empty() {
            let mut deferred = Vec::new();
            let mut progressed = false;
            for saved in pending {
                if let Some(delegation) = &saved.detail.summary.delegation {
                    let root = self.entry(&delegation.root_run_id).ok();
                    let parent = self.entry(&delegation.parent_run_id).ok();
                    let (Some(root), Some(parent)) = (root, parent) else {
                        deferred.push(saved);
                        continue;
                    };
                    let root = root.snapshot.get().summary;
                    let parent = parent.snapshot.get().summary;
                    let parent_root = parent
                        .delegation
                        .as_ref()
                        .map_or(parent.id.as_str(), |owner| owner.root_run_id.as_str());
                    if root.delegation.is_some() || parent_root != root.id {
                        self.recovery_issue(
                            &saved.path,
                            Some(&saved.detail.summary.id),
                            RecoveryStage::Dependency,
                            "协作任务的父任务与权限归属不一致；未恢复此任务，原始记录已保留。",
                            Some(&delegation.parent_run_id),
                        );
                        progressed = true;
                        continue;
                    }
                }
                self.recover_run(saved).await;
                progressed = true;
            }
            if !progressed {
                for saved in deferred {
                    let delegation = saved
                        .detail
                        .summary
                        .delegation
                        .as_ref()
                        .expect("only children can wait for a dependency");
                    let dependency = if self.entry(&delegation.root_run_id).is_err() {
                        &delegation.root_run_id
                    } else {
                        &delegation.parent_run_id
                    };
                    self.recovery_issue(&saved.path, Some(&saved.detail.summary.id),
                        RecoveryStage::Dependency,
                        "父任务或权限所属会话未能恢复；为保持原有权限边界，此协作任务暂不可用。原始记录已保留。",
                        Some(dependency));
                }
                break;
            }
            pending = deferred;
        }
        Ok(())
    }

    async fn recover_run(&self, saved: SavedRun) {
        let SavedRun { path, mut detail } = saved;
        let id = detail.summary.id.clone();
        let store = JsonlEventStore::new(self.inner.directory.join("events"));
        let stored = match store.load_for_runtime(&id).await {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                self.recovery_issue(
                    &path,
                    Some(&id),
                    RecoveryStage::EventLog,
                    "缺少此会话的原始事件日志；未从不完整记录恢复执行，现有文件已保留。",
                    None,
                );
                return;
            }
            Err(_) => {
                self.recovery_issue(
                    &path,
                    Some(&id),
                    RecoveryStage::EventLog,
                    "会话事件日志无法读取或校验；未恢复此会话，原始日志已保留。",
                    None,
                );
                return;
            }
        };
        let run = match AgentRun::restore(stored.run_id, stored.agent_id, stored.events) {
            Ok(run) => run,
            Err(_) => {
                self.recovery_issue(
                    &path,
                    Some(&id),
                    RecoveryStage::History,
                    "会话历史的状态或顺序不一致；未恢复此会话，原始日志已保留。",
                    None,
                );
                return;
            }
        };
        for input in &mut detail.pending_inputs {
            input.delivering = false;
            input.steer = false;
        }
        detail.messages.clear();
        detail.events.clear();
        detail.error = None;
        detail.summary.compacting = false;
        for event in run.events() {
            projection::apply(&mut detail, event);
        }
        // Restart never resumes work or delivers queued steering automatically.
        if !detail.pending_inputs.is_empty() {
            detail.pending_inputs_paused = true;
        }
        if run.status() == CoreStatus::Running {
            detail.summary.status = RunStatus::Interrupted;
            detail.error =
                Some("运行已中断，历史已保存。继续时会先核实未完成操作，不会自动重放命令。".into());
        }
        detail.summary.revision = detail.summary.revision.saturating_add(1);
        let entry = match self.make_entry(detail, run.current_step()) {
            Ok(entry) => entry,
            Err(_) => {
                self.recovery_issue(
                    &path,
                    Some(&id),
                    RecoveryStage::Runtime,
                    "无法初始化此会话的运行环境或上下文记录；未恢复执行，原始日志已保留。",
                    None,
                );
                return;
            }
        };
        // Completion delivery can fail independently (for example, its durable
        // mailbox cannot be written). Do not expose this task, or its children,
        // as recovered until cleanup and completion reconciliation have settled.
        if self.settle_child(&entry).await.is_err() {
            self.recovery_issue(
                &path,
                Some(&id),
                RecoveryStage::Settlement,
                "协作任务的停止状态或结果同步未能恢复；此任务暂不可用，原始记录已保留。",
                None,
            );
            return;
        }
        self.inner
            .runs
            .write()
            .expect("runs lock poisoned")
            .insert(id, entry);
    }
}

#[cfg(test)]
#[path = "startup_recovery_tests.rs"]
mod tests;
