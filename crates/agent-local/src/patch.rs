//! Preflight the whole patch, persist intent, then apply atomic per-file edits.
use crate::{
    LocalTools, PermissionMode, files,
    journal::{SavedFile, SavedPermissions},
    lock_error,
    patch_format::{self, Operation},
    paths,
};
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeMap, io, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    patch: String,
}

struct Change {
    journaled: bool,
    path: String,
    target: PathBuf,
    before: Option<String>,
    before_permissions: Option<SavedPermissions>,
    after: Option<String>,
}

impl LocalTools {
    pub(crate) fn apply_patch(&self, arguments: &str) -> io::Result<String> {
        if self.inner.mode != PermissionMode::WorkspaceWrite {
            return Err(paths::denied("workspace is read-only"));
        }
        let input: Input = files::parse(arguments)?;
        let mut changes: Vec<Change> = Vec::new();
        for operation in patch_format::parse(&input.patch)? {
            let target = self.authorize_write(operation.path())?;
            if target == self.root() {
                return Err(paths::denied("cannot replace the workspace"));
            }
            let path = target
                .strip_prefix(self.root())
                .unwrap_or(&target)
                .to_string_lossy()
                .into_owned();
            // Reject aliases and ancestor conflicts before touching any file.
            if changes.iter().any(|change| {
                target.starts_with(&change.target)
                    || change.target.starts_with(&target)
                    || path.eq_ignore_ascii_case(&change.path)
            }) {
                return Err(io::Error::other("duplicate or overlapping paths in patch"));
            }
            let before = paths::text_file(&target)?;
            let after = match operation {
                Operation::Add { content, .. } => {
                    if before.is_some() {
                        return Err(io::Error::new(
                            io::ErrorKind::AlreadyExists,
                            "Add File requires a new path",
                        ));
                    }
                    Some(content)
                }
                Operation::Update { hunks, .. } => Some(patch_format::update(
                    before.as_deref().ok_or_else(missing)?,
                    &hunks,
                )?),
                Operation::Delete { .. } => {
                    if before.is_none() {
                        return Err(missing());
                    }
                    None
                }
            };
            if after
                .as_ref()
                .is_some_and(|content| content.len() > paths::FILE_LIMIT)
            {
                return Err(io::Error::other("patched file exceeds 256 KiB"));
            }
            let before_permissions = files::file_permissions(&target)?;
            changes.push(Change {
                journaled: self.journal_path(&target).is_some(),
                before_permissions,
                path,
                target,
                before,
                after,
            });
        }
        let output = json!({"applied":true,"files":changes.iter().map(|change|json!({"path":change.path,"project_undo":change.journaled,"status":if change.before.is_none(){"added"}else if change.after.is_none(){"deleted"}else{"modified"}})).collect::<Vec<_>>()}).to_string();
        if output.len() > paths::OUTPUT_LIMIT {
            return Err(io::Error::other(
                "patch result exceeds 64 KiB; split the patch",
            ));
        }
        self.commit_patch(&changes)?;
        Ok(output)
    }

    fn commit_patch(&self, changes: &[Change]) -> io::Result<()> {
        let mut journal = self.inner.journal.lock().map_err(lock_error)?;
        let prior: BTreeMap<_, _> = changes
            .iter()
            .filter(|change| change.journaled)
            .map(|change| {
                (
                    change.path.clone(),
                    journal.files.get(&change.path).cloned(),
                )
            })
            .collect();
        for change in changes {
            self.check_patch_target(change)?;
        }
        for change in changes.iter().filter(|change| change.journaled) {
            let previous = prior[&change.path].as_ref();
            journal.files.insert(
                change.path.clone(),
                SavedFile {
                    before_permissions: previous
                        .and_then(|saved| saved.before_permissions.clone())
                        .or_else(|| change.before_permissions.clone()),
                    before: previous.map_or(change.before.clone(), |saved| saved.before.clone()),
                    after: change.after.clone(),
                    owned: previous.is_none_or(|saved| saved.owned && saved.after == change.before),
                },
            );
        }
        if let Err(error) = journal.save(&self.inner.state_dir) {
            restore_entries(&mut journal.files, &prior);
            return Err(error);
        }
        for (index, change) in changes.iter().enumerate() {
            let result = (|| {
                self.check_patch_target(change)?;
                if change.after.is_some() {
                    paths::create_parent_directory(&change.target)?;
                    self.check_patch_target(change)?;
                }
                files::replace_with_permissions(
                    &change.target,
                    change.after.as_deref(),
                    change.before_permissions.as_ref(),
                )
            })();
            if let Err(error) = result {
                // A failed sync can follow a successful rename, so include the
                // attempted file. Never overwrite an intervening external edit.
                let mut incomplete = false;
                for attempted in changes[..=index].iter().rev() {
                    let current = self
                        .verify_root()
                        .and_then(|()| self.file_scope().resolve(&attempted.path))
                        .and_then(|target| paths::text_file(&target));
                    let restored = match current {
                        Ok(current) if current == attempted.before => true,
                        Ok(current) if current == attempted.after => {
                            files::replace_with_permissions(
                                &attempted.target,
                                attempted.before.as_deref(),
                                attempted.before_permissions.as_ref(),
                            )
                            .is_ok()
                        }
                        _ => false,
                    };
                    if restored && attempted.journaled {
                        restore_entry(
                            &mut journal.files,
                            &attempted.path,
                            prior[&attempted.path].clone(),
                        );
                    } else if !restored {
                        incomplete = true;
                        if let Some(saved) = journal.files.get_mut(&attempted.path) {
                            saved.owned = false;
                        }
                    }
                }
                for unattempted in changes[index + 1..]
                    .iter()
                    .filter(|change| change.journaled)
                {
                    restore_entry(
                        &mut journal.files,
                        &unattempted.path,
                        prior[&unattempted.path].clone(),
                    );
                }
                if let Err(journal_error) = journal.save(&self.inner.state_dir) {
                    return Err(io::Error::other(format!(
                        "patch failed: {error}; recovery journal could not be saved: {journal_error}; inspect workspace changes"
                    )));
                }
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "patch failed: {error}; {}",
                        if incomplete {
                            "rollback incomplete; inspect workspace changes before retrying"
                        } else {
                            "file contents rolled back"
                        }
                    ),
                ));
            }
        }
        Ok(())
    }

    fn check_patch_target(&self, change: &Change) -> io::Result<()> {
        self.verify_root()?;
        let target = self.authorize_write(&change.path)?;
        if paths::text_file(&target)? != change.before {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "file changed during patch; read current contents before retrying",
            ));
        }
        Ok(())
    }
}

fn restore_entries(
    files: &mut BTreeMap<String, SavedFile>,
    prior: &BTreeMap<String, Option<SavedFile>>,
) {
    for (path, entry) in prior {
        restore_entry(files, path, entry.clone());
    }
}
fn restore_entry(files: &mut BTreeMap<String, SavedFile>, path: &str, prior: Option<SavedFile>) {
    if let Some(prior) = prior {
        files.insert(path.into(), prior);
    } else {
        files.remove(path);
    }
}
fn missing() -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        "Update/Delete File requires an existing file",
    )
}
