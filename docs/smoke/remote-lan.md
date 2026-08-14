# Remote LAN Smoke Checklist

Use this checklist to verify the installed mobile app against a PC runtime on
the same LAN or a user-managed private network.

## Preconditions

- PC runtime is running with remote service intentionally enabled.
- A validated Direct or Tailnet HTTPS/WSS endpoint is enabled in Desktop.
- The phone is on the same LAN/private network and has the current app installed.
- A test workspace has at least one Agent session, a Git repository, and one
  terminal session.
- Provider profiles exist in Vibex, including one profile that can show an
  injection preview and health summary.

Do not expose a public listener without pairing/auth enabled. Do not paste auth
tokens, pairing codes, provider secrets, or terminal input into screenshots or
logs.

## Evidence To Record

- Bounded endpoint class (`direct` or `tailnet`), without credentials or pairing data.
- Mobile device class, app/APK identity, and permission level.
- Any structured error code if a step fails.
- Reconnect/catch-up observation notes.

## Steps

1. Pair the installed mobile app.

   - Create or claim a device proof through the PC pairing flow.
   - Record the device permission level: `read_only`, `approve_only`, or
     `full_control`.
   - Scan or open the Desktop-generated `vibex://open/<transport>#/pair/...`
     entry. There is no browser fallback.
   - Confirm `/api/info` reports remote enabled and expected capabilities.

2. Verify Agent session message flow.

   - Open a session from the mobile session list.
   - Send a short non-secret message.
   - Refresh timeline or wait for live update.
   - Confirm the new user message appears with an authoritative sequence.

3. Verify permission approval.

   - Trigger or select a pending permission request.
   - Approve or deny from mobile using an `approve_only` or `full_control`
     device.
   - Confirm the resolution card shows the mobile device as responder.

4. Verify Git diff.

   - Open the Git panel for the selected session/workspace.
   - Select a changed file.
   - Confirm the diff body renders and the path matches the PC workspace.
   - If staging is tested, use a `full_control` device and confirm Git status
     refreshes after the action.

5. Verify terminal command/write path.

   - Open the terminal panel.
   - Confirm a snapshot renders without high-volume output flooding the UI.
   - With `full_control`, send a harmless command such as `pwd`.
   - Confirm a later snapshot shows the command or its output.

6. Verify Provider settings.

   - Open the Provider settings panel.
   - Confirm profile summaries, redacted injection preview, health, usage, and
     failover cards render.
   - Confirm env values are redacted and native config file contents are not
     displayed.
   - With `full_control`, run the auth health probe.
   - Confirm health summaries refresh or a structured permission error is shown.

7. Verify reconnect catch-up.

   - Disconnect the phone network or background the app.
   - Create one timeline change from the PC runtime while disconnected.
   - Resume the app with the same securely stored device proof.
   - Confirm the missed timeline item appears after catch-up before relying on
     live events.

## Pass Criteria

- Mobile connects to the PC runtime over LAN/private network.
- Mobile sends an Agent message, resolves a permission, shows Git diff, writes
  terminal input, and displays Provider settings through the remote backend.
- Read-only devices can read Provider summaries but cannot run health probes.
- Full-control Provider health probe is audited with a redacted summary.
- Reconnect fetches missed authoritative timeline items before live rendering.

## Local Development Note

When a real LAN runtime is not available, use the native mobile client for local
pairing and route diagnostics. This is local development evidence and does not
replace a physical LAN validation.
