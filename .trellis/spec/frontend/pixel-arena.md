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
Arena::spokes() -> [(Vec2, Vec2); 6] // terrain-clipped radial rays
Boss::idle(Guardian, f32, bool) -> Boss
Boss::tentacle_joint(usize) -> Vec2
Boss::tentacle_strike(usize, f32) -> Vec2
Boss::target_locked() -> bool
Boss::pulse_origin() -> Vec2
Boss::beam_radius() -> f32
Map::move_body(Vec2, Vec2, f32) -> Vec2
Map::cover_hit(Vec2, Vec2) -> bool
Map::beam_end(Vec2, Vec2) -> Vec2
Map::break_islands(Vec2, f32) -> u8 // newly submerged platform bits
Map::scar(Vec2, f32, ScarKind)
Raster::polygon_depth(&[(f32, f32, f32)], u8, &mut DepthBuffer)
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
- `combat.rs`, `guardian.rs`, `encounters.rs`, the six `encounter_*.rs` modules,
  `map.rs`, and `geometry.rs` are independent of GPUI. `encounters.rs` owns shared
  targeting and beam behavior; each encounter module owns its choreography.
  `sculpture.rs` authors spatial meshes; `scenery.rs` authors terrain; `art.rs`
  composes actors and the `effects.rs` cues. `raster.rs` owns integer primitives,
  depth rasterization, and rectangle merging; `palette.rs` owns artwork materials;
  `scene.rs` owns projection/painting; `copy.rs` owns localized copy.
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
  result card. Workbench navigation remains available. Keyboard/mouse instructions
  and a short guardian-specific puzzle clue belong in the entry tooltip; the
  canvas has an accessible name.
- Death plays for one second, chooses a random clear spawn at a safe distance
  from the guardian, then materializes the archer for 0.7 seconds. It clears
  threats and input and starts a fresh opening delay without replaying entry.
  Broken columns and impact scars survive rebirth. Submerged platforms remain
  submerged until the next water pulse restores them.
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
  effects to 64, waves to 8, hazards to 16, and persistent terrain scars to 48.
- Focus loss, window deactivation, pause, and close release held keys, mouse
  buttons, pointer aim, charging, and recall, and cancel the clock. Combat always
  pauses on deactivation. Resume resets elapsed time and retains one clock.
- WASD/arrows move; hold J or left mouse then release to shoot; K or right mouse
  recalls; Space rolls; P pauses; Enter or a field click resumes; R respawns;
  Escape returns. Keyboard attacks restore assisted weak-point aim after pointer
  use. Pointer aim uses the last displayed camera, including shake.
- The archer has one arrow and one life. Drawing requires 0.30 seconds. Drawing
  and recalling hold position. A roll cancels drawing, moves for 0.28 seconds,
  protects for 0.30 seconds, and cools down for 0.50 seconds. A 120 ms roll buffer
  bridges the end of cooldown; releasing a charged shot during hit pause fires
  on resumption. Input cancellation clears both pending actions.
- Keyboard aim assists the current puzzle target: an exposed tendon or charged
  energy orb before the core opens. Explicit pointer aim remains exact. In the
  lagoon, swimming slows movement and prevents drawing or shooting; recall
  remains available, and dry platforms enable shooting.
- Each choreography commits its target during windup. Aerial landing targets
  must be clear terrain positions before the tell locks. Landing tells remain
  visible during flight and share the impact radius used by collision. A dash
  shows its complete committed route and final impact circle; a tentacle's tell
  marks its final landing while the limb is still rising.
- Projectile/body collisions sweep relative motion to avoid tunneling. Beam
  rendering and damage share the origin and terrain-clipped endpoint.
  Telegraphs cannot cause damage. Victory/death clear live threats before
  subsequent attacks can spawn additional hazards.
- Target lock, beam radius, pulse origin, and radial rays come from the shared
  combat geometry helpers. Do not derive a second direction from a visual spin
  or use a boss-local warning for a wave whose origin is the center of the map.
- An arrow must intersect the exposed weak point, not merely the body. Core
  coordinates are aim-plane points distinct from the grounded body footprint.
  Moving cores and interceptable projectiles use relative swept collisions;
  the earliest applicable cover/armor/joint/core contact wins. Flying arrows
  stop at cover; returning arrows pass through cover and closed armor to remain
  retrievable. Closed armor never grants a win. A tethered arrow is a distinct
  state, cannot become a second arrow, and ejects if its inhale window expires.

### Six encounters

`Guardian::ALL` is the bounded identity/art-cache order. Each encounter must be
winnable using normal movement, charging, release, recall, roll, and automatic
rebirth, without a damage override.

| Guardian | Form and choreography | Terrain and opening |
| --- | --- | --- |
| Claude | Broad orange square head, pale vertical eyes, pointed underside, six independent curling tentacles; paired sweeps, a scorch-trailing rush, and a venting stance | Cut two distinct glowing tendons during recovery or venting to open the underside. A failed opening regrows the severed limbs. Scorched paths can be cleared with the arrow. |
| Codex | Bronze and gold six-loop knot around a suspended cube; expanding pulses, radial beams, detached segments, and a committed rolling rush | Actual pillar or perimeter collision unfolds the knot for 3.8 seconds. Broken pillars leave rubble and cracks; the perimeter preserves future opportunities. |
| Pi | Black masonry glyph with a square aperture and separate right stem, floating stones, and independent fists; alternating slams, stone barrage, and glyph beam | Lure each fist onto its corresponding ground seal. Each seal stays latched for 13 seconds; both unlock the aperture for 4.4 seconds. An ordinary precise shot can finish the encounter. |
| OpenCode | Hollow dark rectangular construct with an ivory front rim and floating frames; shutter beam, frame barrage, gravity inhale, and frame slam | Shoot into the inhale and hold recall for 0.62 seconds to pull the frame apart. The core opens for 4.8 seconds and must be hit from the front. A missed pull ejects the arrow. |
| DeepSeek | Rounded blue armored whale, ivory belly, luminous cracks, two-sided fins, and raised forked tail; three breaches followed by a water pulse | Breaches destroy nearby platforms and leave delayed water columns. Shoot the tail crystal from a surviving island during descent or the 2.9-second landing recovery. The pulse restores all islands, which protect from the water wave. |
| Copilot | Teal domed helmet, blue goggles, ivory face with two dark slots, ear pods, small hands, and a chest jet reactor; energy volleys, visor beam, and jet dash | Intercept the larger orb with four visible brackets to overload the rotating shield and open the chest reactor for 4.8 seconds. Ordinary orbs and passive waiting never unlock it. |

- Keep identity readable in preview: upright forms sway within a frontal
  three-quarter view; the whale turns along its swimming route. Preserve Pi's
  glyph negative space and Copilot's goggle/face separation through articulation.
- Claude's cut collar is the same 72%-along-the-limb point returned by
  `tentacle_joint`; the sculpted curve must pass through it. Core-centered pitch
  and bank preserve each model's exact projected weak point.
- Terrain damage is combat state (`Map::broken`, `sunken`, and `scars`). Paint it
  over the immutable floor cache; submerged islands must lose all walkable art
  and ambient island rings until restored. Decorative debris never adds a
  hidden collision obstacle.

### Raster and camera

- The world is 176 × 116 units. Art uses a 704 × 592 palette-indexed raster:
  four pixels per world unit plus 128 pixels above ground for raised models.
  Painting and model hit bounds both remove this top offset. Airborne parts
  must not be cut off by a ground-only raster.
- Sculptures use rotated vertices, face culling, bevels, and discrete light
  ramps. Mesh faces use per-pixel interpolated depth in a buffer bounded to the
  model, so interwoven surfaces occlude correctly even when whole-face sorting
  cannot order them. Larger depth is nearer; raster top offsets apply equally
  to the color and depth surfaces. Thin fins/plates render both sides.
- Body, independent tentacles/fists, archer, and pillars sort by ground depth.
  Preview and combat use the same ordering, including a limb behind the torso.
  Opaque bodies draw directly into the actor raster; only submerged or departing
  bodies need a temporary raster for their visibility mask.
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
| One tendon cut / one seal latched | No core opening; the remaining puzzle action is required |
| Inhale missed or recall released | No free opening; the arrow remains recoverable |
| Ordinary energy orb intercepted | No shield overload; the bracketed orb is required |
| Platform destroyed under a charging archer | Cancel drawing, show swimming, and preserve recall |
| Water pulse after all platforms sink | Restore every platform and its visible dry surface |
| Dash in flight / radial beam release | Keep the committed landing / preserve the warning's rays |
| Window or focus changes | Pause immediately and clear input |
| Archer dies | Clear threats; random safe rebirth; no result card |
| Guardian defeated | Finite departure, restored draft/focus, hidden preview until Agent changes |

## 5. Good / Base / Bad Cases

- Good: lure a rush into a pillar, avoid its impact, and hit the revealed cube;
  a later perimeter collision remains valid if that attempt misses.
- Good: cut two exposed tendons, recover the single arrow, and strike the
  underside; or pin both fists to ground seals before aiming through the glyph.
- Base: click a roaming model, play across a scrolling field, then Escape and
  type game-key letters into the unchanged draft.
- Bad: shift the composer to reserve preview space, reset the entry pose,
  translate a flat logo for every attack, use one map for all guardians, leave
  an invisible clock running, or make visible cover disagree with a beam.

## 6. Required Tests and Checks

- Combat: entry/input exclusion, minimum draw, single arrow/retrieval, armor,
  directional weak points, distinct tendon cuts, sustained tether pulls, charged
  orb interception, latched fist seals, committed targets, swept collisions,
  cover, terrain breakage/restoration, island protection, swimming, random rebirth,
  threat cleanup, finite victory, continuous flight/turn transitions, bounded
  long runs, input buffering/cancellation, and normal-input wins for all six
  guardians across spawn seeds. Gameplay pilots must not override damage,
  position, puzzle state, or encounter timers.
- Raster/camera: distinct maps, exact quad reconstruction and bounded count,
  preview without an archer, identical preview/first combat frame, joining idle
  loops, reduced motion, moving model bounds, two-sided fins, raised geometry,
  following camera, pointer mapping, and entry projection endpoints. Pin logo
  negative space, goggles/face slots, tendon/core alignment, per-pixel occlusion
  independent of face order, final landing tells, clipped radial warnings,
  water-pulse origin, and the removal of submerged-platform art.
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
