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

### `send` (requires `agent.send`)

```js
import { send } from "vibex.session";

send("Run the formatter over src/");
// true  — queued to the current session
// false — no session is selected
```

The message is queued on the host and sent by the workbench through the
authority, so the plugin never talks to the backend itself. An empty message
throws. A plugin may request `agent.send` without `agent.read`; it then gets
only `send`.

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

## `vibex.clipboard` (requires `clipboard`)

```js
import { readText, writeText } from "vibex.clipboard";

readText();            // "" when the clipboard holds nothing textual
writeText("hello");    // true
```

Text only. An image on the clipboard is not returned, because a script has no
surface to draw one on and the value would be a handle it could not use.

## `vibex.network` (requires `net.fetch`)

```js
import { fetch } from "vibex.network";

const { status, body } = await fetch("https://api.example.com/data");
```

The address must be `https` and its host must be one the manifest named:

```json
"capabilities": {
  "network": { "hosts": ["api.example.com", "*.githubusercontent.com"] }
}
```

A pattern is `*` (every host), `*.example.com` (the domain and its subdomains),
or an exact host; matching is anchored, so `*.example.com` does not admit
`evil-example.com`. A private or loopback address is refused even when a
pattern would match it, and the response is capped at 8 MiB. Redirects are not
followed: a hop to an undeclared host would be a grant the user never saw.

## `vibex.fs` (requires `fs.read` / `fs.write`)

```js
import { readText, writeText, list } from "vibex.fs";

readText("/home/me/project/Cargo.toml");
writeText("/home/me/project/notes.md", "# Notes");
list("/home/me/project/src");
```

Every path must be inside a root the manifest declared:

```json
"capabilities": {
  "fs": { "read": ["${workspace}"], "write": ["${workspace}/out"] }
}
```

`${workspace}` and `${home}` are the only variables; an absolute path works
too. A path is normalized before it is compared, so `src/../Cargo.toml` is
inside `${workspace}` while `../secret` is not. A relative root is refused: it
would resolve against whatever the process happened to be running in. One file
is capped at 8 MiB.

## `vibex.process` (requires `process.execute`)

```js
import { run } from "vibex.process";

const { status, stdout, stderr } = await run("git", ["status", "--short"]);
```

A plugin names the executables it may start, never a command line:

```json
"capabilities": { "process": { "executables": ["git", "rg"] } }
```

The name is a plain file name, so a grant for `git` does not admit `./git` or
`/usr/bin/git` — those are different programs that share a name. No shell is
involved: the arguments are passed as an argv array. A run is killed after 30
seconds.

## `vibex.agent.tools` (requires `agent.tool`)

```js
import { register, take, respond } from "vibex.agent.tools";

register("search-notes", "Search the notes in this workspace");

// Invocations the Agent made since the last call, cleared as they are returned.
for (const call of take()) {
  const result = search(call.arguments);   // your own work
  respond(call.id, result);                // the Agent is waiting on this
}
```

The queue shape is the same one `vibex.commands` uses, and for the same reason:
`gpui-shell` has no function value, so the host cannot call back into the
script. An invocation lands in the owning plugin's queue, and `respond` is what
hands the answer back — the Agent's `tools/call` stays open until it arrives.

### How the Agent reaches the tools

The desktop starts a loopback MCP server and installs it on the Agent manager as
the built-in server `vibex-plugins`, beside `vibex-browser` and the computer
tool. It is bound to `127.0.0.1` on an ephemeral port, requires a bearer token
minted at startup, and rejects any request whose `Host` is not loopback.

A tool's name as the Agent sees it is `plugin_<owner>_<name>`, so two plugins
may both register `search`. The endpoint asks the plugin for the unqualified
name it actually registered.

### What the Agent sees when a call goes wrong

| Situation | The Agent gets |
| --- | --- |
| No plugin offers that tool | `isError` with "not a tool any plugin offers" |
| The plugin answers | its text, as content |
| No answer within 30 s | `isError` naming the plugin and tool |

A timeout is reported, never an empty success: an Agent that got a blank result
would conclude the tool found nothing rather than that it never ran.

## Panels

A plugin can contribute a rail panel two ways. The default is its own view —
the `ui.panel` surface the script draws. A `web` panel is a page the embedded
browser opens instead:

```json
{
  "capabilities": { "panel": true },
  "contributes": {
    "panels": [
      { "id": "docs", "title": "Docs", "web": "https://example.com/docs" },
      { "id": "own", "title": "Widgets" }
    ]
  }
}
```

Both need `ui.panel`: the rail is not a plugin's to add to without the grant.
A `web` address must be `https`, or `http` on `localhost` for development. A
plugin made only of `web` panels contributes no script view, so the rail shows
exactly the pages it declared.

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
one opens that plugin's panel and hands the command id to it; the panel decides
what the command does.

A plugin may also register commands at run time, once its panel is up:

```js
import { register, list, take } from "vibex.commands";

register("refresh", "Refresh the card");  // true, false when the id is taken
list();                                    // [{ id: "refresh", title: "..." }]

// Command ids dispatched since the last call, cleared as they are returned.
const run = take();
if (run.includes("refresh")) {
  // Act on the command. Commands from the manifest arrive here too.
}
```

The host cannot call into the script — `gpui-shell` has no function value — so
dispatch is a queue: the palette records the command, opens the panel, and the
script takes the ids when it renders. A command cannot run without the plugin's
panel surface, and a plugin that is not loaded has no queue.

Both `vibex.commands` and the manifest's `contributes.commands` require
`ui.command`.

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
    "command": true,
    "storage": true,
    "clipboard": true,
    "network": { "hosts": ["api.example.com"] },
    "fs": { "read": ["${workspace}"] },
    "process": { "executables": ["git"] },
    "agent": { "read": true, "send": true, "tool": true }
  }
}
```

The consent sheet shows `agent.read`, `agent.send`, `ui.panel` and `ui.command`
before installation. An update that adds a permission asks again; an update that
does not add permissions keeps the existing grant.

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
