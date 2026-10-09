# Home ASCII Arena

## 1. Scope / Trigger

Read when changing the optional new-session vignette, Unbound combat, its
keyboard or pointer controls, or the appearance preference that enables it.
The feature lives in `apps/desktop/src/arena/` and has no runtime, network,
Agent-session, or workspace mutation capability.

## 2. Signatures

```rust
AppearanceUiState::show_home_arena: bool
HomeArena::new(FocusHandle) -> HomeArena
HomeArena::close(&mut self, &mut Context<Self>)
arena::home_surface(&Entity<HomeArena>, bool, impl IntoElement, &mut Window, &mut App) -> AnyElement
Arena::tick(Controls)
Arena::release_shot(Option<Vec2>)
Arena::roll(Vec2)
Arena::focus()
```

`showHomeArena` is the serialized appearance key. Old UI-state files default
to `true`; an explicit `false` survives a save and load. The settings command
uses the workbench's existing `queue_ui_state` persistence path.

## 3. Contracts

- The home entry is a keyboard-accessible Button with a faded character grid.
  It is an absolutely positioned background. Its height never reserves space
  in the home content flow: toggling it must leave the centered introduction
  and composer at exactly the same bounds. Foreground content occludes the
  entry's pointer target wherever they overlap.
- The preview has no combat entity or simulation task, draws at most 12 FPS,
  honors reduced motion and the inactive-animation preference, and becomes
  static behind a dialog. Disabling the setting removes it from rendering.
- The workbench retains one `HomeArena` state and observes its open/close
  changes. Opening creates one battle inside the existing home viewport,
  using its available bounds rather than the window dimensions. It does not
  open a dialog or cover navigation. The composer entity and its draft survive.
- Escape and Back emit `DismissEvent`; `HomeArena` stops and drops the battle
  and restores composer focus. Leaving home, reopening New Session, or disabling
  the preference also closes the battle without redirecting navigation focus.
- `ArenaView` owns input and its cancellable clock. A lost gameplay focus,
  inactive window, pause, or close clears held keys, mouse buttons and charging.
  Combat pauses independently of the ambient-animation preference so leaving
  the window cannot consume a life. Resuming resets the elapsed-time baseline.
- Combat uses fixed 60 Hz steps and a bounded approximately 30 FPS draw clock.
  Each wake accepts at most 100 ms of elapsed time. Projectiles and effects are
  bounded, and a completed battle does not tick.
- There is one arrow. Drawing and recalling commit the player to standing
  still; rolling cancels the draw and grants a brief invulnerability window.
  One accurate arrow hit breaks an exposed core. A sealed core is invulnerable,
  and the mirror guardian requires a returning arrow. Stillness spends focus
  to slow threats, never to bypass armor. Catching the arrow recovers focus.
- The renderer uses one canvas. Prepaint shapes rows; paint pins glyphs to grid
  cells using their UTF-8 offsets, including fallback symbols with different
  font advances. Preview ink blends the active foreground into the home
  background; combat keeps its semantic colors. Use text-only grid squares so
  emoji fallback cannot bypass tinting. A viewport-wide gradient is composited
  over all preview glyphs and sprites, reaching the exact home background at
  the bottom edge. The same letterboxed geometry maps pointer positions into
  combat coordinates.

## 4. Validation / Error Matrix

| Condition | Required behavior |
| --- | --- |
| Missing legacy preference | Show the home entry |
| Preference is false | No entry or preview animation |
| Preview is enabled / disabled | Identical centered content bounds |
| Click the composer over the preview | Focus and edit the composer; do not start a battle |
| Entry is activated | Battle fills only the home viewport; no active dialog |
| Charge released too soon / arrow already away | Do not create another arrow |
| Arrow hits sealed armor | Deflect; preserve the retrieve-and-retry path |
| Insufficient focus | Keep energy unchanged and show the recovery hint |
| Focus leaves combat / window deactivates | Pause and release all held input |
| Pointer is in letterbox padding | Do not map it to an arena position |
| Home closes before its first focus callback | Do not restart its clock |

## 5. Good / Base / Bad Cases

- Good: lure the knot guardian into the boundary, roll aside, and hit the core
  revealed by the impact; resume safely after switching windows.
- Base: return to the composer with Escape and type ordinary game-key letters.
- Bad: keep a global keyboard interceptor alive after closing, tick a hidden
  game, recreate its entity during rendering, reserve a top inset for the
  preview, or use each glyph as a separate GPUI element.

## 6. Required Tests

- Combat tests cover charging, single-arrow ownership, retrieval, armor,
  guardian-specific openings, roll protection, focus costs and long-run bounds.
  Each guardian must be beatable through normal player inputs.
- UI integration tests exercise pointer and keyboard entry activation without
  a dialog, Escape/Back, preserving and typing into the draft after closing,
  navigation during play, cancellation before the first focus callback, held
  keys, pointer controls, pause/resume, retry, and advancing to another boss.
- Geometry tests cover letterboxing and UI bounds at narrow widths, multiple
  zoom levels and both theme modes. Enabling the preview must not move the
  composer, and clicks in overlapping foreground content must not reach the
  game entry. Preference tests cover legacy decoding and a persisted opt-out.
- Run `cargo test -p vibex-desktop --lib arena:: --locked` and
  `cargo test -p vibex-desktop-model --locked`, plus scoped formatting and Clippy.

## 7. Wrong vs Correct

```rust
// Wrong: a decorative background displaces the primary task.
home.pt(rems(BANNER_HEIGHT_REM)).child(preview).child(composer);

// Correct: the home owns layering and one retained battle lifecycle.
arena::home_surface(&self.home_arena, enabled, content, window, cx);
```
