# Right-rail web panels

A web panel is an address a reader pins to the right rail. It is drawn by the
**embedded browser** in a user-owned tab, so the page is a real page: its script
runs, links and forms work, and a dashboard behaves like a dashboard.

## What a panel is not

- **Not a plugin.** Nothing is installed. There is no package, manifest,
  version, permission, or consent behind a panel; a panel is a title and an
  address.
- **Not a fetched copy.** The desktop renders through the `gpui-pre` backend,
  which has no in-process web engine, so a panel is not the page's HTML rendered
  as text. It is the same Chrome/Chromium/Edge process the embedded browser
  already owns, shown as CDP screencast frames with input forwarded back. When
  the browser cannot start, the rail says why instead of falling back to a
  static document.

## Storage

Rows live in `right_rail_plugins`, the table the rail always kept its
non-plugin surfaces in:

| column | value |
| --- | --- |
| `plugin_id` | `web.<request-id>` |
| `kind` | `web` |
| `display_name` | the title |
| `url` | the address |
| `order_index` | where it sits on the rail |
| `deleted_at_ms` | soft-delete marker |

`RightRailPluginRepository` reads and writes them, `RightRailHandle`
(`DesktopRuntime::right_rail()`) exposes them, and the management backend
carries them to the desktop. The list is local to the desktop; a remote
authority answers `right_rail_remote_unsupported`.

## The rail

A pinned panel becomes a `RightRailEntry::Web` beside the built-in and plugin
buttons, in the same reorderable bar. Its order is the persisted
`activity_order`, so it drags and restores like every other button.

Opening one:

1. ensure the workspace's browser session;
2. create a `BrowserTabOwner::User` tab at the address;
3. attach a `BrowserSurface` to it and cache it in the rail.

The surface streams frames while the rail is visible. Hiding the rail stops the
screencast without closing the tab, so reopening is instant. Removing a panel
closes its tab; a surface whose panel is gone is pruned on the next refresh.

## Validation

- Title: non-empty, at most 64 characters.
- Address: public `https`, or `http` on loopback (`localhost`, `127.0.0.1`,
  `::1`); no credentials in the address; at most 2048 bytes.
- At most 24 panels.

## Availability and policy

The embedded browser needs a Chrome-family browser on the machine and a
connected runtime. Without either, the panel shows the reason (`is_unavailable`)
rather than a fallback surface.

Navigation policy — the origin-approval flow — gates agent-initiated
navigation. A pinned panel is user-driven: the reader typed the address, so it
takes the same user-tab path the human browser panel already uses.
