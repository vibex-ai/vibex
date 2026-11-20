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
| Pairing page, link, bare-code error | `screens/pairing.rs`, `apps/mobile-nsh/src/pairing.rs`, `crates/vibex-mobile-pairing` | 4 headless page tests, 14 pairing-crate tests | headless + unit | paste, clipboard, real claim round-trip |
| Scan a QR code | `android/ScannerActivity.java`, `screens/pairing.rs`, `scan_photo` | emulator: portrait scanner with viewfinder/close/hint, camera permission, gallery decode (a picked QR was decoded; a non-Vibex code was rejected in place), host→app JNI result exercised | emulator | scan a real desktop QR on a phone |
| Find runtimes on the LAN | `android/LanDiscovery.java`, `src/discovery.rs`, zero-config session | 3 unit tests for the TXT rules; emulator: host NSD permission and Searching | emulator: permission + browsing start | discovery and approval against a desktop on the same LAN |
| Session shell and switching | `screens/session.rs`, `apps/mobile-nsh/src/session.rs` | `the_shell_opens_on_the_first_session`, `the_drawer_lists_sessions_and_switching_one_swaps_the_transcript` | headless | long lists, scroll, safe areas |
| Streaming transcript and Markdown | `screens/session.rs` | `a_branch_may_build_a_service_and_start_its_task` (streaming row), bottom-follow | headless | long answers, code blocks, links |
| Composer send | `screens/session.rs` | `the_composer_appends_what_was_sent` | headless | IME composition, multiline, paste |
| `/` commands | `screens/session.rs`, `query_commands` | `the_slash_palette_inserts_a_command` | headless | discovery against a real runtime |
| `@` references | `screens/session.rs`, `query_mentions` | `the_at_palette_inserts_a_workspace_reference` | headless | real file/delegation entries |
| Attachments (photos/files) | `screens/session.rs`, `attach_images`/`attach_files` | `a_photo_can_be_attached_and_removed` | headless (chip + removal) | platform picker, HEIC scaling, send with bytes |
| Stop a running turn | composer | `a_running_turn_can_be_stopped_from_the_composer` | headless | `interrupt` against a runtime |
| Permission card | `screens/session.rs` | `a_permission_card_is_answered_and_leaves` | headless | each runtime's permission kinds |
| Question form | `screens/session.rs` | `a_question_collects_an_answer_and_leaves` | headless | numeric/date/multi fields from a runtime |
| New session picker | `screens/new_session.rs` | `a_new_session_picks_a_workspace_and_runtime_then_opens` | headless | `list_workspaces`/`list_runtime_options` data, empty states |
| New worktree session | `screens/new_session.rs`, `create_worktree_session` | `a_new_worktree_session_can_be_started_from_the_sheet` | headless | `git_worktree_create` on a real repo, branch conflicts |
| Session rename/archive/delete | drawer actions sheet | `a_session_can_be_renamed_from_the_drawer`, `a_session_can_be_archived_from_the_drawer`, `a_session_can_be_deleted_from_the_drawer` | headless | native delete confirmation, deleting the last session |
| Terminal open/output/resize/UTF-8/title | `screens/terminal.rs`, `attach_terminal` | `the_terminal_opens_and_shows_the_programs_output`, 2 UTF-8 carry unit tests | headless + unit | typing latency, keyboard, rotation/resize, TUI apps |
| Terminal list/switch/new | terminal header sheet | `the_terminal_list_switches_between_terminals` | headless | many terminals, exited/stale states |
| Files list/read | `screens/files.rs` | `the_files_surface_lists_a_directory_and_reads_a_file` | headless | large directories, binary files |
| File edit and save | `screens/files.rs`, `save_file` | `a_files_save_follows_the_edited_state` (Save state) | headless + compile | real typing in `Editor`, revision conflict, large files |
| Git status/diff | `screens/git.rs` | `the_git_surface_lists_changes_and_shows_a_diff` | headless | large diffs, binary/renamed paths |
| Git stage/unstage | `screens/git.rs` | `a_git_change_can_be_staged_from_the_surface` | headless | staging a path the runtime refuses |
| Git commit/history/detail/revert | `screens/git.rs` | `the_git_surface_commits_and_shows_history`, `a_history_row_opens_the_commit_detail`; revert is compile-verified only | headless | commit with hooks/signing, revert confirmation |
| Theme | `theme.json`, `theme_gen` | 3 theme tests + 2 unit tests | headless | light/dark switching, dynamic color |
| Host inbound events | `src/host.rs` | 5 unit tests | unit | real scan/discovery JSON from a shell |
| Host request queue (legacy) | `src/host.rs`, `src/platform/*` | `host_requests_are_taken_once_in_order` | unit | no longer used by the app; kept for a shell that supplies its own scan/discovery |
| Device name | `src/host.rs`, platform entry points | `a_platform_device_name_is_kept_when_usable` | unit | name shown in the runtime's paired-device list |
| Platform embedding | `android/` host, `src/platform/android.rs` | compiled and run on the emulator | Android host mounts the app | iOS host (no Xcode) |
| Packaging | `scripts/build-{android,ios}.sh` | `bash -n` | syntax only | `nsh package`, signing, store upload |

## Device pass, in order

1. **Pair** with a link, with a code, by scanning the desktop's QR (Scan),
   and through Find on the same LAN (approve the six-digit code on the
   desktop); kill and reopen the app (the saved runtime stays).
2. **Send** a prompt and watch it stream; scroll up while it streams; press
   Stop mid-turn.
3. **Attach** a photo and a file, remove one chip, send with and without text.
4. Let the Agent ask for a **permission** and a **question**; answer each kind.
5. **New session** from the picker; then a **new worktree**; rename, archive
   and delete sessions from the drawer.
6. **Terminal**: type in a shell, run a TUI, rotate the phone, open the
   terminal list and create a second one.
7. **Files**: open, edit, Save; produce a conflict (edit the file from the
   desktop) and confirm the error is shown, not an overwrite.
8. **Git**: stage, commit, open history and a commit, revert an unstaged file.
9. **Theme**: switch the system appearance; check safe areas and the keyboard.
10. **Host**: camera scan, nearby discovery, and the device name in the
    runtime's paired-device list.
11. **Package** both apps and install them from the signed artifacts.

## P1 additions since the first matrix

| Area | Where it lives | Automated coverage | Still device/live |
| --- | --- | --- | --- |
| Turn grouping, tool detail, permission/question cards | `screens/session.rs` | headless turn/fold/permission tests | long transcripts, real permission kinds |
| Find, edit, older pages, transcript cache | `screens/session.rs`, `session.rs` | headless search; cache and paging compile-level | large histories, cache quota |
| Runtime picker, token use, plan/goal, queue, history, continue | `screens/session.rs`, `session.rs` | headless chips, bars, queue, history, continue | live runtime selection and queue authority |
| Composer triggers and structured mentions | `/`, `@`, `$` palettes | slash/at/dollar headless tests | live command execution and mentions |
| Scheduled send, camera, attachments | `session.rs`, Android host | compile/headless UI only | real timer, camera, plugin install |
| Sidebar folders/groups/pin/unread/order/collapse | `session.rs`, `screens/session.rs` | folder/group actions and collapse headless tests | SidebarBackend mutations on a live desktop |
| Files content search, Markdown/image preview | `screens/files.rs`, `session.rs` | file search and Markdown headless tests | large trees, binary assets |
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
- iOS still has no Xcode, so the iOS platform crate and `build-ios.sh` remain
  uncompiled and unrun.
- No reachable runtime: pairing, sending, terminal frames, Git and file writes
  are verified against fixtures and types, not a live authority.
- `apps/mobile` (the GPUI client) is untouched; the Native Shell client is a
  separate app and shares `crates/vibex-mobile-pairing` with it.
