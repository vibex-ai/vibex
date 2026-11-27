# Native Shell mobile client — acceptance checklist

What the `apps/mobile-nsh` client does, what the automated tests cover, and
what only a device can confirm. The code-side migration is finished; every
row marked **device** still needs the host shells (Kotlin/Swift) or a real
runtime to be exercised.

## How to run the checks

```sh
cargo test -p vibex-mobile-nsh -p vibex-mobile-pairing
cargo clippy -p vibex-nsh-ui -p vibex-mobile-nsh --all-targets -- -D warnings
cargo fmt --check -p vibex-nsh-ui -p vibex-mobile-nsh
node scripts/check-licenses.mjs
```

These run on Linux. The Android and iOS targets, the packaging scripts and
anything that needs a runtime or a phone are listed at the bottom.

## Coverage matrix

| Area | Where it lives | Automated coverage | Verified on this machine | Confirm on device |
| --- | --- | --- | --- | --- |
| Pairing page, link, bare-code error | `screens/pairing.rs`, `apps/mobile-nsh/src/pairing.rs`, `crates/vibex-mobile-pairing` | 8 headless page tests, 14 pairing-crate tests | headless + unit | paste, clipboard, real claim round-trip |
| Scan a QR code | `android/ScannerActivity.java`, `ios/Vibex/PairingScannerViewController.swift`, `screens/pairing.rs`, `scan_photo` | emulator: portrait scanner with viewfinder/close/hint, camera permission, gallery decode (a picked QR was decoded; a non-Vibex code was rejected in place), host→app JNI result exercised; iOS: Swift syntax, the C ABI and the request branches | emulator (Android) | scan a real desktop QR on a phone, on both shells |
| Find runtimes on the LAN | `android/LanDiscovery.java`, `ios/Vibex/LanDiscovery.swift`, `src/discovery.rs`, zero-config session | 3 unit tests for the TXT rules; emulator: host NSD permission, Searching, empty state with Try Again, verification code and countdown; iOS: syntax + ABI only | emulator: permission + browsing start + empty state | discovery and approval against a desktop on the same LAN, on both shells |
| Runtime actions, rename, details, remove | `screens/pairing.rs`, `apps/mobile-nsh/src/pairing.rs` | none headless (the sheet needs a saved credential) | emulator: actions sheet, rename dialog, details page, remove confirmation, back closes the top surface | rename and remove a real runtime, check the details against the desktop |
| Three-page session shell | `screens/session/pager.rs`, `screens/session/workbench.rs` | horizontal and vertical gestures, modal-first back, retained draft/preview, terminal-mode routing, 320-point viewport | headless | real touch arbitration, system-edge behavior, safe areas, IME |
| Streaming transcript and Markdown | `screens/session.rs` | `a_branch_may_build_a_service_and_start_its_task` (streaming row), bottom-follow | headless | long answers, code blocks, links |
| Composer send | `screens/session.rs` | `the_composer_appends_what_was_sent` | headless | IME composition, multiline, paste |
| `/` commands | `screens/session.rs`, `query_commands` | `the_slash_palette_inserts_a_command` | headless | discovery against a real runtime |
| `@` references | `screens/session.rs`, `query_mentions` | `the_at_palette_inserts_a_workspace_reference` | headless | real file/delegation entries |
| Attachments (photos/files) | `screens/session.rs`, `attach_images`/`attach_files` | `a_photo_can_be_attached_and_removed` | headless (chip + removal) | platform picker, HEIC scaling, send with bytes |
| Stop a running turn | composer | `a_running_turn_can_be_stopped_from_the_composer` | headless | `interrupt` against a runtime |
| Permission card | `screens/session.rs` | `a_permission_card_is_answered_and_leaves` | headless | each runtime's permission kinds |
| Question form | `screens/session.rs` | `a_question_collects_an_answer_and_leaves` | headless | numeric/date/multi fields from a runtime |
| New session picker | `screens/new_session.rs` | `a_new_session_picks_a_workspace_and_runtime_then_opens` | headless | `list_workspaces`/`list_runtime_options` data, empty states |
| New worktree session | `screens/new_session.rs`, `create_worktree_session` | `a_new_worktree_session_can_be_started_from_the_creation_page` | headless | `git_worktree_create` on a real repo, branch conflicts |
| Session rename/archive/delete | drawer actions sheet | `a_session_can_be_renamed_from_the_drawer`, `a_session_can_be_archived_from_the_drawer`, `a_session_can_be_deleted_from_the_drawer` | headless | native delete confirmation, deleting the last session |
| Terminal output and UTF-8 | `screens/terminal.rs`, `attach_terminal` | `a_character_split_across_frames_is_not_mangled`, `the_terminal_opens_and_shows_the_programs_output` | headless: frames reach the emulator as the PTY's own bytes, so a character split across reads stays one character | latency, rotation, live PTY/TUI |
| Terminal keyboard input | `screens/terminal.rs` | `the_terminal_keyboard_sends_native_edits_and_return`, `tapping_the_terminal_grid_takes_the_keyboard`, `native_input_edits_produce_pty_bytes` | headless + unit: a tap on the grid alone raises the keyboard, then outbound text, DEL, CR, Unicode edits | phone IME composition and a command round-trip against a paired PTY |
| Terminal tabs/switch/close | terminal header | `terminal_tabs_switch_and_can_close_a_terminal` | headless | many terminals, exited/stale states |
| Terminal look and feel | `screens/terminal.rs`, `theme.rs` | `the_terminal_is_drawn_as_a_terminal` | headless: monospaced cells at a terminal's size, on the terminal's own ground and chrome | the real device's font metrics and appearance switch |
| One terminal per workspace | `open_terminal`, `terminal_list_for_workspace` | `only_the_newest_ghost_terminal_is_restored` (desktop runtime and remote) | unit + headless: opening the surface returns to the shell already open, and a `Running` row with no PTY behind it is retired instead of restored again | a runtime killed mid-shell, several workspaces open at once |
| Files list/read | `screens/files.rs` | `the_files_surface_lists_a_directory_and_reads_a_file` | headless | large directories, binary files |
| File search and read-only viewer | `screens/files.rs` | `the_file_browser_searches_content`, `the_read_only_file_viewer_can_find_in_source` | headless | live search, search-result positioning, large files |
| Git status/diff | `screens/git.rs` | `the_git_surface_lists_changes_and_shows_a_diff` | headless | large diffs, binary/renamed paths |
| Git stage/unstage | `screens/git.rs` | `a_git_change_can_be_staged_from_the_surface` | headless | staging a path the runtime refuses |
| Git commit/history/detail/revert | `screens/git.rs` | `the_git_surface_commits_and_shows_history`, `a_history_row_opens_the_commit_detail`; revert is compile-verified only | headless | commit with hooks/signing, revert confirmation |
| Theme | `theme.json`, `theme_gen` | 3 theme tests + 2 unit tests | headless | light/dark switching, dynamic color |
| Host inbound events | `src/host.rs` | 5 unit tests | unit | real scan/discovery JSON from a shell |
| Host request queue (legacy) | `src/host.rs`, `src/platform/*` | `host_requests_are_taken_once_in_order` | unit | no longer used by the app; kept for a shell that supplies its own scan/discovery |
| Device name | `src/host.rs`, platform entry points | `a_platform_device_name_is_kept_when_usable` | unit | name shown in the runtime's paired-device list |
| Platform embedding | `android/` host, `src/platform/android.rs` | compiled and run on the emulator | Android host mounts the app | iOS host (no Xcode) |
| iOS host shell | `ios/Vibex`, `ios/VibexShare`, `src/platform/ios.rs` | Swift syntax; `cargo check`/`clippy` for `aarch64-apple-ios`; `tests/ios_host_abi.rs` (header = exports = Swift calls, request branches, share handoff, bundle id) | none: no Xcode here | mount, rotate, background/foreground, signing |
| iOS camera attachment | `ios/Vibex/CameraCapture.swift`, `attach_camera_photo` | ABI + branch checks | none | take a photo into the composer, HEIC/JPEG, permission refusal |
| iOS share intake | `ios/VibexShare`, `ios/Vibex/ShareIntake.swift` | syntax; both sides name the same URL scheme, host and query keys, and no entitlement is declared | none | share text/a link from Safari and Notes, cold start included, long text via the pasteboard |
| iOS quick action | `ios/Vibex/AppDelegate.swift` | branch check (`vibex_mobile_nsh_new_session`) | none | long-press the icon and start a new session |
| iOS background | `ios/Vibex/HostBridge.swift`, `take_connection_service` | ABI + branch checks | none | backgrounding mid-turn, and what the app shows when the assertion expires |
| iOS notifications | nsh's `notifications`, `vibex_mobile_nsh_install_notifications` | install called before `attach` (checked) | none | permission prompt, a press from a cold start opens the session |
| Packaging | `scripts/build-{android,ios}-host.sh`, `scripts/package-ios-unsigned-ipa.sh` | `bash -n`; `scripts/check-upgrade.mjs` pins the application id, versionCode scheme, bundle ids and legacy file names | Android release APK built and inspected locally | signing, store upload, installer re-sign |
| Upgrade over the GPUI client | `src/migration.rs`, `android/app/build.gradle`, `ios/project.yml` | 8 unit tests for the two data layouts and the settings/draft/host imports; the upgrade checker; **emulator: the old 0.1.0-rc.10 APK installed, then the new one installed over it (same id, versionCode 100510 → 109999), first launch imported the old settings and draft into `storage.json`, a second launch after restoring the old file imported nothing** | emulator (Android) | the same on a phone with a real release key; the iOS path on a Mac |

## Device pass, in order

1. **Pair** with a link, with a code, by scanning the desktop's QR (Scan),
   and through Find on the same LAN (approve the six-digit code on the
   desktop); kill and reopen the app (the saved runtime stays).
2. **Send** a prompt and watch it stream; scroll up while it streams; press
   Stop mid-turn.
3. **Attach** a photo and a file, remove one chip, send with and without text.
4. Let the Agent ask for a **permission** and a **question**; answer each kind.
5. **Left drawer**: drag it open and shut and watch the conversation move
   aside as a card; tap the strip of the card to return. Search, long-press a
   row of each kind for its menu, switch regular/worktree views from the
   header menu, select a project with collapsed children, reorder a row by its
   grip while selecting, and start a new session, including a new worktree.
6. **Terminal**: type in a shell, run a TUI, rotate the phone, and create,
   switch, and close terminal tabs.
7. **Files**: expand and collapse directories, search contents, open a result
   at its line, and search the read-only source viewer.
8. **Git**: select files and directories, stage, commit, open a commit, and
   confirm rollback only affects the selected paths.
9. **Theme**: switch the system appearance; check safe areas and the keyboard.
10. **Host**: camera scan, nearby discovery, and the device name in the
    runtime's paired-device list.
11. **Upgrade**: install the previous release (the GPUI client or an earlier
    nsh build with the same id), then install this one over it — no uninstall
    prompt, no second icon — and check that the paired runtime, the appearance,
    the language and an unsent draft are still there on first launch.
12. **Package** both apps and install them from the signed artifacts.

## P1 additions since the first matrix

| Area | Where it lives | Automated coverage | Still device/live |
| --- | --- | --- | --- |
| Turn grouping, tool detail, permission/question cards | `screens/session.rs` | headless turn/fold/permission tests | long transcripts, real permission kinds |
| Find, edit, older pages, transcript cache | `screens/session.rs`, `session.rs` | headless search; cache and paging compile-level | large histories, cache quota |
| Runtime picker, token use, plan/goal, queue, history, continue | `screens/session.rs`, `session.rs` | headless chips, bars, queue, history, continue; queue projection unit test | live runtime selection, queue and scheduled authority |
| Composer triggers and structured mentions | `/`, `@`, `$` palettes | slash/at/dollar headless tests | live command execution and mentions |
| Scheduled send, camera, attachments | `session.rs`, Android host | compile/headless UI only; schedule now uses the Sidebar mutation | real runtime timer, camera, plugin install |
| Sidebar hierarchy and operations | `sidebar.rs`, `screens/session/tree.rs` | shared ordering and drop-scope unit tests; folder/group, search, project picker and batch selection headless tests | SidebarBackend mutations on a live runtime |
| Files tree, search and previews | `screens/files.rs`, `session.rs` | in-place expansion/collapse, content search, Markdown, read-only find and preview retained across tabs | large trees, binary assets and live file revisions |
| Git branch checkout, Worktree lifecycle | `screens/git.rs`, shared backend | compile + list tests | checkout, merge/discard/conflict on a real repo |
| Team, share-in, shortcuts, APK update | `screens/team.rs`, Android host | team sheet headless; host code compile | real device/network/installer |
| Settings pages | `screens/settings.rs` | compile only | device toggles and notification permission |

## Environment limits of this checkout

- Android was built here: `nsh` 0.6.0, SDK platform/build-tools 35, NDK 27.2,
  and a debug-signed universal APK. On an API 35 emulator the pairing page,
  the Link sheet, the custom host's portrait scanner (viewfinder, hint,
  gallery pick, and the "not a Vibex pairing code" path), the host→app scan
  result, the photo picker's QR decode and the Find sheet (browsing started)
  were exercised; the universal APK
  (arm64-v8a, armeabi-v7a, x86_64) installed and launched there too. The
  custom host was rebuilt afterwards with `nsh` 0.6.1 (crates, embed AAR and
  all three ABIs) and still packages; the emulator exercises above were run on
  0.6.0. A real QR decode and a desktop approving the zero-config code still
  need a phone and a desktop on one LAN.
- iOS still has no Xcode: `ios/` (the Swift host and its share extension),
  `build-ios-host.sh` and the packaging path have never been built. What is
  covered on this machine is the Rust iOS target (`cargo check`/`clippy` for
  `aarch64-apple-ios`), the Swift syntax (tree-sitter), and the C ABI between
  the two (`tests/ios_host_abi.rs`). The first run on a Mac is expected to
  find Swift/API mismatches that nothing here can see.
- No reachable runtime: pairing, sending, terminal frames, Git and file reads
  are verified against fixtures and types, not a live authority.
- `apps/mobile` (the GPUI client) is untouched; the Native Shell client is a
  separate app and shares `crates/vibex-mobile-pairing` with it.
