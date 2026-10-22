# Local skills

This crate follows the progressive disclosure used by Codex: advertise skill
names and descriptions, load `SKILL.md` when selected, then read only the
references required by that workflow. It does not execute scripts, install
packages, infer credentials, or load another application's configuration.

## Discovery

`SkillCatalogConfig::for_workspace(user_root, project_root)` discovers:

- `<project>/.agents/skills/`
- `<project>/.openharness/skills/`
- The application-managed `user_root` supplied by the caller

Explicit `SkillRoot` entries can add directories. Roots must be absolute real
directories; the caller should canonicalize the trusted project/application
base before appending the skill path. Symbolic links inside skill roots are not
followed. Project skills override personal skills with the same name. Within a
scope, paths are sorted deterministically, so `.agents` takes precedence over
`.openharness`. Additional roots have the lowest priority. The skill name is its
stable identifier; disabling that identifier disables the effective skill,
including any fallback with the same name.

Each skill is a directory containing `SKILL.md`. Its YAML frontmatter requires
`name` (1–64 letters, numbers, hyphens or underscores) and `description` (1–1024
characters). Folded YAML, Unicode, and additional metadata fields are supported.
Actual instructions must follow the frontmatter. Nested skills are not discovered
inside an already discovered skill's resource directory.

## API and tools

`SkillCatalog::discover(config)` returns valid metadata plus diagnostics for
invalid or shadowed entries. A catalog is an immutable snapshot of enabled state
and main instructions. Rediscover after configuration or installation changes;
callers can cache it between updates. Resource files are read on demand.

The catalog implements `agent_runtime::ToolExecutor`:

- `list_skills` returns enabled metadata only, with `next_offset` for more entries.
- `load_skill({ "id": "..." })` returns instructions and their base path. Follow
  `next_offset` until it is null before using the skill's instructions.
- `read_skill_resource({ "id": "...", "path": "references/guide.md" })` reads a
  contained UTF-8 file, also paginated with `next_offset`. Reading script source
  does not run it. Offsets in text tools count Unicode characters, not bytes.

`prompt()` supplies metadata and selection guidance, including explicit
`$skill-name` activation. Skill content remains untrusted workflow data and
cannot override user requests, harness instructions, permissions, or approvals.

## Bounds

Discovery permits at most 32 roots, 512 directories and 4096 entries per root,
six directory levels, 256 effective skills, and 256 diagnostics. Each instruction
or resource file is limited to 128 KiB, with at most 16 KiB of frontmatter. Binary
files, parent traversal, absolute resource paths, protected files, symbolic links
and hard-linked files are rejected. Unix reads use directory-relative `openat`
with `O_NOFOLLOW` for every path component so a directory replacement cannot
redirect an active skill read outside its original directory.

Reference: [Codex skills](https://developers.openai.com/codex/skills).
