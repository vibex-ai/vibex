# Installing the Unsigned iOS Build

`vibex-<version>-ios-unsigned.ipa` is a complete arm64 device build of the Vibex
mobile client. It is produced without an Apple account: no certificate, no
provisioning profile, no App Store Connect credentials, and no paid membership.

iOS refuses to execute code it has not signed, so this file cannot be installed
on a stock device as it stands. Whoever installs it supplies the signature by
re-signing the bundle with their own Apple ID, which is free. That split is the
whole point of the artifact: the build side stays account-free, and the install
side uses the cheapest credential Apple offers.

| Step | Apple account required |
| --- | --- |
| Producing the IPA | No |
| Installing it on a device | A free Apple ID, or a device covered by TrollStore |
| App Store or TestFlight distribution | Yes, and a paid membership |

The signed lifetime of a free-account re-signature is seven days. After that the
app stops launching until it is signed again.

## Producing the build

### From GitHub Actions

`.github/workflows/mobile-ios-unsigned.yml` builds the IPA on demand. It needs no
repository secrets and no Apple account; the macOS runner is free on a public
repository.

1. Push the workflow to the default branch. `workflow_dispatch` only lists a
   workflow that already exists there.
2. Open **Actions → Mobile iOS Unsigned IPA → Run workflow**. Leave the version
   input empty to stamp the version declared in `apps/mobile-nsh/Cargo.toml`.
3. Download `mobile-ios-unsigned-<sha>` from the finished run and unzip it.

The tagged release workflow publishes the same file as
`vibex-<version>-ios-unsigned.ipa` alongside the static library and the
simulator bundle, so a release always carries a device artifact.

### On a Mac

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
pnpm build:mobile:ios
pnpm package:mobile:ios
```

`package:mobile:ios` builds the device bundle and writes the IPA plus its
SHA-256 sidecar to `target/mobile-nsh-ios-artifacts/`. Set `VIBEX_MOBILE_VERSION`
to override the stamped version. Both steps need macOS with Xcode, because the
device link resolves the iOS SDK through `xcrun`; the script generates the
project and the static library by itself when they are missing.

## Installing the build

Every option below re-signs the bundle with the installer's own credentials.
Only install a build you got from a source you trust: re-signing replaces the
signature entirely, so it proves nothing about who produced the binary.

### Sideloadly (Windows or macOS)

Drag the IPA into Sideloadly, enter a free Apple ID, and start. The tool signs
the bundle, installs it over USB, and repeats the process when the seven-day
signature expires.

### AltStore or SideStore

Both re-sign with a free Apple ID and refresh in the background. AltStore keeps
AltServer running on a computer on the same network. SideStore refreshes
on-device after a one-time pairing setup.

A free Apple ID covers at most three installed apps at a time and about ten new
App IDs per week, so a device that already carries three sideloaded apps must
free a slot first.

### Xcode with a personal team

Open `apps/mobile-nsh/ios/Vibex.xcodeproj` (generate it with
`pnpm build:mobile:ios` first), select the `Vibex` target, set your Apple ID
under **Signing & Capabilities** with **Automatically manage signing**, and run
against the device. Xcode registers the device and issues the free profile. This
is the same flow as any other personal-team build.

### Upgrading an installed build

Every option above replaces the app in place instead of installing a second
copy — including over the GPUI client this one supersedes — because the bundle
id is unchanged (`ai.vibex.mobile`). What the old build kept in its container
survives the update: runtimes, the device identity, appearance and language,
and unsent drafts are imported on the first launch from the old files, which are
left in place. Two conditions apply:

- Keep the same bundle id. `scripts/package-ios-unsigned-ipa.sh` refuses to
  package a bundle whose `CFBundleIdentifier` is not `ai.vibex.mobile`, because
  that is the whole upgrade.
- Keep the same Apple ID (or team). A different signer is a different app to
  iOS, and the install becomes a second copy with its own data.

The nsh build declares no entitlements, exactly like the GPUI client, so a free
personal team can sign it. Adding a managed capability — App Groups most of all
— would break both the free install and the upgrade, which is why the share
handoff travels in a `vibex://share?text=…` URL instead of a shared container.

### TrollStore

TrollStore installs the IPA permanently without any account, but it relies on a
CoreTrust bug that only exists on iOS 14.0 through 16.6.1 and 17.0. The client
requires iOS 17.0 or newer, so this covers a device sitting on exactly 17.0 and
nothing else.

## Why free-account re-signing works here

Free Apple IDs cannot sign apps that request managed entitlements. The mobile
client requests none, which is what makes the unsigned artifact practical:

- Notifications are local. `apps/mobile-nsh/ios/HostBridge.swift` and the
  nsh notification center schedule them through `UNUserNotificationCenter` with
  no server push, so the app needs no `aps-environment` entitlement.
- LAN discovery only browses Bonjour. `apps/mobile-nsh/ios/LanDiscovery.swift`
  runs an `NSNetServiceBrowser`, which is covered by the `NSBonjourServices` and
  `NSLocalNetworkUsageDescription` keys already in
  `apps/mobile-nsh/ios/Resources/Info.plist` and needs no multicast entitlement.
- Neither target carries an `.entitlements` file. The share extension hands its
  text over in a `vibex://share` URL rather than a shared App Group, because the
  App Groups capability is managed and a free Apple ID cannot sign it.

Adding a capability that needs a managed entitlement would break this path, so
treat that as a deliberate trade-off rather than a detail.

## Verifying the download

```bash
shasum -a 256 -c vibex-<version>-ios-unsigned.ipa.sha256
unzip -l vibex-<version>-ios-unsigned.ipa | head
```

The archive must contain `Payload/Vibex.app/` at its root. The bundle inside
carries no `_CodeSignature` directory and no `embedded.mobileprovision`; the
packaging script refuses to emit an IPA whose bundle does carry them, so the
`unsigned` in the filename always describes the contents.
