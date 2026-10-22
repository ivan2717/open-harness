//! Local skill discovery with metadata-only advertising and scoped, lazy reads.
//!
//! A catalog is a per-run snapshot: its enabled state and `SKILL.md` contents do
//! not change underneath an active run. Discover again to pick up configuration
//! or installation changes. Supporting resources are read on demand, exclusively
//! inside the selected skill directory. Nothing in this crate executes scripts.

mod catalog;
mod filesystem;
mod frontmatter;
mod tools;

pub use catalog::{
    LoadedSkill, SkillCatalog, SkillCatalogConfig, SkillDiagnostic, SkillInfo, SkillResource,
    SkillRoot, SkillScope,
};
pub use tools::{LIST_SKILLS_TOOL, LOAD_SKILL_TOOL, READ_SKILL_RESOURCE_TOOL};

/// Limits are independent of the model context window. They bound local I/O and
/// prevent a skill installation from expanding the tool catalog without limit.
pub const MAX_SKILLS: usize = 256;
pub const MAX_SKILL_BYTES: usize = 128 * 1024;
pub const MAX_RESOURCE_BYTES: usize = 128 * 1024;

#[derive(Debug)]
pub struct SkillError(String);

impl SkillError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SkillError {}

impl From<std::io::Error> for SkillError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<serde_json::Error> for SkillError {
    fn from(error: serde_json::Error) -> Self {
        Self(format!("invalid skill tool arguments: {error}"))
    }
}

#[cfg(test)]
mod tests;
