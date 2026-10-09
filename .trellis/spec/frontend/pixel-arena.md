# AI Souls Home Arena

## 1. Scope / Trigger

Read when changing AI Souls (AI 之魂), roaming pixel guardians, local boss
combat, terrain, camera, input lifecycle, or the home appearance preference.
The feature lives in `apps/desktop/src/arena/`. It cannot mutate Agent sessions,
workspaces, runtime state, or network services.

## 2. Signatures and Ownership

```rust
AppearanceUiState::show_home_arena: bool
HomeArena::new(FocusHandle) -> HomeArena
HomeArena::select_agent(Option<&str>, &mut Context<Self>)
HomeArena::close(&mut Context<Self>)
arena::home_surface(&Entity<HomeArena>, bool, impl IntoElement, &mut Window, &mut App) -> AnyElement
Arena::from_preview(Boss, f32, u32) -> Arena // displayed pose, visual time, spawn seed
Arena::tick(Controls)
Arena::release_shot(Option<Vec2>)
Arena::roll(Vec2)
Arena::respawn()
Arena::outcome_ready() -> bool
Boss::idle(Guardian, f32, bool) -> Boss
Map::move_body(Vec2, Vec2, f32) -> Vec2
Map::cover_hit(Vec2, Vec2) -> bool
Map::beam_end(Vec2, Vec2) -> Vec2
Geometry::world(Point<Pixels>) -> Option<Vec2>
```

- `showHomeArena` is the persisted appearance key; absent values default to
  `true`. The existing appearance update path persists changes and closes a
  live battle when disabled.
- `HomeArena` owns Agent identity, the completed flag, an optional `ArenaView`,
  composer focus, and the last displayed `PreviewSample`. Catalog refresh,
  draft initialization, runtime choice, and Agent choice call
  `sync_home_arena_agent` from update handlers, never from render.
- `ArenaView` owns input, the last measured projection, pause/closed flags,
  home opacity, and one cancellable clock. Closing or navigating away releases
  the clock and all input. The existing composer entity and draft survive.
- `combat.rs`, `guardian.rs`, `encounters.rs`, `map.rs`, and `geometry.rs` are
  independent of GPUI. `sculpture.rs` authors spatial meshes; `scenery.rs`
  authors terrain; `art.rs` composes them with actors and effects. `raster.rs`
  owns integer primitives and rectangle merging, `palette.rs` owns artwork
  materials, `scene.rs` owns projection/painting, and `copy.rs` owns copy.
- `Unbound` action context and element IDs remain stable internal identifiers.
  Localized naming is AI Souls / AI 之魂.

## 3. Contracts

### Home, entry, and return

- The home is the guardian's roaming area. Preview is an absolute background
  across the entire area and reserves no layout space. Enabling it must leave
  introduction and composer bounds unchanged. Foreground controls occlude the
  guardian's pointer target where they cover it.
- Supported Agent selection determines the guardian. Switching Agent resets the
  completed flag; catalog refreshes or reselecting the same Agent do not.
  Unsupported Agents have no substitute guardian.
- Preview draws the same model and material colors as combat, with its shadow
  and idle articulation. It has no floor, archer, arrow, attack simulation, or
  gameplay clock. The 36-second roaming loop runs at most 24 FPS; reduced motion
  freezes every part. Dialogs freeze preview, and inactive windows follow the
  existing ambient-animation appearance preference.
- `scene::Entry` measures the current raster and positions a native ghost
  `Button` around the visible model, excluding its shadow. The Button owns
  pointer/keyboard activation, accessibility, hover, focus, and tooltip. Empty
  home space is not a game entry. Do not replace it with a clickable canvas.
- Prepaint records pose, visual time, bounds, and exact projection. Activation
  copies that sample into `Arena::from_preview`; it never resets the pose or
  re-samples a different animation time. Hover may redraw between pointer down
  and up, so tests must inspect the sample actually consumed by activation.
- The 1.4-second `Awakening` phase keeps the first model frame identical, fades
  home content out, reveals terrain, materializes the archer, and interpolates
  into the following camera. No gameplay input or damage runs during entry.
  Flight height remains continuous; rolling models settle through the nearest
  upright rotation. Reduced motion skips camera travel.
- Battle fills the home area and remains clipped to it. There is no dialog,
  title/header/footer, visible instruction panel, selector, control button, or
  result card. Workbench navigation remains available. Instructions belong in
  the entry tooltip and the canvas has an accessible name.
- Death plays for one second, chooses a random clear spawn at a safe distance
  from the guardian, then materializes the archer for 0.7 seconds. It clears
  threats and input and starts a fresh opening delay without replaying entry.
  Destroyed terrain remains destroyed.
- Victory has a finite 3.3-second departure: the guardian collapses, unravels,
  rises, splits, submerges, or folds according to its form. The final 0.7 seconds
  fade the field away and restore home content. `DismissEvent` drops the battle,
  restores composer focus, and hides the guardian until an Agent switch.
- Escape emits the same dismissal event but resumes preview. Navigation and
  disabling the preference close without redirecting navigation focus. A
  pending first-focus callback must not restart an already closed battle.

### Combat and input

- Simulate at fixed 60 Hz. Each clock wake accepts at most 100 ms of elapsed
  time, so suspension cannot replay a lethal backlog. Bound projectiles to 48,
  effects to 64, waves to 8, and hazards to 16.
- Focus loss, window deactivation, pause, and close release held keys, mouse
  buttons, pointer aim, charging, and recall, and cancel the clock. Combat always
  pauses on deactivation. Resume resets elapsed time and retains one clock.
- WASD/arrows move; hold J or left mouse then release to shoot; K or right mouse
  recalls; Space rolls; P pauses; Enter or a field click resumes; R respawns;
  Escape returns. Keyboard attacks restore assisted weak-point aim after pointer
  use. Pointer aim uses the last displayed camera, including shake.
- The archer has one arrow and one life. Drawing requires 0.30 seconds. Drawing
  and recalling hold position. A roll cancels drawing, moves for 0.28 seconds,
  protects for 0.30 seconds, and cools down for 0.50 seconds.
- Each choreography commits its target during windup. Aerial landing targets
  must be clear terrain positions before the tell locks. Landing tells remain
  visible during flight and share the impact radius used by collision.
- Projectile/body collisions sweep relative motion to avoid tunneling. Beam
  rendering and damage share the origin and terrain-clipped endpoint.
  Telegraphs cannot cause damage. Victory/death clear live threats before
  subsequent attacks can spawn additional hazards.
- An arrow must intersect the exposed weak point, not merely the body. Core
  coordinates are aim-plane points distinct from the grounded body footprint.
  Closed armor never grants a win; every miss retains a retrieval path.

### Six encounters

`Guardian::ALL` is the bounded identity/art-cache order. Each encounter must be
winnable using normal movement, charging, release, recall, roll, and automatic
rebirth, without a damage override.

| Guardian | Form and choreography | Terrain and opening |
| --- | --- | --- |
| Claude | Orange square mascot, four planted legs, independent reaching fists, alternating strikes and double slam | Mossy clipped garden; split ground faults follow the double slam and its chest fractures open. |
| Codex | Six interwoven strands with visible depth, rolling rushes, leaps, and rebound | Circular stone court with breakable pillars; pillar or perimeter impacts expose the rear knot. The perimeter preserves opportunities after all pillars break. |
| Pi | Hollow P mantle with an i-shaped staff bearer, crossing runes, radial pulses, and relocation | Three dais in a polygonal observatory; the exposed seal only yields to a returning arrow. |
| OpenCode | Hollow terminal block, full spatial turns, directional shutter beam, and venting | Gridded foundry with physical column cover; the shutter opens after the beam and must be hit from the front. |
| DeepSeek | Segmented whale, two-sided fins, tail motion, submerged travel, surges, and high breach | Lagoon with four islands; islands protect from water waves but not falling bodies. The throat opens after landing, with delayed geysers nearby. |
| Copilot | Goggled helmet, articulated mechanical wings, circling, paired feather salvos, and dive | Stepped terrace with sparse columns; landing folds its wings and exposes a cracked lens before a smooth takeoff. |

### Raster and camera

- The world is 176 × 116 units. Art uses a 704 × 592 palette-indexed raster:
  four pixels per world unit plus 128 pixels above ground for raised models.
  Painting and model hit bounds both remove this top offset. Airborne parts
  must not be cut off by a ground-only raster.
- Sculptures use rotated vertices, face culling, depth ordering, and discrete
  light ramps. Thin fins/plates render both sides. Full turns finish at an
  equivalent orientation; do not reset a half turn to upright.
- Body, independent hands, archer, and pillars sort by ground depth. Preview
  and combat use the same ordering, including a hand behind the torso.
- The camera smoothly follows the archer with a small guardian/look-ahead bias,
  clamps near world edges, and leaves overhead room. It does not fit the whole
  map into a fixed box. Resolve scale from rem and viewport coverage; never add
  letterboxing or derive pointer aim from window size alone.
- One canvas paints cached floor rectangles and dynamic actor rectangles.
  Merge horizontal runs vertically; work/memory stay linear in raster size.
  Caches are limited to six floors, their rectangles, and the hero sprite.
  No glyph rendering, emoji, filtered texture scaling, or element per pixel.
- Authored material ramps are raster content, not application chrome. Preview
  and combat use identical colors on light and dark home surfaces. Terrain
  opacity reveals separately; no theme-colored veil recolors the guardian.
- Snap shared rectangle edges to physical pixels and cull against the clip.
  `Geometry::world` rejects zero scale and positions outside the world.

## 4. Validation / Error Matrix

| Condition | Required behavior |
| --- | --- |
| Missing preference / explicit false | Show supported guardian / omit preview and close combat |
| Unsupported Agent | No substitute guardian |
| Empty home or composer clicked | No game entry; composer edits its existing draft |
| Visible guardian activated | Same displayed pose/palette; native pointer and keyboard paths work |
| Entry pending when home closes | No focus steal, restart, or clock |
| Reduced motion | Static preview; no camera travel, shake, or decorative particles; essential tells remain |
| Charge released early / arrow away | No arrow / no second arrow |
| Closed armor or wrong weak-point direction | No victory; arrow remains retrievable |
| All breakable pillars destroyed | Perimeter impacts still provide an opening |
| Window or focus changes | Pause immediately and clear input |
| Archer dies | Clear threats; random safe rebirth; no result card |
| Guardian defeated | Finite departure, restored draft/focus, hidden preview until Agent changes |

## 5. Good / Base / Bad Cases

- Good: lure a rush into a pillar, avoid its impact, and hit the rear opening;
  a later perimeter collision remains valid if that attempt misses.
- Base: click a roaming model, play across a scrolling field, then Escape and
  type game-key letters into the unchanged draft.
- Bad: shift the composer to reserve preview space, reset the entry pose,
  translate a flat logo for every attack, use one map for all guardians, leave
  an invisible clock running, or make visible cover disagree with a beam.

## 6. Required Tests and Checks

- Combat: entry/input exclusion, minimum draw, single arrow/retrieval, armor,
  directional and return-only weak points, committed targets, swept collisions,
  independent fists, cover, terrain breakage, island protection, random rebirth,
  threat cleanup, finite victory, continuous flight/turn transitions, bounded
  long runs, and normal-input wins for all six guardians.
- Raster/camera: distinct maps, exact quad reconstruction and bounded count,
  preview without an archer, identical preview/first combat frame, joining idle
  loops, reduced motion, moving model bounds, two-sided fins, raised geometry,
  following camera, pointer mapping, and entry projection endpoints.
- GPUI integration: native pointer/keyboard entry, no dialogs/chrome, full-home
  bounds at narrow sizes and rem zoom, composer hit testing, retained draft/focus,
  navigation and pending-focus cancellation, Agent switching, victory hiding,
  death/rebirth, held input, pointer release outside, and pause.
- Preference tests retain legacy decoding and persisted opt-out coverage.
- Run `cargo test -p vibex-desktop --lib arena:: --locked`,
  `cargo test -p vibex-desktop-model --locked`, formatting, and scoped Clippy.
  Measure dynamic rasterization and rectangle counts after artwork changes;
  CPU raster timings alone do not establish end-to-end UI frame rate.

## 7. Wrong vs Correct

```rust
// Wrong: restarting entry discards the visible roaming pose and camera.
let battle = Arena::new(Guardian::Claude, 0.0);

// Correct: the entry owns the pose actually measured and displayed.
let sample = self.preview.get();
let arena = Arena::from_preview(sample.boss, sample.time, seed);
let entry_camera = sample.viewport;
```
