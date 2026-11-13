# Plugin Host API

Vibex plugins run in the desktop scripting host. Besides the `gpui-kit` and
`gpui-base` modules, the host exposes a small, versioned native surface. A
plugin imports a module by name; nothing is available through a global lookup.

The host API is **deny by default**. A module only exists for a plugin whose
manifest requested the matching permission, and every function checks the
grant again before answering.

## `vibex.session` (requires `agent.read`)

```js
import { current } from "vibex.session";

const session = current();
// null, or:
// {
//   id: "session-id",
//   title: "Fix the parser",
//   workspaceId: "workspace-id",
//   workspaceRoot: "/home/me/project",
//   agentId: "codex"
// }
```

`current()` returns `null` when no Agent session is selected.

## `vibex.workspace` (requires `agent.read`)

```js
import { current } from "vibex.workspace";

const workspace = current();
// null, or { id: "workspace-id", root: "/home/me/project" }
```

The workspace module is part of the same read-only session context as
`vibex.session`; a plugin that only needs the workspace still requests
`agent.read`.

## `vibex.settings` (requires `storage`)

```js
import { get, save } from "vibex.settings";

const theme = get("theme");        // null when unset
save("theme", "dark");             // true
```

The store is scoped to the plugin's data directory and survives upgrades. Values
are JSON: null, boolean, number, string, array or object. The file is written
atomically (temporary file, then rename).

## Commands

A plugin can contribute commands through its manifest:

```json
{
  "contributes": {
    "commands": [
      { "id": "open-session-card", "title": "Open session card" }
    ]
  }
}
```

Each command appears in the command palette under **Plugin commands**. Selecting
one opens that plugin's panel; the panel decides what the command does. This is
the safe first shape: the host does not execute arbitrary script callbacks from
the palette, and a command cannot run without the plugin's panel surface.

## Manifest

Request the permission in `vibex-plugin.json`:

```json
{
  "id": "com.example.session-card",
  "name": "Session card",
  "version": "1.0.0",
  "license": "MIT",
  "entry": "main.js",
  "capabilities": {
    "panel": true,
    "agent": { "read": true }
  }
}
```

The consent sheet shows `agent.read` before installation. An update that adds
the permission asks again; an update that does not add permissions keeps the
existing grant.

## Example

```js
import { View, div } from "gpui-kit";
import { current } from "vibex.session";

export default class SessionCard extends View {
  render() {
    const session = current();
    return div().child(session ? session.title : "No session selected");
  }
}
```

## Versioning

The module functions documented here are the `v1` surface. New optional fields
may be added to returned objects; removed fields or changed argument shapes
require a new module name. The host injects the module only when the desktop
plugin host is running; TUI and mobile builds do not load plugins.
