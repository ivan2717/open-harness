//! Seatbelt is path based: an existing workspace hard link must not turn its
//! external inode into an authorized file. Block aliases and pin their parent
//! names against sandboxed renames; new hard links are denied by the profile.
use crate::paths;
use std::{
    collections::BTreeSet,
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const MAX_ENTRIES: usize = 250_000;
const MAX_SCAN_TIME: Duration = Duration::from_secs(5);
const MAX_PROFILE_BYTES: usize = 48 * 1024;

pub(super) fn restrictions(root: &Path, write_paths: &[PathBuf]) -> io::Result<String> {
    let mut pending = vec![root.to_owned()];
    pending.extend(
        write_paths
            .iter()
            .filter(|path| !path.starts_with(root))
            .cloned(),
    );
    let mut aliases = BTreeSet::new();
    let mut parents = BTreeSet::new();
    let mut count = 0;
    let started = Instant::now();
    while let Some(path) = pending.pop() {
        count += 1;
        if count > MAX_ENTRIES || started.elapsed() > MAX_SCAN_TIME {
            return Err(paths::denied(
                "workspace link safety scan exceeded its limit; choose a smaller workspace before running commands",
            ));
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            // Seatbelt checks the resolved target. Keeping internal dependency
            // symlinks is safe; never traverse their potentially external tree.
            continue;
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                if count + pending.len() >= MAX_ENTRIES || started.elapsed() > MAX_SCAN_TIME {
                    return Err(paths::denied(
                        "workspace link safety scan exceeded its limit; choose a smaller workspace before running commands",
                    ));
                }
                pending.push(entry?.path());
            }
        } else if metadata.is_file() && metadata.nlink() > 1 {
            aliases.insert(path.clone());
            for parent in path.ancestors().skip(1) {
                if !parent.starts_with(root) {
                    break;
                }
                parents.insert(parent.to_owned());
            }
            // Bound memory as well as the eventual sandbox argument size.
            if aliases.len() + parents.len() > 512 {
                return Err(too_many_links());
            }
        }
    }
    let mut profile = String::new();
    for path in aliases {
        profile.push_str(&format!(
            "(deny file-read* file-write* (literal {}))\n",
            super::quoted(&path)
        ));
    }
    for path in parents {
        // Without this, renaming an ancestor changes the alias pathname and
        // bypasses a literal deny while retaining the same external inode.
        profile.push_str(&format!(
            "(deny file-write-unlink (literal {}))\n",
            super::quoted(&path)
        ));
    }
    if profile.len() > MAX_PROFILE_BYTES {
        return Err(too_many_links());
    }
    Ok(profile)
}

fn too_many_links() -> io::Error {
    paths::denied(
        "workspace contains too many hard-linked files for safe command execution; use copied or cloned dependencies instead of hard links",
    )
}
