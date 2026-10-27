//! Shared protected-name policy for filesystem tools and native sandboxes.
pub(crate) const PROTECTED_NAMES: &[&str] = &[
    ".git",
    ".ssh",
    ".aws",
    ".gnupg",
    ".codex",
    ".agents",
    ".npmrc",
    ".netrc",
    "credentials",
    "credentials.json",
    ".env",
];
pub(crate) const PROTECTED_PREFIXES: &[&str] = &[".env.", "id_rsa", "id_ed25519"];
pub(crate) const PROTECTED_SUFFIXES: &[&str] = &[".pem", ".key"];

pub(crate) fn denied_component(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    PROTECTED_NAMES.contains(&name.as_str())
        || PROTECTED_PREFIXES
            .iter()
            .any(|prefix| name.starts_with(prefix))
        || PROTECTED_SUFFIXES
            .iter()
            .any(|suffix| name.ends_with(suffix))
}
