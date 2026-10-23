//! A deliberately small, exact-match patch grammar. No fuzzy matching or shell.
use std::io;

pub(super) enum Operation {
    Add { path: String, content: String },
    Update { path: String, hunks: Vec<Hunk> },
    Delete { path: String },
}

impl Operation {
    pub(super) fn path(&self) -> &str {
        match self {
            Self::Add { path, .. } | Self::Update { path, .. } | Self::Delete { path } => path,
        }
    }
}

pub(super) struct Hunk {
    old: Vec<String>,
    new: Vec<NewLine>,
}

enum NewLine {
    Context(usize),
    Added { text: String, anchor: usize },
}

pub(super) fn parse(patch: &str) -> io::Result<Vec<Operation>> {
    if patch.contains('\0') {
        return Err(invalid("patch contains NUL"));
    }
    let lines: Vec<_> = patch.lines().collect();
    if lines.first() != Some(&"*** Begin Patch") || lines.last() != Some(&"*** End Patch") {
        return Err(invalid("expected *** Begin Patch and *** End Patch"));
    }
    let end = lines.len() - 1;
    let mut index = 1;
    let mut operations = Vec::new();
    while index < end {
        if operations.len() == 64 {
            return Err(invalid("patch exceeds 64 files; split it"));
        }
        let header = lines[index];
        index += 1;
        let operation = if let Some(path) = header.strip_prefix("*** Add File: ") {
            let mut content = String::new();
            while index < end && !lines[index].starts_with("*** ") {
                let line = lines[index]
                    .strip_prefix('+')
                    .ok_or_else(|| invalid("added lines must start with +"))?;
                content.push_str(line);
                content.push('\n');
                index += 1;
            }
            Operation::Add {
                path: path.into(),
                content,
            }
        } else if let Some(path) = header.strip_prefix("*** Delete File: ") {
            Operation::Delete { path: path.into() }
        } else if let Some(path) = header.strip_prefix("*** Update File: ") {
            let mut hunks = Vec::new();
            while index < end && !lines[index].starts_with("*** ") {
                if lines[index] != "@@" {
                    return Err(invalid("update hunks must begin with @@"));
                }
                if hunks.len() == 256 {
                    return Err(invalid("patch exceeds 256 hunks per file; split it"));
                }
                index += 1;
                let mut hunk = Hunk {
                    old: vec![],
                    new: vec![],
                };
                let mut changed = false;
                while index < end && lines[index] != "@@" && !lines[index].starts_with("*** ") {
                    let line = lines[index];
                    if let Some(text) = line.strip_prefix(' ') {
                        hunk.new.push(NewLine::Context(hunk.old.len()));
                        hunk.old.push(text.into());
                    } else if let Some(text) = line.strip_prefix('-') {
                        hunk.old.push(text.into());
                        changed = true;
                    } else if let Some(text) = line.strip_prefix('+') {
                        hunk.new.push(NewLine::Added {
                            text: text.into(),
                            anchor: hunk.old.len().saturating_sub(1),
                        });
                        changed = true;
                    } else {
                        return Err(invalid("hunk lines need a space, - or + prefix"));
                    }
                    index += 1;
                }
                if !changed {
                    return Err(invalid("update hunk contains no changes"));
                }
                hunks.push(hunk);
            }
            if hunks.is_empty() {
                return Err(invalid("update requires at least one @@ hunk"));
            }
            Operation::Update {
                path: path.into(),
                hunks,
            }
        } else {
            return Err(invalid(
                "expected Add File, Update File or Delete File header",
            ));
        };
        if operation.path().is_empty() || operation.path().trim() != operation.path() {
            return Err(invalid(
                "file path must be nonempty without surrounding whitespace",
            ));
        }
        operations.push(operation);
    }
    if operations.is_empty() {
        return Err(invalid("patch contains no file operations"));
    }
    Ok(operations)
}

/// Find hunks in the original file, not progressively rewritten content. This
/// prevents a later hunk from accidentally editing lines introduced by an earlier one.
pub(super) fn update(before: &str, hunks: &[Hunk]) -> io::Result<String> {
    let raw: Vec<_> = before.split_inclusive('\n').collect();
    let lines: Vec<_> = raw
        .iter()
        .map(|line| {
            line.strip_suffix('\n')
                .map_or(*line, |line| line.strip_suffix('\r').unwrap_or(line))
        })
        .collect();
    let mut offsets = vec![0];
    for line in &raw {
        offsets.push(offsets.last().unwrap() + line.len());
    }
    let newline = if raw
        .iter()
        .find(|line| line.ends_with('\n'))
        .is_some_and(|line| line.ends_with("\r\n"))
    {
        "\r\n"
    } else {
        "\n"
    };
    let mut result = String::new();
    let mut consumed = 0;
    let mut previous_end = 0;
    for (index, hunk) in hunks.iter().enumerate() {
        let start = if hunk.old.is_empty() {
            if !before.is_empty() || index != 0 || hunks.len() != 1 {
                return Err(invalid(
                    "insertions need context unless the original file is empty",
                ));
            }
            0
        } else {
            let mut positions =
                lines
                    .windows(hunk.old.len())
                    .enumerate()
                    .filter_map(|(index, candidate)| {
                        candidate
                            .iter()
                            .zip(&hunk.old)
                            .all(|(a, b)| *a == b)
                            .then_some(index)
                    });
            let start = positions
                .next()
                .ok_or_else(|| invalid("hunk does not match current file; read it again"))?;
            if positions.next().is_some() {
                return Err(invalid(
                    "hunk matches more than once; include unique context",
                ));
            }
            start
        };
        if start < previous_end {
            return Err(invalid("hunks must be non-overlapping and in file order"));
        }
        let end = start + hunk.old.len();
        let start_byte = offsets[start];
        let end_byte = offsets[end];
        result.push_str(&before[consumed..start_byte]);
        for (index, line) in hunk.new.iter().enumerate() {
            let (text, ending) = match line {
                NewLine::Context(offset) => {
                    let original = raw[start + offset];
                    (
                        lines[start + offset],
                        line_ending(original).unwrap_or(newline),
                    )
                }
                NewLine::Added { text, anchor } => {
                    let ending = raw
                        .get(start + anchor)
                        .and_then(|line| line_ending(line))
                        .unwrap_or(newline);
                    (text.as_str(), ending)
                }
            };
            result.push_str(text);
            if index + 1 < hunk.new.len() || end_byte < before.len() || before.ends_with('\n') {
                result.push_str(ending);
            }
        }
        consumed = end_byte;
        previous_end = end;
    }
    result.push_str(&before[consumed..]);
    Ok(result)
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn line_ending(line: &str) -> Option<&'static str> {
    if line.ends_with("\r\n") {
        Some("\r\n")
    } else if line.ends_with('\n') {
        Some("\n")
    } else {
        None
    }
}
