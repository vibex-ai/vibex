# Desktop GPUI License Gate

`pnpm check:licenses` traverses every normal and build dependency reachable from
`vibex-desktop`, including target-specific macOS, Windows, Linux X11, and Linux
Wayland edges. Cargo dev dependencies are excluded. The command validates SPDX
expressions against `desktop-policy.json`, checks package and asset provenance,
and rejects stale generated output.

The generator still writes a complete inventory when a reviewed graph contains an
unapproved expression. Such components are marked `UNAPPROVED` in notices and with
`vibex:license-policy-status=unapproved` in CycloneDX; the command then exits nonzero.
This keeps blocked evidence accurate without treating inventory generation as license
approval.

The committed outputs are:

- `desktop.cdx.json`: deterministic CycloneDX 1.6 SBOM.
- `desktop-third-party-notices.md`: package, source, declared-license, selected-
  license, and evidence inventory.

The two Zed packages whose manifests omit `license` are not silently accepted. Their
Apache-2.0 classification is tied to the reviewed `vendor/zed` submodule revision
and the SHA-256 of the package-local `LICENSE-APACHE` file. The
gpui-component Lucide icon bundle is audited from the locked upstream Cargo package
and tied to a file count and aggregate tree hash. The reused Vibex application/tray
icons have the same provenance treatment and are owned by the GPUI application
component in the SBOM. Native and installer inputs are policy-audited, included as hashed CycloneDX
components, and enumerated with version, platform, source, license, and distribution
role in the generated notices.

Expressions with alternatives pass only when the policy can select an approved
branch. Vibex itself is `AGPL-3.0-or-later`; the current graph intentionally selects
`GPL-3.0-or-later` for Zed's ztracing, ztracing_macro, and zlog packages. No other
copyleft family is accepted without a separate allowlist review.

Regenerate reviewed outputs with:

```bash
node scripts/check-licenses.mjs --write
```
