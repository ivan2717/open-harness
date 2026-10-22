use crate::{LocalTools, PermissionMode, journal::SavedFile, lock_error, paths};
use agent_runtime::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs, io,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) fn definitions() -> Vec<ToolDefinition> {
    let make = |name: &str, description: &str, properties: serde_json::Value, required: &[&str]| {
        ToolDefinition { name: name.into(), description: description.into(), input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}).to_string() }
    };
    vec![
        make(
            "list_files",
            "List files in a workspace-relative directory. Protected paths and dependency/build directories are excluded.",
            json!({"path":{"type":"string"}}),
            &["path"],
        ),
        make(
            "read_file",
            "Read a UTF-8 file, returning up to 32 KiB and next_offset for pagination. Offsets count characters.",
            json!({"path":{"type":"string"},"offset":{"type":"integer","minimum":0}}),
            &["path"],
        ),
        make(
            "search_files",
            "Search literal text in workspace files; returns matching paths, line numbers and bounded excerpts.",
            json!({"query":{"type":"string"},"path":{"type":"string"}}),
            &["query", "path"],
        ),
        make(
            "write_file",
            "Create or replace a UTF-8 file. For existing files expected_content must exactly match the current contents; null is only valid for a new file. Edits are journaled for review and safe undo.",
            json!({"path":{"type":"string"},"content":{"type":"string"},"expected_content":{"type":["string","null"]}}),
            &["path", "content", "expected_content"],
        ),
        make(
            "edit_file",
            "Replace exactly one occurrence of old_text in a UTF-8 file. Refuses missing or ambiguous matches; edits are journaled for safe undo.",
            json!({"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}}),
            &["path", "old_text", "new_text"],
        ),
        make(
            "run_command",
            "Run a foreground shell command after explicit one-time user approval. macOS sandbox limits writes to the project, denies network and protected paths. Defaults to 60s, max 120s. Background daemons, detached processes and setsid are unsupported; do not start them. Output is bounded; command edits are reviewed but not automatically reversible.",
            json!({"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":120}}),
            &["command"],
        ),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathInput {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    path: String,
    #[serde(default)]
    offset: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchInput {
    path: String,
    query: String,
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
                    entries.push(json!({"name":name,"directory":kind.is_dir()}));
                    if entries.len() > 1000 {
                        return Err(io::Error::other(
                            "directory has too many entries; choose a subdirectory",
                        ));
                    }
                }
                entries.sort_by_key(|entry| entry["name"].as_str().unwrap_or_default().to_owned());
                json!({"entries":entries}).to_string()
            }
            "read_file" => {
                let input: ReadInput = parse(&call.arguments)?;
                let target = paths::resolve(self.root(), &input.path)?;
                let content = paths::text_file(&target)?.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "file does not exist")
                })?;
                let mut chars = content.chars().skip(input.offset).peekable();
                let chunk: String = chars.by_ref().take(8192).collect();
                let next = chars.peek().map(|_| input.offset + chunk.chars().count());
                json!({"path":input.path,"content":chunk,"offset":input.offset,"next_offset":next})
                    .to_string()
            }
            "search_files" => {
                let input: SearchInput = parse(&call.arguments)?;
                if input.query.is_empty() || input.query.len() > 1024 {
                    return Err(io::Error::other("query must contain 1 to 1024 bytes"));
                }
                let target = paths::resolve(self.root(), &input.path)?;
                let all = if target.is_file() {
                    [(
                        input.path.clone(),
                        paths::text_file(&target)?.unwrap_or_default(),
                    )]
                    .into()
                } else {
                    crate::journal::snapshot(&target)?
                };
                let mut results = Vec::new();
                'outer: for (path, content) in all {
                    for (index, line) in content.lines().enumerate() {
                        if line.contains(&input.query) {
                            let path =
                                if target.is_dir() && !input.path.is_empty() && input.path != "." {
                                    format!("{}/{path}", input.path.trim_end_matches('/'))
                                } else {
                                    path.clone()
                                };
                            results.push(
                                json!({"path":path,"line":index+1,"text":paths::bounded(line,256)}),
                            );
                            if results.len() == 100 {
                                break 'outer;
                            }
                        }
                    }
                }
                json!({"limit_reached":results.len()==100,"matches":results}).to_string()
            }
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
        let target = paths::resolve(self.root(), path)?;
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
    let Some(content) = content else {
        return fs::remove_file(target);
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
