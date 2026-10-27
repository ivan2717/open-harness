use crate::{
    LocalTools, PermissionMode,
    journal::{SavedFile, SavedPermissions},
    lock_error, paths,
};
use agent_runtime::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs, io,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) fn definitions(mode: PermissionMode) -> Vec<ToolDefinition> {
    let make = |name: &str, description: &str, properties: serde_json::Value, required: &[&str]| {
        ToolDefinition { name: name.into(), description: description.into(), input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}).to_string() }
    };
    let mut definitions = vec![
        make(
            "list_files",
            "List files in a workspace-relative directory. Protected paths and dependency/build directories are excluded.",
            json!({"path":{"type":"string"}}),
            &["path"],
        ),
        make(
            "read_file",
            "Read UTF-8, PDF, DOCX, XLSX or PPTX (up to 32 MiB). Returns bounded text and source locations; pass next_cursor unchanged to continue. No OCR or macro execution.",
            json!({"path":{"type":"string"},"cursor":{"type":"object","properties":{"unit":{"type":"integer","minimum":1},"offset":{"type":"integer","minimum":0}},"required":["unit","offset"],"additionalProperties":false}}),
            &["path"],
        ),
        make(
            "search_files",
            "Search case-sensitive literal text. A file path reads text or PDF/DOCX/XLSX/PPTX; directories scan bounded text only and report skipped documents. Returns source locations and next_offset for matching-line pagination.",
            json!({"query":{"type":"string"},"path":{"type":"string"},"offset":{"type":"integer","minimum":0}}),
            &["query", "path"],
        ),
        make(
            "apply_patch",
            "Apply journaled UTF-8 edits (256 KiB/file). Format: *** Begin Patch, *** Add File: path (+lines), *** Update File: path (@@ then space context/-old/+new lines), *** Delete File: path, *** End Patch. Each update hunk must match exactly once; read first. Preserves existing line endings and final newline. Validates all files before writing; supports safe undo.",
            json!({"patch":{"type":"string"}}),
            &["patch"],
        ),
        make(
            "run_command",
            if cfg!(windows) {
                "Execute PowerShell in a Windows AppContainer within the user-approved access scope. Writes are confined to the workspace; protected files are denied. Network is denied by default; request network_access=allowed only when the command needs Internet/LAN access. Windows loopback remains blocked. Isolation failure stops execution. Set access=read_only for tests/inspection: the project is OS read-only, only this command’s temporary directory ($TMPDIR on macOS, $env:TEMP on Windows) is writable and is removed after exit. Read-only commands can run concurrently and see live files, not a snapshot; rerun final checks after edits settle. Delegated agents can only use read_only. Workspace writes require access=workspace_write and run exclusively. Returns a command_id if still running; use manage_command to inspect or stop it. yield_time_ms is only the initial wait (default 10000); timeout_seconds is an optional total deadline. Output is bounded. Do not start detached daemons."
            } else {
                "Execute /bin/sh in the macOS sandbox within the user-approved access scope. macOS sandbox allows workspace writes and denies protected paths. Network is denied by default; request network_access=allowed only when necessary. The user approves access and network together, once or for subsequent commands in this conversation; file restrictions stay in place. Set access=read_only for tests/inspection: the project is OS read-only, only this command’s temporary directory ($TMPDIR on macOS, $env:TEMP on Windows) is writable and is removed after exit. Read-only commands can run concurrently and see live files, not a snapshot; rerun final checks after edits settle. Delegated agents can only use read_only. Workspace writes require access=workspace_write and run exclusively. Returns a command_id if still running; use manage_command to inspect or stop it. yield_time_ms is only the initial wait (default 10000); timeout_seconds is an optional total deadline. Output is bounded. Do not start detached daemons."
            },
            json!({"command":{"type":"string"},"access":{"type":"string","enum":["read_only","workspace_write"]},"network_access":{"type":"string","enum":["denied","allowed"],"default":"denied"},"yield_time_ms":{"type":"integer","minimum":0,"maximum":30000},"timeout_seconds":{"type":"integer","minimum":1,"maximum":86400}}),
            &["command"],
        ),
        make(
            "manage_command",
            "Read incremental output/status or stop one command from this conversation. Pass next_cursor as cursor to avoid duplicate output; keep reading while has_more_output, including after completion. wait_ms waits for new output or completion (max 30000), not a process deadline. Use wait_ms of at least 1000 when a command is quiet; avoid tight empty polling. Stopped/interrupted commands are never automatically replayed. Only workspace-write commands keep the workspace busy; read-only commands can run concurrently.",
            json!({"command_id":{"type":"string"},"action":{"type":"string","enum":["output","status","stop"]},"cursor":{"type":"integer","minimum":0},"wait_ms":{"type":"integer","minimum":0,"maximum":30000}}),
            &["command_id"],
        ),
        make(
            "load_workspace_dependencies",
            "Report the host-prepared Node/Python interpreters, exact arguments and verified libraries without installing anything. Use these paths for local scripts; an unavailable interpreter is null. Does not grant execution permission or download packages.",
            json!({}),
            &[],
        ),
    ];
    definitions.retain(|tool| match tool.name.as_str() {
        "apply_patch" => mode == PermissionMode::WorkspaceWrite,
        "run_command" | "manage_command" | "load_workspace_dependencies" => {
            crate::command::execution_available()
        }
        _ => true,
    });
    definitions
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathInput {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    path: String,
    content: String,
    #[serde(deserialize_with = "required_nullable")]
    expected_content: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditInput {
    path: String,
    old_text: String,
    new_text: String,
}

impl LocalTools {
    pub(crate) fn file_call(&self, call: ToolCall) -> io::Result<String> {
        let output = match call.name.as_str() {
            "list_files" => {
                let input: PathInput = parse(&call.arguments)?;
                let target = paths::resolve(self.root(), &input.path)?;
                let mut entries = Vec::new();
                for entry in fs::read_dir(target)? {
                    let entry = entry?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let kind = entry.file_type()?;
                    if paths::skipped_directory(&name) || kind.is_symlink() {
                        continue;
                    }
                    let metadata = entry.metadata()?;
                    let reader = if kind.is_dir() {
                        None
                    } else {
                        Some("read_file")
                    };
                    entries.push(json!({"name":name,"directory":kind.is_dir(),"size_bytes":metadata.len(),"reader":reader}));
                    if entries.len() > 1000 {
                        return Err(io::Error::other(
                            "directory has too many entries; choose a subdirectory",
                        ));
                    }
                }
                entries.sort_by_key(|entry| entry["name"].as_str().unwrap_or_default().to_owned());
                json!({"entries":entries}).to_string()
            }
            "read_file" => crate::reader::read(self.root(), &call.arguments)?,
            "search_files" => crate::reader::search(self.root(), &call.arguments)?,
            "load_workspace_dependencies" => {
                crate::workspace_runtime::describe(self.root(), &call.arguments)?
            }
            "apply_patch" => self.apply_patch(&call.arguments)?,
            "read_document" | "search_document" => crate::documents::call(self.root(), &call)?,

            "write_file" => {
                let input: WriteInput = parse(&call.arguments)?;
                self.write(&input.path, input.expected_content, input.content)?;
                json!({"path":input.path,"written":true}).to_string()
            }
            "edit_file" => {
                let input: EditInput = parse(&call.arguments)?;
                let target = paths::resolve(self.root(), &input.path)?;
                let before = paths::text_file(&target)?.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "file does not exist")
                })?;
                if input.old_text.is_empty() || before.matches(&input.old_text).count() != 1 {
                    return Err(io::Error::other("old_text must match exactly once"));
                }
                let after = before.replacen(&input.old_text, &input.new_text, 1);
                self.write(&input.path, Some(before), after)?;
                json!({"path":input.path,"edited":true}).to_string()
            }
            _ => return Err(io::Error::other("unknown local tool")),
        };
        if output.len() > paths::OUTPUT_LIMIT {
            return Err(io::Error::other(
                "tool result too large; narrow the request",
            ));
        }
        Ok(output)
    }

    fn write(&self, path: &str, expected: Option<String>, content: String) -> io::Result<()> {
        if self.inner.mode != PermissionMode::WorkspaceWrite {
            return Err(paths::denied("workspace is read-only"));
        }
        if content.len() > paths::FILE_LIMIT || content.contains('\0') {
            return Err(io::Error::other(
                "file content exceeds 256 KiB or contains NUL",
            ));
        }
        let target = self.authorize_write(path)?;
        let path = target
            .strip_prefix(self.root())
            .unwrap()
            .to_str()
            .ok_or_else(|| paths::denied("path must be UTF-8"))?;
        let before = paths::text_file(&target)?;
        if before != expected {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "file changed; read current contents before editing",
            ));
        }
        let parent = target
            .parent()
            .ok_or_else(|| paths::denied("cannot replace the workspace"))?;
        fs::create_dir_all(parent)?;
        let mut journal = self.inner.journal.lock().map_err(lock_error)?;
        // Persist intent before touching the workspace. After a crash, review
        // compares the actual file to this expected result before allowing undo.
        let prior = journal.files.get(path).cloned();
        let saved = SavedFile {
            before_permissions: prior
                .as_ref()
                .and_then(|prior| prior.before_permissions.clone())
                .or(file_permissions(&target)?),
            before: prior
                .as_ref()
                .map_or(before.clone(), |saved| saved.before.clone()),
            after: Some(content.clone()),
            owned: prior
                .as_ref()
                .is_none_or(|saved| saved.owned && saved.after == before),
        };
        journal.files.insert(path.to_owned(), saved);
        if let Err(error) = journal.save(&self.inner.state_dir) {
            if let Some(prior) = prior {
                journal.files.insert(path.into(), prior);
            } else {
                journal.files.remove(path);
            }
            return Err(error);
        }
        replace_file(&target, Some(&content))
    }
}

pub(crate) fn parse<T: serde::de::DeserializeOwned>(arguments: &str) -> io::Result<T> {
    if arguments.len() > 1024 * 1024 {
        return Err(io::Error::other("tool arguments exceed 1 MiB"));
    }
    serde_json::from_str(arguments).map_err(io::Error::other)
}

fn required_nullable<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer)
}

/// Rename a fresh regular file instead of truncating an existing inode. This
/// preserves other hard-link aliases and makes each saved edit atomic.
pub(crate) fn replace_file(target: &Path, content: Option<&str>) -> io::Result<()> {
    replace_with_permissions(target, content, None)
}

pub(crate) fn replace_with_permissions(
    target: &Path,
    content: Option<&str>,
    original: Option<&SavedPermissions>,
) -> io::Result<()> {
    let Some(content) = content else {
        fs::remove_file(target)?;
        #[cfg(unix)]
        fs::File::open(
            target
                .parent()
                .ok_or_else(|| paths::denied("cannot remove root"))?,
        )?
        .sync_all()?;
        return Ok(());
    };
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let parent = target
        .parent()
        .ok_or_else(|| paths::denied("cannot replace root"))?;
    let temporary = parent.join(format!(
        ".openharness-edit-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    use std::io::Write;
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        if let Ok(metadata) = fs::metadata(target) {
            file.set_permissions(metadata.permissions())?;
        } else if let Some(original) = original {
            #[cfg(unix)]
            if let Some(mode) = original.unix_mode {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(fs::Permissions::from_mode(mode))?;
            }
            #[cfg(not(unix))]
            {
                let mut permissions = file.metadata()?.permissions();
                permissions.set_readonly(original.readonly);
                file.set_permissions(permissions)?;
            }
        }
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, target)?;
        #[cfg(unix)]
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Store permissions before deletion so rollback and restart-safe undo can
/// recreate a script or private file without losing its original mode.
pub(crate) fn file_permissions(path: &Path) -> io::Result<Option<SavedPermissions>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let permissions = metadata.permissions();
            #[cfg(unix)]
            let unix_mode = {
                use std::os::unix::fs::PermissionsExt;
                Some(permissions.mode())
            };
            #[cfg(not(unix))]
            let unix_mode = None;
            Ok(Some(SavedPermissions {
                readonly: permissions.readonly(),
                unix_mode,
            }))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
