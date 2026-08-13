# Remote Protocol v2

Remote Protocol v2 is the single business protocol for Direct and self-hosted
Relay transports. `crates/core/src/remote_v2.rs` is the wire source of truth;
native and WASM clients consume those Rust contracts directly.

## Compatibility

- Protocol v2 uses range negotiation and currently selects `2.0`.
- A connection starts with `control/hello` and receives `control/server_info`.
- Unknown client, control, JSON-message, attachment, binary-frame, timeout, close,
  and transport enum values decode as `unknown`. Unknown active messages close
  with a structured protocol reason instead of panicking.
- Existing `0.4` HTTP and `/ws` request/response routes are compatibility
  endpoints. New clients use `/ws/v2`; Direct and Relay wrap the same v2 JSON and
  binary contracts rather than defining separate business APIs.

## JSON And Binary Frames

- JSON `control` frames own hello, ping/pong, subscribe, attach/detach, resync,
  and close state.
- JSON `rpc_request` / `rpc_response` frames preserve request and correlation
  ids. RPC timeout ends only that request, not the socket, and each connection
  has a bounded in-flight RPC limit.
- JSON `event` frames carry a domain generation and monotonic sequence.
- Binary frames begin with `VBX2`, a big-endian JSON-header length, the typed
  header, then raw payload bytes. Terminal bytes are never converted to UTF-8.
- Mutations use a bounded idempotency key. Files additionally use the existing
  content revision/CAS contract. A generation or cursor gap returns
  `resync_required` and points at an authoritative operation.
- Live attachments reauthenticate and authorize their domain before a stream is
  started. Terminal attachments include their workspace scope, and terminal
  input is generation-checked, authorized, and audited without retaining bytes.

## Pairing And Mobile Authentication

- The desktop identity is generated under the runtime home and persisted with
  `0600` permissions on Unix. The current identity/proof primitive uses mature
  X25519, HKDF-SHA256, and HMAC-SHA256 implementations already in the workspace.
- Pairing offers live for 60-120 seconds, are cancelable, and are consumed by a
  conditional update inside the same SQLite transaction that creates the device
  grant. Challenge, grant, nonce, and ticket plaintext are never stored.
- QR/deep-link data uses
  `vibex://open/<transport>#/pair/<base64url-offer>`, where `<transport>` is an
  offered `direct`, `tailnet`, or `self_hosted_relay` route. The mobile host
  parses it locally, removes the fragment before asynchronous work, and passes
  only the transport hint plus offer to Rust for validation.
- A QR contains Direct candidates, an optional user-provided self-hosted Relay,
  the desktop public identity, permission summary, expiry, and one-time
  challenge. It never contains a long-lived grant, provider secret, private key,
  or workspace data.
- WASM/WebView WebSockets exchange a device grant for a 30-second, single-use
  ticket. The ticket travels in a controlled WebSocket subprotocol and never in
  a URL.
- The hello proof binds the single-use ticket challenge, full hello transcript,
  server identity, session epoch, paired device identity, and client ephemeral
  key. `server_info` returns a server ephemeral key and session-key confirmation.

## Deployment

- RemoteGateway defaults to disabled and loopback. LAN bind requires explicit
  `Lan` mode plus `TrustedHttpsProxy` policy.
- Recommended private deployment keeps the listener on loopback and publishes it
  through Tailscale Serve or an equivalent controlled HTTPS/WSS reverse proxy.
- Host and Origin are independently validated. CORS reflects only an accepted
  Origin, supports Private Network Access preflight, and never uses `*`.
- `RemoteGateway` exposes protocol and health endpoints only. It never serves
  the bundled mobile runtime or any browser UI. Workspace file operations retain
  the stricter `WorkspaceFileService` canonicalization and symlink policy.

Example Tailscale deployment (verify syntax against the installed version):

```bash
tailscale serve --bg http://127.0.0.1:1428
tailscale serve status
```

Revoking a device updates durable state and immediately signals every active
connection for that device with `device_revoked`. Runtime shutdown sends
`server_shutdown`, drains the listener, and releases all sockets.
