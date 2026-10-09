# AI Souls Home Arena

## 1. Scope / Trigger

Read when changing AI Souls (AI 之魂), the optional new-session preview, pixel
art, local boss combat, input lifecycle, or its appearance preference. The
feature lives in `apps/desktop/src/arena/` and cannot mutate Agent sessions,
workspaces, runtime state, or network services.

## 2. Signatures and Ownership

```rust
AppearanceUiState::show_home_arena: bool
HomeArena::new(FocusHandle) -> HomeArena
HomeArena::close(&mut self, &mut Context<Self>)
arena::home_surface(&Entity<HomeArena>, bool, impl IntoElement, &mut Window, &mut App) -> AnyElement
Arena::new(Guardian, f32) -> Arena // second argument: last displayed idle time
Arena::tick(Controls)
Arena::release_shot(Option<Vec2>)
Arena::roll(Vec2)
Arena::needs_tick() -> bool
Arena::outcome_ready() -> bool
Boss::attack_origin() -> Vec2
Geometry::world(Point<Pixels>) -> Option<Vec2>
```

- `showHomeArena` is the serialized appearance key. Legacy UI-state files
  default to `true`; an explicit `false` survives save and load through the
  existing `queue_ui_state` path.
- `HomeArena` owns one optional `ArenaView`, the composer's focus handle, and
  the last displayed `PreviewSample { guardian, time, viewport }`.
- `ArenaView` owns held input, measured canvas geometry, the entry camera,
  pause/closed flags, attempt count, and one cancellable GPUI clock.
- `combat.rs`, `guardian.rs`, and `geometry.rs` have no GPUI dependency.
  `art.rs` authors raster frames; `raster.rs` supplies integer primitives and
  rectangle merging; `palette.rs` composites game materials into the UI theme;
  `scene.rs` projects and paints them; `copy.rs` owns localized game copy.
- Existing `Unbound` key context and element IDs remain internal stable
  identifiers. User-visible naming comes from `copy::title()`.

## 3. Contracts

### Home layering and entry

- The keyboard-accessible entry Button is an absolutely positioned background.
  Its height reserves no space: enabling it must leave the introduction and
  composer at identical bounds. Foreground content occludes its pointer target.
- Preview renders only the selected guardian's idle pose, shadow, and ambient
  details. It has no archer, arrow, attack, combat entity, or simulation task.
- A six-second GPUI animation draws at most 12 FPS. All idle loops join at the
  repeat boundary. Reduced motion freezes every ambient detail; inactive-window
  animation follows the appearance preference; an open dialog freezes preview.
- Prepaint records the guardian, animation time, bounds, and actual projection
  shown to the user. Opening samples these values, creates one battle inside
  the existing home viewport, and preserves the composer entity and its draft.
  It never opens a dialog or covers workbench navigation.
- The 1.2-second `Awakening` phase starts from that same pose and palette. The
  camera interpolates from the measured preview into the letterboxed field;
  the ground and controls appear gradually, and the archer materializes before
  attacks are enabled. The root clips this transition to the home surface.
  Reduced motion skips camera travel while retaining essential combat tells.
- A pending first-focus callback must not flash a pause card or restart a battle
  already closed by navigation. Retry and guardian selection clear the home
  entry camera, so neither replays a stale transition from the home preview.
- Escape/Back emit `DismissEvent`, stop and drop the battle, restore composer
  focus, and resume preview with the last selected guardian and visual time.
  Leaving home, reopening New Session, or disabling the preference also closes
  combat without redirecting navigation focus.

### Combat and input

- Simulate at fixed 60 Hz with an approximately 60 FPS GPUI clock. Each wake
  accepts at most 100 ms of elapsed time; a suspended UI cannot catch up a
  lethal backlog. Bound projectiles to 48, effects to 48, and shockwaves to 8.
- Focus loss, window deactivation, pause, and close release held keys, mouse
  buttons, aim, charging, and recall, and cancel the clock. Combat pauses on
  deactivation regardless of the ambient-animation preference. Resume resets
  the elapsed-time baseline and retains one clock.
- WASD/arrows move; hold J/left mouse then release to shoot; hold K/right mouse
  to recall; Space rolls; P pauses; R retries; Enter continues; Escape returns.
  Keyboard attacks restore assisted aim to the core after pointer use. Mouse
  aiming uses the exact displayed projection, including letterboxing and shake.
- The archer has one arrow and one life. Drawing requires at least 0.30 seconds.
  Drawing/recalling commit the archer to standing still. Rolling cancels drawing,
  moves for 0.26 seconds, protects for 0.28 seconds, and cools down for 0.54 seconds.
- Attacks track during the first 60% of windup and commit for the final 40%.
  Telegraphs cannot deal damage. Rendered beam paths and collisions share
  `Boss::attack_origin()`; the sentinel's beam originates at its shutter/core.
  Relative-motion segment tests prevent fast bodies/projectiles tunneling
  through the archer. An arrow must intersect an exposed core to win.
- Victory/defeat clear active threats immediately; lethal impacts cannot create
  a new shockwave afterward. Results appear after one second of finite outro
  animation. After 1.25 seconds, a finished battle is inert.

### Guardian choreography

`Guardian::ALL` defines selection, progression, and the bounded art-cache order.
Each guardian must remain beatable through ordinary inputs without a damage
override. Direct selection starts a fresh attempt and clears held input.

| Guardian | Recognizable sculpture | Attack and opening |
| --- | --- | --- |
| Claude | Orange mascot, square eyes, wide arms, four legs | Left fist, right fist, double slam; chest opens after the double slam. |
| Codex | Extruded woven ring with stone limbs | Charges and leaps; a wall impact reveals the rear core. |
| Pi | P-shaped mantle and staff-bearing i figure | Cross spell and expanding pulse; the exposed seal yields only to a returning arrow. |
| OpenCode | Hollow terminal body and mechanical limbs | Slam followed by a beam; shutter opens after firing. |
| DeepSeek | Blue whale, fins, and water ripples | Three surges; the third recovery reveals its heart. |
| Copilot | Goggled helmet and layered mechanical wings | Paired volleys followed by a dive; landing exposes the core. |

### Raster and projection

- World geometry is 96 × 56 units; authored art is a 384 × 224 palette-indexed
  raster. Use integer primitives and nearest-neighbor transforms. Never use
  glyph metrics, emoji, linear texture filtering, or one GPUI element per pixel.
- One canvas paints cached ground rectangles and dynamic actors. Merge matching
  horizontal runs vertically; work/memory remain linear in raster size. Cache
  only the bounded floor, bundled logo reliefs, and hero sprite.
- Give raised bodies side/top planes, a ground shadow, height during jumps,
  grounded impact debris, and depth ordering against the archer. Idle breathing,
  windup, strike, recovery, and defeat must be distinct readable poses.
- Material ramps are authored game artwork, not UI tokens. Header/footer use
  theme roles. Preview ink blends foreground/background with restrained material
  hue in both theme modes. The initial battle palette exactly matches preview.
- Composite the preview's vertical gradient over all artwork, starting at 45%
  and reaching the exact home background at the bottom edge. Fade this same
  measured overlay away during entry. Ground colors reveal separately.
- Snap shared rectangle edges to device pixels. Fractional viewport fits must
  remain crisp and seamless. `Geometry::world` rejects letterbox padding and
  zero-sized projections; never derive aim from the window dimensions.

## 4. Validation / Error Matrix

| Condition | Required behavior |
| --- | --- |
| Legacy preference is absent / explicit false | Show entry / remove entry and preview animation |
| Preview toggles or foreground composer is clicked | No content displacement; click edits the draft |
| Entry is activated | Same displayed guardian/time/camera; no dialog; archer appears during awakening |
| Reduced motion | Static preview, no camera travel or shake; attack tells still readable |
| Shoot/roll during awakening | No attack, charge, or player movement |
| Charge released too soon / arrow already away | Do not create an arrow / a second arrow |
| Arrow intersects closed armor / misses an exposed core | No victory; retain the retrieval path |
| Pi is hit with an outgoing arrow | No victory; explain the return-arrow opening |
| Target moves after windup locks | Preserve the committed attack path |
| Focus leaves / window deactivates | Pause and clear input immediately |
| Select guardian / retry | Reset attempt state; cancel the prior clock; discard entry camera |
| Home closes before initial focus callback | Stay closed with no clock |
| Victory/defeat completes | No new threats, held input, or ongoing simulation |

## 5. Good / Base / Bad Cases

- Good: lure a charge into the boundary, roll aside, then hit the exposed rear
  core; pause safely by switching windows and continue with cleared input.
- Base: open from an idle preview, switch guardian, return with Escape, and type
  ordinary game-key letters into the original draft.
- Bad: reserve a top inset for decoration, create a combat entity during every
  render, attach global gameplay interceptors, keep a hidden clock alive, or
  paint a beam from a different origin than its collision test.

## 6. Required Tests and Checks

- Combat: awakening, minimum charge, one-arrow ownership, retrieval, armor,
  return-only seal, every opening, target lock, roll protection, one-hit defeat,
  beam origin, post-death threat cleanup, relative collision, long-run bounds,
  finite outro, and normal-input victories for every guardian.
- Raster/geometry: exact rectangle reconstruction and a bounded quad count,
  no archer in preview, identical preview/initial actor rasters, frozen reduced
  motion, joining idle loops, entry camera endpoints, pointer mapping, and
  palette continuity in light and dark themes.
- GPUI integration: pointer/keyboard entry without a dialog, Escape/Back,
  retained draft/focus, navigation during play, pending-focus cancellation,
  held input, pointer aim, pause/resume, retry/advance, all guardian selectors,
  returning to the last guardian, narrow widths, zoom, and both themes.
- Preference tests cover legacy decoding and a persisted opt-out.
- Run `cargo test -p vibex-desktop --lib arena:: --locked`,
  `cargo test -p vibex-desktop-model --locked`, formatting, and scoped Clippy.

## 7. Wrong vs Correct

```rust
// Wrong: decoration moves the user's primary work and starts an unrelated pose.
home.pt(rems(BANNER_HEIGHT_REM)).child(preview).child(composer);
let battle = Arena::new(Guardian::Claude, 0.0);

// Correct: retain layering and initialize from the actual displayed preview.
arena::home_surface(&self.home_arena, enabled, content, window, cx);
let battle = Arena::new(sample.guardian, sample.time);
```
