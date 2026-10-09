# TUI Session State

## 1. Scope / Trigger

Read this contract when changing TUI new-session composition, session navigation,
runtime selection, asynchronous creation/fork/send results, optimistic
transcripts, composer text transitions, or animation repaint scheduling.
The runtime owns durable sessions; the TUI owns editor buffers and
the association between a user's action and its result.

## 2. Signatures

The relevant types live in `crates/vibex-tui/src/app.rs` and `worker.rs`:

```rust
ComposerTarget::Draft(VibexSessionId)
ComposerTarget::Session(VibexSessionId)
Effect::CreateSession { request_id, workspace_root, title, runtime }
AppMessage::SessionCreated { request_id, result }
AppMessage::SessionForked { request_id, result }
Effect::SendMessage { session_id, send_id, correlation_id, text, attachments }
AppMessage::MessageSent { session_id, send_id, result }
ComposerTicket { target, navigation_serial, runtime, text, cursor }
App::switch_composer(target: ComposerTarget)
App::observe_composer_text()
App::advance_transcript_animation() -> bool // whether a repaint is due
```

Creation reserves its `VibexSessionId` before dispatch and passes it through
`CreateAgentSessionRequest.session_id`. The worker requests deferred runtime
materialization; the authority's durable message queue waits for the Agent to
be ready. `AgentWorkflowController::select_pending_session` selects this reserved
identity using the same cache and generation rules as an existing session,
without fetching a record before creation has completed.

## 3. Contracts

- An editor has one `ComposerTarget`. Switching to another session or draft
  stores the entire buffer, including images and cursor state, under its owner.
  Returning restores that owner's buffer. A global page does not transfer a
  draft to an unrelated session.
- Each pending creation holds its own immutable first message, attachments,
  runtime selection, workspace, and send identity. A reply consumes only the
  matching entry. Creation, fork, and ordinary message results are distinct.
- A successful creation must return the reserved session ID. Unknown, duplicate,
  or mismatched results cannot consume another request's message.
- A read result updates the requested projection; it is not a navigation
  command. A late session snapshot must not leave a draft, dismiss another
  page, or change the target of an open runtime picker.
- A runtime picker captures its target when it opens. Navigation cancels that
  target, including a pending catalogue read. Apply validates the target rather
  than inferring it from whichever page or session is current at completion.
- Opening a different session immediately replaces the transcript projection
  with that session's cached data or an empty loading view. Errors cannot leave
  the previous session's messages visible under the newly selected identity.
- Every optimistic message belongs to a concrete session ID, including the
  first message of a pending creation. Its completion is identified by both
  session ID and send ID. Confirmation, timeout, or failure of one send cannot
  clear another send. Old history with the same text and attachments is not
  confirmation of a new send. The ordering barrier for an unfinished send RPC
  cannot expire with its visual projection. The request's `correlation_id`
  travels to the authoritative timeline; only that correlation and session
  acknowledge its optimistic row. Matching returned or live items also settle
  background sessions without waiting for the user to open their history.
- A second message written during creation remains queued for that reserved
  identity. It cannot run before creation is acknowledged or overtake the first
  message. Failed creation must not release queued messages to a missing record.
- Creation failure preserves that request's inputs without overwriting newer
  edits or moving a reader who has left it. Explicit retry uses a fresh attempt
  identity because the failed attempt may already have a durable record.
- Asynchronous clipboard, external-editor, completion, and workspace results
  carry the editor/draft or navigation identity that requested them. A result
  for a departed or submitted editor cannot mutate the current buffer, change
  another draft's directory, or enter New Session on its behalf.
- Completion replies also match the requested runtime and query; both insertion
  and deletion request the updated query. Concurrent external-editor invocations
  each own a distinct, exclusively created scratch file and clean up only that
  file.
- Reconnect and history reload do not replay creation or message mutations.
- A missing timeline event range triggers `App::refresh_timeline`, which asks
  `begin_session_load` for a generation-scoped snapshot. While
  `timeline_status.phase == Loading`, subsequent events cannot launch another
  load. Applying the reply preserves the current page and editor target.
- Transcript expansion is local presentation state keyed by block ID. A live
  update or final snapshot must preserve it. Group expansion opens every member;
  toggling all reasoning chooses a target state once before walking groups.
- Runtime text transitions follow the current `ComposerTarget` on both
  `Page::NewSession` and `Page::Agent`. Observe workspace text only on New Session,
  where that animated row is visible. `switch_composer` clears both observed
  labels and running transitions when the target changes, so opening another
  editor starts with its own labels.
- `observe_composer_text` defers observation while an overlay covers the page.
  A runtime change received behind the picker begins on the first frame after
  it closes. With text transitions disabled, observations still record the
  current labels; re-enabling the setting cannot replay an old change.
- `advance_transcript_animation` must request the final resting frame before
  suppressing repaints. Check whether the landing mark was waiting both before
  and after advancing the phase and pruning finished text transitions. A burst
  or transition ending is still a visible change; subsequent quiet ticks must
  not repaint. Text transitions settle within 750 ms at `ANIMATION_TICK` cadence.

## 4. Validation & Error Matrix

| Condition | Required behavior |
| --- | --- |
| Existing session load completes after entering New Session | Keep New Session and its editor/picker target. |
| A and B creations complete in either order | Send each first message exactly once to its own reserved ID. |
| Creation result repeats or has another session ID | Do not send or navigate using the unrelated result. |
| Fork completes while a new draft is pending | Keep the draft and its first message associated with creation. |
| A creation fails while another draft is being edited | Preserve both drafts; do not replace the current editor. |
| B history fails after leaving A | Show B's failure without A's transcript. |
| A send fails after sending in B | Remove only A's optimistic send; keep B's projection. |
| User types again before creation completes | Retain the second message for that session; no premature RPC. |
| Creation takes longer than the optimistic display timeout | The first real send still blocks queued follow-ups until accepted. |
| Clipboard/editor/workspace result arrives after changing targets | Do not modify the later editor or navigate away from it. |
| Background send repeats text already in history | Keep its pending identity until that particular send is confirmed. |
| A mark burst or text transition ends | Repaint its resting frame without requiring input, then stop quiet repaints. |
| An existing session's runtime changes behind its picker | Begin the composer transition only after the overlay closes. |
| A different composer opens during a transition | Clear the previous labels and transition before its first frame. |
| Text transitions are disabled | Show updated labels immediately and do not replay them when re-enabled. |

## 5. Good / Base / Bad Cases

- Good: create a Codex session, start a DeepSeek draft, and receive the older
  Codex snapshot; the DeepSeek draft remains selected and Codex is unchanged.
- Base: ordinary sequential creation still opens the new session, projects its
  first message immediately, and sends with the selected Agent and workspace.
- Bad: one global `pending_new_session.take()` supplies whichever creation
  happens to finish first, or a missing session ID matches all transcripts.
- Good: the last animated frame is replaced by a scheduled resting frame, and
  the mark's quiet interval then produces no repaints.
- Bad: pruning a completed text transition before deciding whether to repaint
  leaves its last noise characters on screen until the next input event.

## 6. Tests Required

- `tests/session_isolation.rs` exercises public intents/effects, distinct
  creation identities, per-session and new-session drafts, immediate transcript
  replacement, and retention of messages typed during creation.
- `run.rs` callback tests must call the real message application path with
  deliberately reordered success/failure results. Cover stale session loads,
  concurrent and duplicate creations, mismatched IDs, forks, failed recovery,
  picker targets, and send completion isolation.
- `tests/render.rs` must draw animation frames only when the scheduler requests
  them. Assert that Classic and Glitch restore their resting buffers, a text
  transition finishes while the mark is resting, and later quiet ticks do not
  repaint. Cover existing-session runtime switches with the picker open/closed,
  the disabled setting, and transitions cleared when changing composer targets.
- PTY scenarios own temporary interface, runtime-preference, and keymap files.
  Pinning `LC_ALL` alone is insufficient because remembered locale settings
  outrank environment detection.
- Run the TUI unit, render, contract, and PTY suites, affected shared-controller
  tests, formatting, and Clippy. Screenshots are not required to establish
  ownership or callback ordering.

## 7. Wrong vs Correct

```rust
// Wrong: the current editor and page may belong to a later user action.
let outgoing = app.pending_new_session.take();
app.open_session(reply.id);
```

```rust
// Correct: consume only the matching creation; the read never navigates.
if reply.id == request_id {
    if let Some(effect) = app.pending_send_effect(request_id) {
        worker.dispatch(effect);
    }
}
```

```rust
// Wrong: the state is quiet now, but the screen still holds a moving frame.
self.animation_phase = self.animation_phase.wrapping_add(1);
self.transitions.prune(self.animation_phase);
!self.landing_mark_waits()
```

```rust
// Correct: restore the resting frame before suppressing later repaints.
let was_waiting = self.landing_mark_waits();
self.animation_phase = self.animation_phase.wrapping_add(1);
self.transitions.prune(self.animation_phase);
!was_waiting || !self.landing_mark_waits()
```
