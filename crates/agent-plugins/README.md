# agent-plugins

Local capability packages that contribute existing Skills and MCP configurations.
The package manager does not execute scripts, start MCP servers, install runtime
dependencies, contact a marketplace, or add a second agent execution loop.

## Package format

Import a directory or ZIP containing a root `plugin.json`:

```text
document-review/
  plugin.json
  skills/
    document-review/
      SKILL.md
  mcp.json                 # optional
```

```json
{
  "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
  "name": "document-review",
  "version": "1.0.0",
  "description": "Review documents against supplied requirements."
}
```

`name` is the stable package identity and skill namespace. It accepts 1–64
lowercase ASCII letters, numbers, hyphens, underscores or single dots, beginning
and ending in a letter or number. A skill named `review` becomes
`document-review:review`. Changing a package identity is a new install rather than
an update. Version labels are informational; SHA-256 records actual package content.

Root manifests default to `skills/` and `mcp.json`. An optional `skills` path or
array can select other package-relative directories; `mcpServers` can be a
package-relative JSON filename or an inline server map. `./` paths are accepted.
ZIPs may contain the package at their root or inside exactly one enclosing folder.

The compatibility subset also accepts `.codex-plugin/plugin.json` with `skills`
and `mcpServers`; its default MCP file is `.mcp.json`. A root `plugin.json` takes
precedence over that legacy file. This is a supported subset, not a claim of full
Codex compatibility: Apps, Hooks, commands, agents, install scripts, dependencies,
onboarding workflows and vendor extensions are reported as unsupported and not
executed. A package containing no supported capability is rejected.

The `examples/document-review` directory is a complete importable package.

## MCP configuration

`mcp.json` / `.mcp.json`:

```json
{
  "mcpServers": {
    "local-service": {
      "command": "node",
      "args": ["${PLUGIN_ROOT}/server.mjs"],
      "env_vars": ["SERVICE_TOKEN"]
    },
    "remote-service": {
      "type": "http",
      "url": "https://example.com/mcp",
      "bearer_token_env": "SERVICE_TOKEN"
    }
  }
}
```

The referenced script must exist in the package. The `command` may be an
executable already on PATH or a bundled `./path`; `${PLUGIN_ROOT}`,
`${CODEX_PLUGIN_ROOT}` and `${CLAUDE_PLUGIN_ROOT}` in command/arguments resolve to
the immutable installed package, not its import directory. Arguments beginning
with `./` are also resolved against that root. Other arguments are literal and
have no implicit package working directory: use `./server.js` or a root placeholder
for bundled scripts. On Unix a directly invoked bundled command must have an
executable permission bit; scripts passed to an interpreter need not. No interpreter or
package manager is invoked during installation. Runtime dependencies must already
exist on the host, and MCP calls retain the host's ordinary approval requirements.

Only stdio and Streamable HTTP transports are accepted. `env` may contain matching
references such as `"SERVICE_TOKEN": "${SERVICE_TOKEN}"`; values are supplied by
the host environment. A single `Authorization: Bearer ${ENV_NAME}` header is
converted to a bearer-token environment reference. Literal secrets, renamed env
references, custom headers, OAuth configuration, SSE and unknown MCP config fields
are rejected with an actionable error rather than silently ignored. Secrets must
not be embedded in any package file. HTTPS is required except for loopback HTTP.

MCP IDs are stable `plugin_`-prefixed hashes of package identity and declared server
name. Display names include the package name. Disabled packages or disabled server
entries do not contribute runtime configurations. Installing a package does not
connect to any of its servers.

## Lifecycle and storage

`PluginStore::open` should run once at host startup, before creating runs. The host
owns one store and serializes changes. Separate processes must not mutate the same
store concurrently. Use `install(source, None)` for a new identity and
`install(source, Some(id))` for an explicit update. `install_checked` additionally
accepts a precommit host validation callback for cross-catalog constraints.

Imports are copied to staging and validated, including every Skill's frontmatter
and limits. Complete immutable revisions are moved into `packages/` before an
atomic `registry.json` replacement publishes the change. Validation, host guard,
or persistence failures preserve the previous active installation. Updates retain
the enabled state. Updating and uninstalling keep old files available to in-flight
run snapshots. The next startup reclaims unreferenced revisions and staging data.
This is failure recovery, not a user-facing history or rollback catalog.

Missing, modified or unsafe package content is isolated on startup: the package is
disabled with a warning so the user can repair or uninstall it. Enabling rechecks
its content hash. Invalid registry structure itself is an error.

## Boundaries and limits

- Imports reject symlinks, hard-linked files on Unix, devices and other special
  files, ZIP traversal, duplicate/case-colliding archive paths and absolute paths.
  Unix directory imports use retained descriptors and `openat(O_NOFOLLOW)` to
  avoid following replacement symlinks. Windows uses filesystem link checks;
  package storage should remain private to the host application.
- A package is limited to 4,096 entries, 16 directory levels, 16 MiB per file and
  64 MiB total uncompressed content. ZIP input itself is capped at 64 MiB. Manifests
  and MCP JSON files are limited to 256 KiB.
- Across installed packages, including disabled ones: 64 packages, 64 MCP servers,
  64 Skill roots and 192 Skills. The Skill allowance leaves room for independent
  project/user Skills in the shared catalog.
- Skill text remains untrusted workflow content. Package metadata never changes
  user permissions, workspace access, tool approvals or model protocol behavior.
- Installation is not an execution sandbox or a security audit of package code.
  Authorizing a later MCP process uses the host's existing execution policy.

Tests exercise lifecycle/restart cleanup, failed updates and atomic publication,
host guards, missing/modified package isolation, namespace/ID stability, root path
substitution, unsupported credentials, links/traversal/ZIP bounds and global limits.
