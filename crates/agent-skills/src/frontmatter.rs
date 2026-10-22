use serde::Deserialize;

use crate::SkillError;

const MAX_FRONTMATTER_BYTES: usize = 16 * 1024;
const MAX_NAME_CHARS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 1024;

#[derive(Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
}

pub(crate) struct SkillMetadata {
    pub name: String,
    pub description: String,
}

/// Standard YAML handles folded descriptions, quotes, comments and Unicode.
/// Unknown optional frontmatter fields are deliberately accepted for portability
/// with existing skill packages; they do not grant permissions or execute code.
pub(crate) fn parse(content: &str) -> Result<SkillMetadata, SkillError> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = content.split_inclusive('\n');
    if lines.next().is_none_or(|line| line.trim_end() != "---") {
        return Err(SkillError::new(
            "SKILL.md must start with YAML frontmatter delimited by ---",
        ));
    }
    let mut frontmatter = String::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        if frontmatter.len() + line.len() > MAX_FRONTMATTER_BYTES {
            return Err(SkillError::new("skill frontmatter exceeds 16 KiB limit"));
        }
        frontmatter.push_str(line);
    }
    if !closed {
        return Err(SkillError::new("skill YAML frontmatter has no closing ---"));
    }
    let metadata: Frontmatter = serde_yaml::from_str(&frontmatter)
        .map_err(|error| SkillError::new(format!("invalid skill YAML frontmatter: {error}")))?;
    let name = metadata.name.trim().to_owned();
    if name.is_empty()
        || name.chars().count() > MAX_NAME_CHARS
        || !name
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(SkillError::new(
            "skill name must be 1–64 letters, numbers, hyphens or underscores",
        ));
    }
    let description = metadata
        .description
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if description.is_empty()
        || description.chars().count() > MAX_DESCRIPTION_CHARS
        || description.chars().any(char::is_control)
    {
        return Err(SkillError::new(
            "skill description must be 1–1024 text characters",
        ));
    }
    if lines.all(|line| line.trim().is_empty()) {
        return Err(SkillError::new(
            "SKILL.md must include instructions after its frontmatter",
        ));
    }
    Ok(SkillMetadata { name, description })
}
