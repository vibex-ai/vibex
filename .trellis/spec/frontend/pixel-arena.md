# AI Souls Home Arena

## 1. Scope / Trigger

Read when changing AI Souls (AI 之魂), roaming pixel guardians, local boss
combat, terrain, camera, input lifecycle, the reward companion that a won
encounter unlocks on a session composer, or the home appearance preference.
The feature lives in `apps/desktop/src/arena/` and the arena section of the
desktop UI state. It cannot mutate Agent sessions, workspaces, runtime state,
or network services.

## 2. Signatures and Ownership

```rust
AppearanceUiState::show_home_arena: bool
ArenaUiState::unlocked: BTreeSet<String> // canonical Agent keys of won encounters
ArenaUiState::best_times: BTreeMap<String, u64> // fastest clear per key, in ms
ArenaUiState::is_unlocked(&str) -> bool
ArenaUiState::unlock(&str) -> bool // true only when the victory was new
ArenaUiState::best_time(&str) -> Option<u64>
ArenaUiState::record_best(&str, u64) -> bool // true only when it beat the stored record
Guardian::key() -> &'static str // canonical Agent id an unlock is stored under
arena::reward_key(&str) -> Option<&'static str> // Agent id, aliases included, to that key
arena::Awakened { key: &'static str, clear_ms: u64 } // emitted by either duel surface on victory
Companion::new() -> Companion
arena::companion_lane(&Entity<Companion>, Option<&str>, Option<u64>, bool, Option<FocusHandle>, bool, &mut App) -> Option<AnyElement>
arena::session_surface(&Entity<Companion>, impl IntoElement, &mut App) -> AnyElement
art::companion_box(Guardian) -> (i32, i32, i32, i32) // crop window around the ground anchor
art::companion_height(Guardian) -> i32 // median lane pose height, in raster pixels
art::companion(&Boss, f32, bool) -> Raster // one idle frame cropped to that window
Raster::resample(usize, usize) -> Raster // coarser grid, one ink per cell
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
Arena::clear_millis() -> Option<u64> // the winning run's time; None until victory
Arena::brazier_lit() -> bool
Boss::idle(Guardian, f32, bool) -> Boss // each form's own gait on the shared home loop
Boss::lane(Guardian, f32) -> Boss // the companion's pose on that guardian's own lane itinerary
gait::lane_lap(Guardian) -> f32 // seconds one lane itinerary takes; distinct per guardian
Boss::articulated(f32, f32, f32) -> Vec2 // model point after pivoted tilt
Boss::body_center() -> Vec2
Boss::target_locked() -> bool
Boss::cube_shielded() -> bool
Boss::guard_point(usize) -> (Vec2, f32)
Map::move_body(Vec2, Vec2, f32) -> Vec2
Map::walk(Vec2, Vec2, f32) -> Vec2 // the archer: also blocks, dais, bowl
Map::standable(Vec2, f32) -> bool
Map::cover_hit(Vec2, Vec2) -> bool
Map::beam_end(Vec2, Vec2) -> Vec2 // dash routes end at the rim or a column
Map::break_islands(Vec2, f32) -> u8 // newly submerged floe bits
Map::drop_block(Vec2); Map::crush_blocks(Vec2, f32) -> Vec<Vec2>
Map::scar(Vec2, f32, ScarKind)
Raster::polygon_depth(&[(f32, f32, f32)], u8, &mut DepthBuffer)
Raster::outline(&DepthBuffer, u8) // one-pixel model silhouette
Geometry::world(Point<Pixels>) -> Option<Vec2>
```

- `showHomeArena` is the persisted appearance key; absent values default to
  `true`. The existing appearance update path persists changes and closes a
  live battle when disabled.
- `arena.unlocked` is the persisted record of won encounters, keyed by the
  canonical Agent id rather than by an alias or a display name. A victory only
  ever adds to it: replaying a duel, and losing one, never remove an unlock.
  Both duel surfaces emit `Awakened`, and the workbench writes the unlock
  through one path.
- `arena.bestTimes` is the persisted pace to beat, keyed by the same canonical
  Agent id and stored in milliseconds. It only ever improves: a slower or equal
  win is not a change and does not rewrite the state file. Absent values decode
  to "no record yet". `Awakened.clear_ms` carries the winning run, so the
  workbench can both unlock and record through one event.
- `HomeArena` owns Agent identity, the completed flag, an optional `ArenaView`,
  composer focus, and the last displayed `PreviewSample`. Catalog refresh,
  draft initialization, runtime choice, and Agent choice call
  `sync_home_arena_agent` from update handlers, never from render.
- `Companion` owns the unlocked reward on a session: the Agent whose guardian
  rides that composer, its roaming/leaving stage, its frame cache, the lane
  width measured by the last prepaint, the pose it last painted, and the duel it
  opened. The composer drives it from the render path through
  `arena::companion_lane`, which never notifies: every change it makes is read
  back by the same frame that asked for it. The exit's retire task carries the
  generation it was scheduled for, so a stage that replaced it is never retired
  by a stale timer.
- `ArenaView` owns input, the last measured projection, pause/closed flags,
  home opacity, and one cancellable clock. Closing or navigating away releases
  the clock and all input. The existing composer entity and draft survive.
  Both surfaces drive the same view; only one of them can hold it at a time.
- `combat.rs`, `guardian.rs`, `encounters.rs`, the six `encounter_*.rs` modules,
  `map.rs`, and `geometry.rs` are independent of GPUI. `encounters.rs` owns shared
  targeting, ballistic arcs and rolling/dash collisions; each encounter module
  owns its choreography.
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
  gameplay clock. The 36-second roaming loop runs at most 30 FPS; reduced motion
  freezes every part. Each form roams in its own gait: the spark hops with
  squash and spin, the knot rolls on its rim, the glyph strides on its stems,
  the chest hops and chomps, the whale swims and breaches, and the helmet
  strafes, banks and barrel-rolls. Roaming spans most of the home while keeping the
  whole model inside it, and every loop starts in the upper half, clear of the
  composer. Dialogs freeze preview, and inactive windows follow the
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
  Flight height remains continuous; rolls, banks, squash and an open lid settle
  through the nearest upright pose. Reduced motion skips camera travel.
- Battle fills the home area and remains clipped to it. There is no dialog,
  header, instruction panel, selector, or result card. Only three overlays sit
  on the field: a pixel clock in the top-left corner (see below), a faint single
  line at the bottom with the game name and controls (`copy::battle_hint`), and
  a small pixel-drawn exit mark in the top-right corner that leaves like Escape
  and never fires an arrow. None gives a guardian-specific clue; players
  discover each encounter themselves. The roaming entry has an accessible label
  but no hover tooltip. Workbench navigation remains available; the canvas has
  an accessible name.
- The clock's live face shows `Arena::run_time`, the duel's own clock, not the
  per-life `elapsed` counter: it starts as the intro hands control to the
  archer and keeps counting through a death and its rebirth, because an attempt
  is the whole duel. It freezes on victory and rounds up to whole milliseconds,
  so a run never claims a tenth it did not spend and a barely-started win still
  records a positive time. `Arena::clear_millis` is the only source a victory's
  record is taken from.
- The record is shown beneath the live face as the localized "Best …" line, or
  "New best!" in the record ink on the victory that beat it. Before the first
  win the line reads "No best yet". The clock derives the displayed record from
  the pace it was opened with, so a slower replay keeps that pace on screen and
  never replaces it, while a faster win shows itself. `HomeArena` retains the
  win's time and the workbench persists it, so the next duel opens with it.
- The clock face is authored raster content, not a font: a 3 × 5 cell face per
  digit, a one-cell colon, and a one-cell period for the tenths, painted as
  merged rectangles on whole pixels (`clock::clock_overlay`). The face sits on a
  stepped, bevelled pixel plate — dark navy ground, one lit edge and one shaded
  edge, corners notched by one cell — so ivory digits never land directly on a
  bright stone floor. The plate is drawn as cells from the bounds it is given,
  not from an assumed height, so a localized pace line of any length still has
  its ground and its frame.
- Contrast and legibility rules for the face: the cell is large enough
  (`clock::CELL`) that a 3 × 5 digit reads at arm's length; the drop shadow is
  painted as one pass under the whole face, never cell by cell, because a
  per-cell offset shadow closes counters and joins neighbours into a smear; the
  tenths are drawn in a dimmer ink than minutes and seconds so the steady part
  of the time reads first while the tenths still animate. The overlay is
  pointer-transparent and never takes an aim from the field, exposes its numbers
  as one accessible label, and fades with the field's reveal so it cannot
  outlive the encounter.
- The workbench owns the record. It reads `ArenaUiState::best_time` when it
  syncs the home Agent (`select_agent(id, best_ms)`) and when it renders a
  session composer's lane, and hands the value down; the arena never reads UI
  state from a render path.
- Death plays for one second, chooses a random clear spawn at a safe distance
  from the guardian, then materializes the archer for 0.7 seconds. It clears
  threats and input and starts a fresh opening delay without replaying entry.
  Broken columns and impact scars survive rebirth; spat blocks and split sparks
  are cleared and the spark regains its full form. Sunken floes stay sunken
  until the next discharge restores them.
- Victory has a finite 3.3-second departure: the guardian collapses, unravels,
  rises, splits, submerges, or folds according to its form. The final 0.7 seconds
  fade the field away and restore home content. `DismissEvent` drops the battle,
  restores composer focus, and hides the guardian until an Agent switch.
- Escape emits the same dismissal event but resumes preview. Navigation and
  disabling the preference close without redirecting navigation focus. A
  pending first-focus callback must not restart an already closed battle.

### Reward companion

- A victory is permanent. The defeated guardian's reward is the companion that
  rides the composer of every session whose Agent resolves to that key; it is
  neither time-limited nor repeatable-earned, and losing later never removes it.
- The lane is the composer's own strip: the first row of the composer column,
  on top of everything it stacks above the input box (error, statuses, status
  line, plan, queue, goal), inside the composer's content width, clipped, and
  absent from the tree entirely when no companion is mounted. It reserves
  nothing when it is not shown, and it must never overlap the timeline or any
  composer row. Its height is the guardian's fitted crop, at most `MAX_LANE`.
- The companion is drawn from `art::companion`: one idle frame cropped to
  `art::companion_box` around the model's ground anchor, without the arena's
  ground shadow. The window holds every pose of the idle loop, reduced pose
  included, and every pose of the guardian's lane itinerary, so no frame is
  ever sliced. The model is sized so its median lane pose
  (`art::companion_height`) stands `MODEL_HEIGHT` screen pixels tall, unless
  the window would then exceed `MAX_LANE`. The crop is resampled to one ink per
  screen pixel before it is painted: quads at a fraction of a screen pixel snap
  onto each other or vanish and the model dissolves into noise. Cells are
  painted as merged rectangles on whole pixels and rebuilt only when the
  quantized pose changes. There is no mirroring: the sculpture turns to face
  its itinerary's heading.
- Mounting follows the turn. A composer whose Agent has an unlocked guardian and
  whose turn is running shows the lane. The companion does not reuse the home
  route: side-on, that route is the same even sway on the same 36 s loop for
  every guardian. `Boss::lane` (`arena/gait.rs`) gives each guardian an
  itinerary of its own — stops along the lane (-1..1, `LANE_REACH` either side
  of the centre), rests, and the stretches between them — on a lap of its own
  length, travelled in that form's gait: the spark hops in bursts and rolls the
  way it hops, the knot cruises with momentum, rocks to a stop and pivots, Pi
  strides with stems that settle together whenever it stops and looks about,
  the chest lumbers and peeks out of its lid during its long sit, the whale
  never stops — it swims at one steady speed, breaches on the outbound pass
  only, and swims each end's U-turn as a half circle at that same speed, never
  surging or gliding — and Copilot darts between uneven hovering spots with one
  barrel roll per lap. Every itinerary averages under 0.12 lanes a second, so
  no guardian sprints. The pose clock is monotonic and every itinerary closes
  on itself (spin, facing, and stems included), so the lap's seam never cuts
  back to a first frame mid-lane.
- Entrances and exits are each guardian's own too (`gait::shot` before lane
  time 0, `gait::leaving`, durations `gait::entrance` / `gait::exit`). A turn
  that starts sets the pose clock's origin one entrance ahead, so the entrance
  plays on the same monotonic clock: the spark falls in from above and bounces,
  the knot rolls in past the left end, Pi climbs out of the composer one stem at
  a time, the chest pops out lid open, the whale surfaces nose first, and
  Copilot swoops in from above the left end with a roll. Each starts wholly out
  of sight and lands exactly on the itinerary's first pose. When the turn ends
  the companion leaves from exactly the pose it held: the spark crouches and
  flares up out of the lane, the knot pivots and rolls off the nearer end, Pi
  gathers its stems and steps down into the composer, the chest gapes and jumps
  in, the whale noses down and sounds tail-up, and Copilot banks and climbs away
  past the nearer end. A `Shot` carries the pose plus `slide` (share of the way
  past a lane end) and `lift` (lane heights above or below), so the lane's clip
  hides what has gone; a turn that ends mid-entrance hands the entrance's offset
  over to the exit. The lane is removed once the exit ends, and the composer
  returns to its own height. A stage replaced during an exit is never retired by
  the stale exit's timer. The companion's clocks read
  `BackgroundExecutor::now()`, so tests drive entrances and exits on the fake
  clock.
- Double-clicking the companion opens the same duel as the home page, over the
  session page: the session's elements fade out, the arena fills the column
  page, and Escape returns. `Arena::from_preview` still starts from the pose
  actually painted, so the model grows out of the lane it stood in. A victory
  there re-emits `Awakened`; the unlock it was reached through stays unlocked.
- The companion follows the composer it is rendered for. Switching to another
  Agent hands the lane to that Agent's guardian at once, with neither an exit
  nor an entrance — the turn neither ended nor just started — and ends a duel
  that was opened over the previous session rather than letting it follow the
  reader. Reduced motion removes the effect entirely: no lane, no badge, no
  entrance or exit, while the companion's stage is still retired.
- Nothing here mutates a session, draft, attachment, queue, or runtime state.
  The companion is a render-time passenger of a composer.

### Combat and input

- Simulate at fixed 120 Hz on an 8 ms clock. Each clock wake accepts at most
  100 ms of elapsed time, so suspension cannot replay a lethal backlog. Bound
  projectiles to 48, effects to 64, waves to 8, hazards to 16, spat blocks to
  10, and persistent terrain scars to 48. Old slime yields its hazard slot
  before a live threat is ever dropped.
- Focus loss, window deactivation, pause, and close release held keys, mouse
  buttons, pointer aim, charging, and recall, and cancel the clock. Combat always
  pauses on deactivation. Resume resets elapsed time and retains one clock.
- WASD/arrows move; hold J or left mouse then release to shoot; K or right mouse
  recalls; Space rolls and holding it sprints (×1.45); P pauses; Enter or a field click resumes; R respawns;
  Escape returns. Keyboard attacks restore assisted weak-point aim after pointer
  use. Pointer aim uses the last displayed camera, including shake.
- The archer has one arrow and one life and walks at 30 units per second.
  Drawing requires 0.20 seconds (capped at 0.8); arrows fly at 100 + 45 ×
  charge. Drawing and recalling hold position. A roll cancels drawing, moves
  for 0.24 seconds, protects for 0.26 seconds, and cools down for 0.32 seconds. A 120 ms roll buffer
  bridges the end of cooldown; releasing a charged shot during hit pause fires
  on resumption. Input cancellation clears both pending actions.
- Keyboard aim targets the open weak point, the lit brazier flame before the
  knot melts, and otherwise the body. Explicit pointer aim remains exact. In
  the lagoon, swimming slows movement and prevents rolling, drawing and
  shooting; recall remains available, and dry floes enable shooting. Ember
  slime slows walking to 55% and never harms. An energy orb knocks the archer
  back; meeting a wall while the push is still fast kills.
- Each choreography commits its target during windup. Aerial landing targets
  must be clear terrain positions before the tell locks. Landing tells remain
  visible during flight and share the impact radius used by collision; a
  dashed ring closes onto it. A roll or dash shows its complete committed route
  to the rim or column it will strike. A vanished chest casts the shadow of its
  return, and a breach marks its landing before the whale surfaces.
- Projectile/body collisions sweep relative motion to avoid tunneling.
  Telegraphs and falling shadows cannot cause damage. Victory/death clear live threats before
  subsequent attacks can spawn additional hazards.
- Target lock, impact radius, routes and weak-point anchors come from the
  shared combat helpers. Do not derive a second direction from a visual spin,
  or a weak point from art that collision does not share.
- An arrow must intersect the exposed weak point, not merely the body. Core
  coordinates are aim-plane points distinct from the grounded body footprint.
  Moving cores and interceptable projectiles use relative swept collisions;
  the earliest applicable cover/armor/spark/core contact wins. Flying arrows
  stop at cover and at closed weak points; returning arrows pass through cover
  and armor to remain retrievable, though they still split a carrier spark or
  shatter a split one. Closed armor never grants a win. An arrow that passes
  through the lit brazier burns for four seconds.

### Encounters

`Guardian::ALL` is the bounded identity/art-cache order. Each encounter must be
winnable using normal movement, charging, release, recall, roll, and automatic
rebirth, without a damage override.

| Guardian | Form and choreography | Terrain and opening |
| --- | --- | --- |
| Claude | Terracotta spark of twelve uneven, club-ended rays around a glowing heart; relentless hops whose landings shake the ground and leave slowing ember slime | Every hit splits it: the smaller heart carrier recoils while a heartless spark bursts away and hunts on its own rhythm. After four splits the heart lies bare and one hit wins. Split sparks shatter when hit. |
| Codex | Bronze six-loop knot around a cube; three rolling rushes, then a heavy leap with a shock ring | Four forge plates surround a central brazier that burns only while the knot presses a plate it landed on. A burning arrow melts the knot; the bare cube keeps leaping for 7.5 seconds behind a shield closed 0.55 of every 1.5 seconds. Rushes break columns. |
| Pi | Black block glyph on a dais at the north edge; its left leg and separate right stem are its fists | The stem on the archer's side slams while the other guards the square aperture. Crossing the midline forces a switch that bares the eye for 1.3 seconds. The stems reach 54 units; the south gate is out of reach, but distant archers draw a stone barrage. |
| OpenCode | Light-grey chest after the logo frame, its window a hinged mouth; spit, vanish-and-bite, two leaps, then a gaping block volley | A close archer is bitten on reflex. Spat code blocks fall, stay as obstacles for the archer, and are crushed by landings. The open mouth accepts a hit from the front during the 1.6-second gape. |
| DeepSeek | Orca in the logo blue: deep navy back, white chin, belly sweep and eye patch, tall dorsal fin, paddle flippers; the tail curls over its back as in the logo when it falls; three breaches, then a dive and an electric discharge | Breaches dive onto the archer and shatter floes where they erupt and land. The raised tail opens as the whale falls and for 0.7 seconds after landing. Shooting needs a dry floe; the discharge kills anyone in the water and then restores every floe. |
| Copilot | Teal helmet with goggles, a dark face plate with two slots, ear pods and a rear jet; energy orbs and one to three spin dashes | Orbs knock the archer back, fatally into a wall. A dash into the rim or a column stuns it facing the wall for 1.4 seconds with the rear jet bare, while a telegraphed row of ice falls across the archer's line. The jet accepts a hit only from behind. |
| Cline | The logo's robot head in light steel: a rounded hollow frame with a point at the middle of each side, two upright cyan pill eyes in a dark face, the large antenna ball, and a rounded shell behind with a hot vent; it glides clockwise between four station pads, always turned to the archer | At each station a fan of lethal bolts flies at the archer, 3, 5, 6, 6 round and round; an even fan is offset half a gap so standing still is still hit. The whole back is always open but never shown: an arrow loosed past the head and recalled through it from behind wins anywhere on the back. |
| Grok | The logo's white ring, broken where a long double-edged slash crosses it from lower left to upper right, a red core set in the back of its dark disc; it keeps its face to the archer at a throwing distance | It hurls the slash on a swinging arc, lodges it, and hauls itself along the line after it. Getting behind it makes it whip round to cover its back; the third whip-round sends it into a three-second frenzy, spinning with the blade whirled out 15.5 units round it and closing in. The back core is open through the frenzy and the reel after it, and passes the archer once a revolution. |
| ZCode | The Z.ai logo's Z in slate stone: the long diagonal is a slab standing on a dais at the north with a shuttered core in the middle of its face; the two bars of the Z sit across its ends at home and fly as fists in a fight | The fists slam the archer by turns; one that lands on either of the two floor switches stays to hold it down. Every fourth blow the slab's eye sweeps a laser through a half circle west to east through the south, ended by the hall's two columns; a roll carries the archer through it. With both switches held, the shutter slides open and the core is bare from the south for 1.5 seconds before it fires straight down the hall and lets go. Low fists stop arrows. |
| Cursor | The logo's graphite cube standing corner-on (its hexagon), the pointer cut into one face as a light arrow with its notch; it tumbles four quarter-turns toward the archer along the diagonals, then the pointer charges and fires | A laser fires down the pointer's face normal, telegraphed by rails at its exact width and stopped by the hall's four columns. The pointer is bare while it charges, but only from the side it faces. Facing the sky it flares harmlessly; pressed to the floor it blasts the cube over, and it lands with the pointer toward the viewer, dazed for 1.9 seconds. |

- Keep identity readable: models follow the Agent logos — the spark's uneven
  rays, the knot's six loops, Pi's aperture and separate stem, the chest's
  frame and window, the orca's markings, fin and curled tail, Copilot's
  goggles and face slots, Cursor's corner-on cube and pointer cut, Cline's
  pointed frame, pill eyes and antenna ball, Grok's broken ring and slash,
  ZCode's slab and bars as the logo's own parallelograms. Every model
  carries a dark one-pixel silhouette.
- Cursor's cube (`arena/cube.rs`) names faces in model space with +y the way
  it rolls. A quarter-turn about the leading bottom edge is a pitch about the
  center plus `cube::tumble`'s lift and travel. In combat the facet is
  discrete (`Boss::facet`, re-expressed by `cube::turned` whenever the
  symmetric cube snaps its yaw) because exposure and the laser read it; on the
  home route and the lane the turns accumulate as one rolling pitch with the
  facet held, so the pose never snaps, and each loop's quarter-turn count is a
  multiple of four.
- `Beam` is the shared straight-line threat: rails at its exact width while
  it charges (`delay`), lethal for `duration`, ended by `Map::beam_end`.
- Weak points use `Boss::articulated`, the same pivoted pitch and bank as the
  sculpture, so the drawn and the collided point agree in every pose.
- Terrain damage is combat state (`Map::broken`, `sunken`, `scars`, `blocks`).
  Paint it over the immutable floor cache; sunken floes lose all walkable art
  and ambient rings until restored. Spat blocks are the only prop that blocks
  the archer, and they are drawn as actors. Decorative debris never adds a
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
- Body, Pi's stems, split sparks, spat blocks, the brazier, archer and pillars
  sort by ground depth. Preview and combat use the same ordering, including a
  stem behind the glyph. Opaque bodies draw directly into the actor raster;
  only submerged, vanishing, flashing or departing bodies need a temporary
  raster for their mask.
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
| Spark hit before four splits / split spark hit | Split and recoil / shatter it; no victory |
| Ordinary arrow on the knot / shielded cube | Glance off; only a burning arrow melts it, only a shield gap wins |
| Arrow at Cline's face | Glances off; its back takes the arrow, recalled through it or loosed from behind |
| Waiting without play | No encounter opens its weak point by itself |
| Guarding stem in front of Pi's eye | The arrow stops; the eye opens only on a switch |
| Floe destroyed under a charging archer | Cancel drawing, show swimming, and preserve recall |
| Discharge after all floes sink | Restore every floe and its visible dry surface |
| Orb push into a wall / open ground | Death / displacement only |
| Committed landing or dash | Keep the committed target and route after the lock |
| Window or focus changes | Pause immediately and clear input |
| Archer dies | Clear threats; random safe rebirth; no result card |
| Guardian defeated | Finite departure, restored draft/focus, hidden preview until Agent changes |
| Won encounter, first time | Persist the unlock under the canonical Agent key; later losses never remove it |
| Won encounter, faster than the record | Replace the stored best and show "New best!" in the clock |
| Won encounter, slower or equal to the record | Keep the stored best; unlock still persists |
| State predates clear times | No best line, and a win still records one |
| Duel started with a stored best | The clock shows it as the pace to beat on the home page and over a session |
| Session Agent has no unlocked guardian | No lane, no badge, no reserved row |
| Session turn starts | The guardian makes its own entrance, then rides its lane |
| Session turn not running | No lane; a turn that just ended plays the guardian's own exit once |
| Companion double-clicked | Duel over the session page, session elements gone, Escape returns |
| Composer moves to another Agent | Lane hands over instantly; a duel opened over the previous session ends |
| Reduced motion enabled | No companion lane at all, and nothing left roaming behind the setting |

## 5. Good / Base / Bad Cases

- Good: bait the knot onto a forge plate, shoot through the flame, then time
  the bare cube between shield pulses; a miss leaves the next plate landing.
- Good: split the spark four times while dodging its hops and the split
  sparks, then hit the bare heart; or cross Pi's midline and shoot the eye
  during the switch.
- Base: click a roaming model, play across a scrolling field, then Escape and
  type game-key letters into the unchanged draft.
- Bad: shift the composer to reserve preview space, reset the entry pose,
  translate a flat logo for every attack, use one map for all guardians, leave
  an invisible clock running, or make visible cover disagree with a beam.

## 6. Required Tests and Checks

- Combat: entry/input exclusion, minimum draw, sprint, single arrow/retrieval,
  closed and directional weak points, spark splitting, brazier ignition and
  shield gaps, stem switches and reach, block obstacles, ambush lock, breach
  floe damage, discharge shelter, orb knockback and wall crush, wall stun,
  committed targets, swept collisions, cover, random rebirth, threat cleanup,
  finite victory, continuous transitions, bounded long runs, input
  buffering/cancellation, and normal-input wins for all six guardians across
  spawn seeds. Gameplay pilots must not override damage,
  position, puzzle state, or encounter timers.
- Raster/camera: distinct maps, exact quad reconstruction and bounded count,
  preview without an archer, identical preview/first combat frame, joining idle
  loops, reduced motion, moving model bounds, two-sided fins, raised geometry,
  following camera, pointer mapping, and entry projection endpoints. Pin logo
  features (spark rays, Pi's aperture and stems, the chest frame, the whale's
  eye and tail, goggles/face slots), drawn weak points at their collision
  anchors, silhouettes, per-pixel occlusion independent of face order, landing
  and route tells, floe shelter marks, and the removal of sunken-floe art.
- GPUI integration: native pointer/keyboard entry, no dialogs/chrome, full-home
  bounds at narrow sizes and rem zoom, composer hit testing, retained draft/focus,
  navigation and pending-focus cancellation, Agent switching, victory hiding,
  death/rebirth, held input, pointer release outside, and pause.
- Companion: the lane appears only for an unlocked guardian of a running
  session and disappears for a locked one, a non-running turn, and reduced
  motion; the badge stays inside the lane, and the lane above every composer
  row, at narrow and wide composer widths; every guardian stands at a legible
  height inside its lane; every guardian covers the lane on a lap of a
  distinct length, pauses or turns for a share of it, and stays under the
  pace limit (the whale exempt from pausing); every itinerary moves without a
  jump and closes its lap; every entrance starts out of sight and lands on the
  lane without a cut, every exit starts from the held pose and ends out of
  sight, including from mid-entrance; a starting turn brings the badge in from
  outside the lane while a hand-over does not; the resampled frame keeps the
  figure's coverage; a finished turn plays its exit and retires it, returning
  the composer to its own height; double-click
  opens a duel that covers the session page and Escape returns; a victory
  reports the unlock; a session switch ends the previous duel. Art tests pin
  the crop to the same drawing, and that no idle, lane, entrance, or exit
  pose reaches its window edge.
- Preference tests retain legacy decoding and persisted opt-out coverage.
  Progress tests retain "absent state decodes to nothing defeated" and
  "a repeat win is not a change". Clear-time tests retain "only a faster win
  replaces the record", "an absent record decodes to none", and key/value
  normalization on load.
- Clock tests retain the `mm:ss.t` format, a square face for every character
  the format can emit, the reserved row matching the painted digits, and the
  run clock freezing on the victory it records. GPUI tests retain the clock's
  top-left placement away from the exit mark, the winning run becoming the pace
  to beat, a slower replay keeping a faster record, and the face staying large
  enough to read: pin the cell size and assert the plate is wider and taller
  than the face it frames.
- `arena::art::sheets` (ignored) writes contact sheets — home roaming, lane
  entrance and pass, and battle frames — to `$VIBEX_ARENA_SHEETS`, optionally
  for one `$VIBEX_ARENA_GUARDIAN` key; `arena::art::sheets::windows` prints
  each guardian's companion window and median lane height for authoring
  `COMPANION_BOXES` / `COMPANION_HEIGHTS`. Review new artwork with them.
- Run `cargo test -p vibex-desktop --lib arena:: --locked`,
  `cargo test -p vibex-desktop-model --locked`, formatting, and scoped Clippy.
  Measure dynamic rasterization and rectangle counts after artwork changes;
  CPU raster timings alone do not establish end-to-end UI frame rate. Baseline:
  release battle frames rasterize in under 1 ms with at most ~3.3k merged
  rectangles per guardian.

## 7. Wrong vs Correct

```rust
// Wrong: restarting entry discards the visible roaming pose and camera.
let battle = Arena::new(Guardian::Claude, 0.0);

// Correct: the entry owns the pose actually measured and displayed.
let sample = self.preview.get();
let arena = Arena::from_preview(sample.boss, sample.time, seed);
let entry_camera = sample.viewport;

// Wrong: a per-pass clock cuts the loop every time the companion turns, and
// one shared pass (or the home route side-on) gives every guardian the same
// rhythm whatever its gait.
let time = phase * ROAM_PERIOD;
let place = across(&Boss::idle(guardian, time, false));

// Correct: the pose clock is monotonic and reads the executor's clock; pose
// and place both come from the guardian's own entrance and lane itinerary.
let time = seconds(executor.now(), clock); // negative while entering
let shot = gait::shot(guardian, time);
let (x, y) = placement(&shot, lane_width, badge_width, lane_height);

// Wrong: painting raster pixels at a third of a screen pixel each.
let pixel = unit / art::SCALE; // ~0.3px: quads collapse into noise

// Correct: resample the crop to whole screen pixels, one ink each.
let frame = art::companion(&boss, time, false).resample(fit.columns, fit.rows);

// Wrong: timing one life instead of the attempt, or letting the clock restart.
let elapsed = self.elapsed; // resets on respawn

// Correct: the duel's own clock, frozen by victory and recorded from there.
self.run_time += STEP; // through Battle, Defeat and Rebirth
let clear_ms = self.clear_millis();
```
