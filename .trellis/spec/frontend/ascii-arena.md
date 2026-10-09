# Home ASCII Arena

## 1. Scope / Trigger

Read when changing the optional new-session vignette, Unbound combat, its
keyboard or pointer controls, or the appearance preference that enables it.
The feature lives in `apps/desktop/src/arena/` and has no runtime, network,
Agent-session, or workspace mutation capability.

## 2. Signatures

```rust
AppearanceUiState::show_home_arena: bool
arena::banner(&mut Window, &mut App) -> AnyElement
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
  Its height and the home's reserved top inset share `BANNER_HEIGHT_REM`.
  The preview has no combat entity or simulation task, draws at most 12 FPS,
  honors reduced motion and the inactive-animation preference, and becomes
  static behind a dialog. Disabling the setting removes it from rendering.
- Opening the entry starts a boss fight directly in the standard dialog. Create
  the battle entity before the dialog builder; the builder may run every frame.
  Escape closes the dialog and restores focus through the standard overlay owner.
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
  font advances. All ink derives from the active theme. The same letterboxed
  geometry maps pointer positions into combat coordinates.

## 4. Validation / Error Matrix

| Condition | Required behavior |
| --- | --- |
| Missing legacy preference | Show the home entry |
| Preference is false | No entry or preview animation |
| Charge released too soon / arrow already away | Do not create another arrow |
| Arrow hits sealed armor | Deflect; preserve the retrieve-and-retry path |
| Insufficient focus | Keep energy unchanged and show the recovery hint |
| Focus leaves combat / window deactivates | Pause and release all held input |
| Pointer is in letterbox padding | Do not map it to an arena position |
| Dialog closes before its first focus callback | Do not restart its clock |

## 5. Good / Base / Bad Cases

- Good: lure the knot guardian into the boundary, roll aside, and hit the core
  revealed by the impact; resume safely after switching windows.
- Base: return to the composer with Escape and type ordinary game-key letters.
- Bad: keep a global keyboard interceptor alive after closing, tick a hidden
  game, recreate its entity inside the dialog builder, or use each glyph as a
  separate GPUI element.

## 6. Required Tests

- Combat tests cover charging, single-arrow ownership, retrieval, armor,
  guardian-specific openings, roll protection, focus costs and long-run bounds.
  Each guardian must be beatable through normal player inputs.
- UI integration tests exercise entry activation, Escape, typing after closing,
  held keys, pointer controls, pause/resume, retry, and advancing to another boss.
- Geometry tests cover letterboxing and UI bounds at narrow widths, multiple
  zoom levels and both theme modes. Preference tests cover legacy decoding and
  a persisted opt-out.
- Run `cargo test -p vibex-desktop --lib arena:: --locked` and
  `cargo test -p vibex-desktop-model --locked`, plus scoped formatting and Clippy.

## 7. Wrong vs Correct

```rust
// Wrong: the next render replaces both the battle and its focus target.
window.open_dialog(cx, |dialog, window, cx| {
    dialog.child(cx.new(|cx| ArenaView::new(window, cx)))
});

// Correct: the dialog retains one owner for its entire lifetime.
let view = cx.new(|cx| ArenaView::new(window, cx));
window.open_dialog(cx, move |dialog, _, _| dialog.child(view.clone()));
```
