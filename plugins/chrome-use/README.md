# Chrome connection

This plugin operates approved tabs in an existing Chrome profile. It uses a
Manifest V3 extension, Chrome's `debugger` API (targeted by `tabId`), and a native
messaging host. There is no HTTP listener, remote debugging port, profile copy,
OS keyboard injection, window activation or `bringToFront` call. It inherits the
profile's cookies and login state; separate tabs do not isolate account changes.

## Setup

The production installation entry point is configured by `distribution.json`.
Its store URL is currently `null`: no Chrome Web Store listing exists yet.
[Publishing preparation](PUBLISHING.md) documents the extension-only ZIP, identity
synchronization and the remaining owner-operated publishing steps. A published
listing lets the desktop app open the store installation page and prepare the
local host; Chrome still asks the user to confirm installation.

For local development only:

1. In Open Harness Extensions expand **Developer options** and click **Open local
   extension folder**. This prepares the connection and exports the bundled
   extension into a stable ordinary folder under the app's user-data directory,
   outside the macOS `.app` bundle.
2. In `chrome://extensions`, enable Developer mode, choose **Load unpacked**, and
   select that exported `extension` directory. If the macOS picker opens another
   location, use **Copy extension directory**, press **Cmd+Shift+G** in the picker,
   paste the path and confirm it before choosing **Select**. This local build has
   no store listing or silent installation.
3. Open **Open Harness Chrome** from Chrome's toolbar. Connect, then explicitly
   enable agent-created background tabs or share the current HTTP(S) tab.
4. Enable the Chrome plugin in Open Harness. A conversation's approved open
   creates a background tab; an existing shared tab requires exact tab and origin
   approval. All later operations verify that scope.

On macOS an approved `chrome_open` starts Chrome in the background with the
system launcher if the extension is not connected, then waits for startup. It
never retries an already dispatched operation. Status checks do not launch apps.

Only one Chrome profile may connect at a time. Disconnect the existing profile
before connecting another. The manifest public key fixes the development
extension ID. Change it only during the coordinated store identity migration
described in PUBLISHING.md; the host and desktop must use that same ID. No private
signing key is stored in this repository. Chrome shows
its normal debugger permission and attachment indicators. Browser policies or
an open DevTools session may prevent attachment.

## Desktop integration

`constants.mjs` exports `HOST_NAME`, `EXTENSION_ID` and the protocol limits.
The desktop app creates a configuration file with permissions **0600** under a
user-owned **0700** directory and passes its absolute path in
`OPENHARNESS_CHROME_CONFIG` to the plugin and native host:

```json
{"version":1,"socketPath":"/private/tmp/oh-chrome-USER-HASH/bridge.sock","token":"64-lowercase-hex-characters","extensionId":"keibjmjjimfgkikahpjgbfebiglbjkee"}
```

The socket's own directory must also be user-owned, 0700 and not a symlink. The
socket path must fit within 100 characters. The desktop installer registers the
native messaging manifest `dev.openharness.chrome` with exactly
`chrome-extension://keibjmjjimfgkikahpjgbfebiglbjkee/` in `allowed_origins`. Its
executable launcher must run a trusted Node or Electron-as-Node runtime with the
bundled `native-host.mjs` and the original Chrome command-line arguments. The
host verifies that original extension origin itself. No shell command is built
from browser or tool content. The desktop app registers and updates this connection. Disable the plugin and
remove the Chrome extension to stop using it; the registered host manifest may
remain until a later connection setup updates it.

`node status.mjs` prints one JSON object (`connected`, optional settings/error),
using the same config environment. It never enumerates tabs or reads contents.
`node server.mjs` serves MCP over JSONL stdio. On cancellation or EOF it closes
its socket; the extension detaches the selected tab but leaves tabs open for
review. Chrome owns the native host's lifetime; native-port loss closes all
client sockets and detaches all selected tabs.

Socket protocol: the first JSONL request is
`{id,session_id,method:"authenticate",params:{token}}`. The session ID is a UUID.
Subsequent requests use the fixed `chrome_*` methods in `extension/schema.mjs`.
The bridge maps client request IDs to fresh UUIDs before forwarding native frames
and replies with `{id,result}` or `{id,error}`. It never passes authentication
material to the extension. Requests are bounded to 256 KiB and responses to
8 MiB. Calls are serialized, both per MCP process and in the extension. A timeout
invalidates the client connection rather than retrying a possibly completed
operation.

## Authorization and limitations

### Conversation tab groups

`chrome_open` creates inactive tabs in a green `Open Harness · <short ID>` group.
One conversation reuses its group; different conversations have separate groups.
The desktop passes a trusted conversation identifier to the bundled server, which
derives an opaque layout key scoped to the local connection. This is separate
from the random transport session ID and grants. Reconnecting a task may reuse
its group layout but does not restore access to earlier pages.

Only group IDs and known member tab IDs are kept in `chrome.storage.session`.
They survive worker suspension and native-host reconnection, not an extension
reload/update or browser restart. A live known member is required before reuse;
the extension never discovers groups by title or takes over user-created groups.
Closing a group/window causes the next open to create a new group. Moving the
whole group to another window is respected without activating that window;
renaming/collapsing it is also preserved. Explicitly shared user tabs stay in
their existing locations. Tabs manually added to a group remain private.

Dragging an agent tab out of its group relinquishes its control and close right.
`close_owned` checks the individual creator, group, origin and inactive state;
it never closes a group or its other members. Disconnect leaves pages open.
This grouping applies to pages created through `chrome_open`; arbitrary website
popups do not automatically gain ownership or permissions.

### Page access

- Existing tabs remain private unless shared from the extension's own popup.
  Websites and content scripts cannot call popup grants or native host methods.
- Agent-created tabs belong to one MCP session. Shared tabs can have only one
  controller. Tab IDs are checked again on each call. Grant and snapshot state
  is intentionally not restored after disconnect or browser restart.
- The origin is rechecked before and after observations and before actions. A
  redirect outside the authorized origin stops reading. Agent-owned tabs expose
  only the new origin for a fresh approval and `chrome_select_tab`; existing
  user tabs also need that new origin shared through the popup. Redirects do not
  silently expand access.
- Page operations are fixed functions in an isolated execution world. Model
  input is serialized as data; arbitrary JavaScript, selectors and CDP commands
  are not exposed. Ref revisions are consumed on actions, mutation/navigation
  invalidates stale observations, and action results include fresh observations.
- Screenshots target the selected tab through CDP, including background tabs.
  DOM snapshots are bounded (16k text, 100 viewport controls). Current support
  covers the main document, not cross-origin iframes, native dialogs, password
  filling, downloads/uploads or browser-internal pages.
- User cancellation stops further queued requests; the last dispatched action
  may have happened. Observe instead of automatically repeating it.

## Verification

`node --test plugins/chrome-use/tests/*.test.mjs`

Unit/integration checks use fake Chrome APIs and real temporary local sockets;
no personal browser is started or modified. A manual acceptance check after
installing the extension should keep a user tab active while another session
creates a background test tab, reads it, fills/clicks in it and captures only that
tab. Check that the active tab/window never changes, revoked/cross-origin tabs
are rejected, and process exit detaches without closing the user's browser.

Official API references:
- https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging
- https://developer.chrome.com/docs/extensions/reference/api/debugger
- https://developer.chrome.com/docs/extensions/reference/api/tabs
- https://developer.chrome.com/docs/extensions/reference/api/tabGroups
