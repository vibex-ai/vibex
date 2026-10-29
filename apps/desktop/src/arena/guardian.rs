//! Six encounter identities and their visible, spatial combat state.

use super::{
    geometry::{Vec2, smoothstep},
    map::{CENTER, THRONE},
};
use std::f32::consts::{PI, TAU};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Guardian {
    Claude,
    Codex,
    Pi,
    OpenCode,
    DeepSeek,
    Copilot,
    Cursor,
    Cline,
}

impl Guardian {
    pub const ALL: [Self; 8] = [
        Self::Claude,
        Self::Codex,
        Self::Pi,
        Self::OpenCode,
        Self::DeepSeek,
        Self::Copilot,
        Self::Cursor,
        Self::Cline,
    ];
    pub fn agent(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Pi => "Pi",
            Self::OpenCode => "OpenCode",
            Self::DeepSeek => "DeepSeek",
            Self::Copilot => "Copilot",
            Self::Cursor => "Cursor",
            Self::Cline => "Cline",
        }
    }
    pub fn from_agent(id: &str) -> Option<Self> {
        match id {
            "claude" | "claude-code" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "pi" | "pi-agent" => Some(Self::Pi),
            "opencode" => Some(Self::OpenCode),
            "deepseek" | "deepseek-harness" => Some(Self::DeepSeek),
            "copilot" | "github-copilot" => Some(Self::Copilot),
            "cursor" | "cursor-agent" => Some(Self::Cursor),
            "cline" => Some(Self::Cline),
            _ => None,
        }
    }
    /// The canonical Agent id this guardian is stored and looked up by.
    ///
    /// Agent ids have aliases, so a victory has to be recorded against the one
    /// spelling every surface agrees on; otherwise the same reward could stay
    /// locked for a session that names its Agent differently.
    pub fn key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Pi => "pi",
            Self::OpenCode => "opencode",
            Self::DeepSeek => "deepseek",
            Self::Copilot => "copilot",
            Self::Cursor => "cursor",
            Self::Cline => "cline",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BossState {
    Dormant,
    Watching,
    Windup,
    Striking,
    Rushing,
    Recovery,
    Submerged,
    Fallen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Attack {
    Hop,
    Rush,
    Leap,
    LeftFist,
    RightFist,
    Switch,
    Barrage,
    Spit,
    Ambush,
    Bite,
    Gape,
    Breach,
    Discharge,
    Volley,
    Dash,
    /// A cube's quarter-turn onto its next face.
    Tumble,
    /// The pointer facet charging, then firing along its face.
    Laser,
    /// A glide to the next station round the room.
    Glide,
    /// A fan of bolts from the eyes.
    Fan,
}

/// Visual and collision scale of the spark after each split.
pub(super) const SPARK_SCALE: [f32; 5] = [1.0, 0.8, 0.64, 0.52, 0.42];
pub(super) const IDLE_LOOP: f32 = 36.0;
/// The middle of Cline's head above its ground anchor.
pub(super) const CLINE_HEAD: f32 = 11.0;

/// Elevation of a spark's center; a landing squash lowers it.
pub(super) fn spark_height(size: f32, squash: f32) -> f32 {
    11.0 * size * (1.0 - 0.25 * squash)
}

/// How far the whale's tail rises at body station `y` when curled over its
/// back like the logo; `curl` runs from 0 (swimming) to 1 (raised).
pub(super) fn tail_lift(curl: f32, y: f32) -> f32 {
    if y < -5.0 {
        curl * 9.0 * ((-5.0 - y) / 9.5).powi(2)
    } else {
        0.0
    }
}

/// Width and length of Pi's two stems: the glyph's left leg and right stem.
pub(super) const STEMS: [(f32, f32); 2] = [(5.74, 16.8), (5.17, 12.06)];

/// A spark split from the carrier. It hops on its own rhythm but has no heart.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Shard {
    pub alive: bool,
    pub position: Vec2,
    pub previous: Vec2,
    pub from: Vec2,
    pub target: Vec2,
    pub height: f32,
    pub time: f32,
    pub duration: f32,
    pub airborne: bool,
    pub size: f32,
    pub spin: f32,
}

impl Shard {
    pub fn radius(&self) -> f32 {
        7.5 * self.size
    }
    /// Aim-plane center of the floating spark.
    pub fn center(&self) -> Vec2 {
        self.position
            .plus(Vec2::new(0.0, -spark_height(self.size, 0.0) - self.height))
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Boss {
    pub guardian: Guardian,
    pub position: Vec2,
    pub previous_position: Vec2,
    /// Aim-plane coordinates of the actual visible weak point, distinct from
    /// the footprint used for body contact and terrain collisions.
    pub core: Vec2,
    pub previous_core: Vec2,
    pub from: Vec2,
    pub target: Vec2,
    pub direction: Vec2,
    pub exposed: f32,
    pub state: BossState,
    pub attack: Attack,
    pub time: f32,
    pub duration: f32,
    pub attacks: u32,
    pub spin: f32,
    pub height: f32,
    pub from_height: f32,
    pub pitch: f32,
    pub bank: f32,
    pub yaw: f32,
    pub stride: f32,
    /// Pi's two detachable stems: ground anchors and independent elevations.
    pub hands: [Vec2; 2],
    pub hand_heights: [f32; 2],
    /// The stem currently covering the glyph's square aperture.
    pub guard: usize,
    /// Completed splits of the spark; after four its heart is bare.
    pub generation: u8,
    pub shards: [Shard; 4],
    /// Positive flattens a landing body, negative stretches it in the air.
    pub squash: f32,
    pub lid: f32,
    /// Visibility of a body that can vanish and reappear elsewhere.
    pub fade: f32,
    pub unravel: f32,
    /// Forge plate held down by the knot, lighting the central brazier.
    pub plate: Option<usize>,
    pub shield: f32,
    pub dashes: u8,
    /// Brief white flash after a hit that did not finish the guardian.
    pub flash: f32,
    pub fired: u32,
    /// The face of the cube that carries Cursor's pointer facet, in model
    /// space (see [`cube`](super::cube)).
    pub facet: u8,
}

impl Boss {
    pub fn new(guardian: Guardian) -> Self {
        let position = if guardian == Guardian::Pi {
            THRONE
        } else {
            CENTER.plus(Vec2::new(0.0, -12.0))
        };
        let mut boss = Self {
            guardian,
            position,
            previous_position: position,
            core: position,
            previous_core: position,
            from: position,
            target: CENTER.plus(Vec2::new(0.0, 25.0)),
            direction: Vec2::new(0.0, 1.0),
            exposed: 0.0,
            state: BossState::Dormant,
            attack: Attack::Hop,
            time: 0.0,
            duration: 1.0,
            attacks: 0,
            spin: 0.0,
            height: 0.0,
            from_height: 0.0,
            pitch: 0.0,
            bank: 0.0,
            yaw: 0.0,
            stride: 0.0,
            hands: [position; 2],
            hand_heights: [0.0; 2],
            guard: 1,
            generation: 0,
            shards: [Shard::default(); 4],
            squash: 0.0,
            lid: 0.0,
            fade: 1.0,
            unravel: 0.0,
            plate: None,
            shield: 0.0,
            dashes: 0,
            flash: 0.0,
            fired: 0,
            facet: super::cube::FRONT,
        };
        if guardian == Guardian::Cursor {
            // The cube stands corner-on, as the logo's hexagon.
            boss.yaw = -PI * 0.25;
            boss.direction = Vec2::from_angle(boss.yaw + PI * 0.5);
        }
        boss.rest_hands();
        boss.update_core();
        boss.previous_core = boss.core;
        boss
    }
    pub fn scale(&self) -> f32 {
        if self.guardian == Guardian::Claude {
            SPARK_SCALE[self.generation.min(4) as usize]
        } else {
            1.0
        }
    }
    pub fn radius(&self) -> f32 {
        match self.guardian {
            Guardian::Claude => 7.5 * self.scale(),
            Guardian::Codex | Guardian::OpenCode => 8.0,
            Guardian::Pi | Guardian::DeepSeek => 9.0,
            Guardian::Copilot => 7.0,
            Guardian::Cursor => 8.5,
            Guardian::Cline => 8.0,
        }
    }
    pub fn progress(&self) -> f32 {
        (self.time / self.duration).clamp(0.0, 1.0)
    }
    pub fn airborne(&self) -> bool {
        self.height > 5.0
            || self.state == BossState::Submerged
            || self.fade < 0.5
            || self.guardian == Guardian::DeepSeek && self.height < -0.5
    }
    pub fn enter(&mut self, state: BossState, duration: f32) {
        self.from_height = self.height;
        self.state = state;
        self.time = 0.0;
        self.duration = duration;
        self.fired = 0;
    }
    /// Commits an attack: the windup aims at the player until its lock point.
    pub fn begin(&mut self, attack: Attack, player: Vec2, windup: f32) {
        self.attack = attack;
        self.attacks = self.attacks.wrapping_add(1);
        self.from = self.position;
        self.target = player;
        self.direction = player.minus(self.position).normalized();
        self.enter(BossState::Windup, windup);
    }
    pub fn face(&mut self, target: Vec2, rate: f32) {
        let direction = target.minus(self.position).normalized();
        self.turn_to(direction.angle() - PI * 0.5, rate);
    }
    pub fn turn_to(&mut self, desired: f32, rate: f32) {
        let delta = (desired - self.yaw + PI).rem_euclid(TAU) - PI;
        self.yaw += delta.clamp(-rate, rate);
        self.direction = Vec2::from_angle(self.yaw + PI * 0.5);
    }
    pub fn local(&self, x: f32, y: f32, z: f32) -> Vec2 {
        let (sin, cos) = self.yaw.sin_cos();
        self.position.plus(Vec2::new(
            x * cos - y * sin,
            (x * sin + y * cos) * 0.62 - z - self.height,
        ))
    }
    /// A model-space ground point, ignoring the body's elevation.
    pub fn ground(&self, x: f32, y: f32) -> Vec2 {
        self.local(x, y, -self.height)
    }
    /// Vertical scale of a squashing body, applied about its ground contact.
    pub fn squash_scale(&self) -> f32 {
        1.0 - self.squash
            * match self.guardian {
                Guardian::Claude => 0.25,
                Guardian::Codex => 0.2,
                Guardian::OpenCode => 0.3,
                _ => 0.0,
            }
    }
    /// Rotation center of each sculpture. Pitch and bank never move it.
    pub fn pivot(&self) -> (f32, f32, f32) {
        let squash = self.squash_scale();
        match self.guardian {
            Guardian::Claude => (0.0, 0.0, spark_height(self.scale(), self.squash)),
            Guardian::Codex => (0.0, 0.0, 11.0 * squash),
            Guardian::Pi => (-2.6, 0.5, 14.0),
            Guardian::OpenCode => (0.0, 0.0, 6.5 * squash),
            Guardian::DeepSeek => (0.0, 2.0, 8.0),
            Guardian::Copilot => (0.0, 0.0, 11.0),
            Guardian::Cursor => (0.0, 0.0, super::cube::HALF),
            Guardian::Cline => (0.0, 0.0, CLINE_HEAD),
        }
    }
    pub fn tilt(&self) -> (f32, f32) {
        if self.guardian == Guardian::Codex {
            (self.pitch, self.spin)
        } else {
            (self.pitch, self.bank)
        }
    }
    /// Model point after the same pivoted pitch, bank, yaw and lift used by
    /// the sculpture, projected to the aim plane.
    pub fn articulated(&self, x: f32, y: f32, z: f32) -> Vec2 {
        let (px, py, pz) = self.pivot();
        let (pitch, bank) = self.tilt();
        let (dx, dy, dz) = (x - px, y - py, z - pz);
        let (s, c) = pitch.sin_cos();
        let (dy, dz) = (dy * c - dz * s, dy * s + dz * c);
        let (s, c) = bank.sin_cos();
        let (dx, dz) = (dx * c + dz * s, -dx * s + dz * c);
        self.local(dx + px, dy + py, dz + pz)
    }
    pub fn resting_hand(&self, ix: usize) -> Vec2 {
        self.ground(if ix == 0 { -8.3 } else { 8.6 }, 0.0)
    }
    /// In front of the aperture, high enough for that stem to cover it.
    pub fn guard_point(&self, ix: usize) -> (Vec2, f32) {
        (self.ground(-2.6, 5.5), if ix == 0 { 6.0 } else { 9.0 })
    }
    pub fn rest_hands(&mut self) {
        self.hands = [self.resting_hand(0), self.resting_hand(1)];
        self.hand_heights = [0.0; 2];
    }
    pub fn body_center(&self) -> Vec2 {
        let squash = self.squash_scale();
        match self.guardian {
            Guardian::Claude => self.local(0.0, 0.0, spark_height(self.scale(), self.squash)),
            Guardian::Codex => self.local(0.0, 0.0, 11.0 * squash),
            Guardian::Pi => self.local(-1.0, 0.0, 15.0),
            Guardian::OpenCode => self.articulated(0.0, 0.0, 7.0 * squash),
            Guardian::DeepSeek => self.articulated(0.0, 2.0, 8.0),
            Guardian::Copilot => self.articulated(0.0, 0.0, 11.0),
            Guardian::Cursor => self.local(0.0, 0.0, super::cube::HALF),
            Guardian::Cline => self.articulated(0.0, 0.0, CLINE_HEAD),
        }
    }
    pub fn update_core(&mut self) {
        let squash = self.squash_scale();
        self.core = match self.guardian {
            Guardian::Claude => self.local(0.0, 0.0, spark_height(self.scale(), self.squash)),
            Guardian::Codex => self.local(0.0, 0.0, 11.0 * squash),
            Guardian::Pi => self.local(-2.6, 0.5, 14.0),
            Guardian::OpenCode => self.articulated(0.0, 1.5, 9.0 * squash),
            Guardian::DeepSeek => {
                self.articulated(0.0, -15.2, 11.4 + tail_lift(self.unravel, -15.2))
            }
            Guardian::Copilot => self.articulated(0.0, -8.6, 10.0),
            Guardian::Cursor => {
                let (across, up) = super::cube::POINTER;
                let (x, y, z) =
                    super::cube::facet_point(self.facet, across, up, super::cube::HALF + 0.2);
                self.articulated(x, y, super::cube::HALF + z)
            }
            // The vent on the back of the head.
            Guardian::Cline => self.articulated(0.0, -8.6, CLINE_HEAD),
        };
    }
    pub fn attack_origin(&self) -> Vec2 {
        match self.guardian {
            Guardian::Pi => self.local(-2.6, 2.0, 20.0),
            Guardian::OpenCode => self.articulated(0.0, 5.0, 9.0 * self.squash_scale()),
            Guardian::Copilot => self.articulated(0.0, 6.0, 12.0),
            Guardian::Cline => self.articulated(0.0, 4.0, CLINE_HEAD),
            _ => self.body_center(),
        }
    }
    pub fn target_locked(&self) -> bool {
        let lock = match (self.guardian, self.attack) {
            (Guardian::Claude, _) => 0.6,
            (Guardian::OpenCode, Attack::Ambush) => 0.6,
            _ => 0.55,
        };
        !matches!(self.state, BossState::Windup | BossState::Submerged) || self.progress() >= lock
    }
    pub fn impact_radius(&self) -> f32 {
        match (self.guardian, self.attack) {
            (Guardian::Claude, _) => 3.0 + 5.0 * self.scale(),
            (Guardian::Codex | Guardian::OpenCode | Guardian::Cursor, Attack::Leap) => 7.0,
            (Guardian::Pi, _) => 5.5,
            (Guardian::DeepSeek, _) => 6.5,
            _ => 6.0,
        }
    }
    /// Angles of the bolts in Cline's current fan, relative to its aim. The
    /// fans run 3, 5, 6, 6: an odd fan is centred on the archer and an even
    /// one straddles them, offset half a gap so standing still is still hit.
    pub fn fan(&self) -> Vec<f32> {
        const COUNTS: [u32; 4] = [3, 5, 6, 6];
        const SPREAD: f32 = 0.2;
        let count = COUNTS[(self.attacks.wrapping_sub(1) % 4) as usize];
        let even = if count % 2 == 0 { SPREAD * 0.5 } else { 0.0 };
        (0..count)
            .map(|n| (n as f32 - (count as f32 - 1.0) * 0.5) * SPREAD + even)
            .collect()
    }
    pub fn hover_height(&self, time: f32) -> f32 {
        match self.guardian {
            Guardian::Copilot => 6.5 + (time * 3.0).sin() * 0.6,
            Guardian::Cline => 3.0 + (time * 2.4).sin() * 0.5,
            _ => 0.0,
        }
    }
    /// The bare cube's shield pulses; only its gaps accept an arrow.
    pub fn cube_shielded(&self) -> bool {
        self.guardian == Guardian::Codex && self.exposed > 0.0 && self.shield.rem_euclid(1.5) < 0.55
    }
    pub fn opening(&mut self, duration: f32) {
        self.exposed = duration;
        self.enter(BossState::Recovery, duration + 0.2);
        self.update_core();
        self.previous_core = self.core;
    }
    /// Smooth, deterministic roaming in each form's own gait. No combat entity
    /// or clock is needed on the home; the displayed pose is copied verbatim
    /// when the user enters.
    pub fn idle(guardian: Guardian, time: f32, reduced: bool) -> Self {
        let mut boss = Self::new(guardian);
        if reduced {
            boss.position = CENTER.plus(Vec2::new(0.0, -34.0));
            boss.previous_position = boss.position;
            boss.rest_hands();
            boss.update_core();
            boss.previous_core = boss.core;
            return boss;
        }
        let time = time.rem_euclid(IDLE_LOOP);
        let t = time * TAU / IDLE_LOOP;
        boss.stride = time * 4.0;
        match guardian {
            Guardian::Claude => {
                // Twenty-four hops per loop, airborne for the first 45% of each.
                let (hop, u) = beat(time, 1.5);
                let air = (u / 0.45).min(1.0);
                let s = (hop + smoothstep(air)) * TAU / 24.0;
                boss.position = route(guardian, s).0;
                boss.height = if u < 0.45 {
                    (air * PI).sin() * 9.0
                } else {
                    0.0
                };
                boss.squash = landing_squash(u, 0.45);
                boss.spin = (hop + smoothstep(air)) * TAU / 8.0;
                boss.yaw = s.sin() * 0.3;
            }
            Guardian::Codex => {
                // The knot rolls on its rim like a wheel along a wide figure eight.
                let (position, tangent) = route(guardian, t);
                boss.position = position;
                boss.yaw = tangent.angle();
                boss.spin = t * 4.0;
                boss.height = ((t * 24.0).sin() * 0.35).max(0.0);
            }
            Guardian::Pi => {
                let (position, tangent) = route(guardian, t);
                boss.position = position;
                boss.yaw = t.sin() * 0.25;
                let (step, u) = beat(time, 1.2);
                let swing = step as usize % 2;
                let heading = tangent.normalized();
                for ix in 0..2 {
                    let offset = if ix == swing {
                        -4.0 + smoothstep(u) * 8.0
                    } else {
                        4.0 - u * 8.0
                    };
                    boss.hands[ix] = boss.resting_hand(ix).plus(heading.scale(offset));
                    boss.hand_heights[ix] = if ix == swing {
                        (u * PI).sin() * 4.5
                    } else {
                        0.0
                    };
                }
                boss.bank = if swing == 0 { 1.0 } else { -1.0 } * (u * PI).sin() * 0.07;
            }
            Guardian::OpenCode => {
                let (hop, u) = beat(time, 2.0);
                let air = (u / 0.4).min(1.0);
                let s = (hop + smoothstep(air)) * TAU / 18.0;
                boss.position = route(guardian, s).0;
                boss.height = if u < 0.4 { (air * PI).sin() * 8.0 } else { 0.0 };
                boss.squash = landing_squash(u, 0.4);
                boss.lid = if u < 0.4 {
                    (air * PI).sin() * 0.55
                } else if (0.55..0.8).contains(&u) {
                    ((u - 0.55) * PI * 12.0).sin().abs() * 0.3
                } else {
                    0.0
                };
                boss.yaw = s.sin() * 0.35;
            }
            Guardian::DeepSeek => {
                let (position, tangent) = route(guardian, t);
                boss.position = position;
                boss.yaw = tangent.angle() - PI * 0.5;
                let (leap, u) = beat(time, 9.0);
                // Breaches happen where the arc keeps clear of the page top.
                let start = route(guardian, leap * 9.0 * TAU / IDLE_LOOP).0;
                if u < 0.28 && start.y > CENTER.y - 8.0 {
                    let a = u / 0.28;
                    boss.height = 1.5 + (a * PI).sin() * 11.0;
                    boss.pitch = (0.5 - a) * (a * PI).sin() * 1.0;
                    boss.unravel = (a * PI).sin();
                } else {
                    boss.height = 1.5 + (t * 16.0).sin() * 0.6;
                    boss.pitch = (t * 8.0).sin() * 0.05;
                }
                boss.bank = (t * 2.0).sin() * 0.12;
            }
            Guardian::Copilot => {
                let (position, tangent) = route(guardian, t);
                boss.position = position;
                boss.height = 8.0 + (t * 4.0).sin();
                let heading = tangent.normalized();
                boss.yaw = (heading.x * 0.45).clamp(-0.5, 0.5);
                let (_, u) = beat(time, 12.0);
                boss.bank = -heading.x * 0.3 + TAU * smoothstep(u / 0.1);
                boss.pitch = heading.y * 0.12;
            }
            Guardian::Cursor => {
                // The cube stands corner-on, the logo's hexagon, and tumbles one
                // face at a time along the diagonals: a zigzag east along the
                // top of the home and back west along the bottom.
                let (step, u) = beat(time, IDLE_LOOP / CURSOR_STEPS as f32);
                let step = step as usize;
                let roll = (u / 0.4).min(1.0);
                let theta = smoothstep(roll) * PI * 0.5;
                let (lift, advance) = super::cube::tumble(theta);
                let heading = cursor_heading(step);
                let (from, to) = (cursor_waypoint(step), cursor_waypoint(step + 1));
                boss.position = from.lerp(to, advance / super::cube::SIDE);
                boss.height = lift;
                // The quarter-turns accumulate as one rolling pitch, with the
                // facet carried round by it rather than snapping between faces.
                boss.pitch = (-(step as f32 * PI * 0.5 + theta) + PI).rem_euclid(TAU) - PI;
                let yaw = super::cube::yaw_toward(heading);
                let next = super::cube::yaw_toward(cursor_heading(step + 1));
                let turn = (next - yaw + PI).rem_euclid(TAU) - PI;
                boss.yaw = yaw + turn * smoothstep((u - 0.55) / 0.3);
            }
            Guardian::Cline => {
                // The head glides clockwise between eight stations round the
                // home, always turned to the viewer, and rocks back at each
                // stop as if it had fired.
                let (station, u) = beat(time, IDLE_LOOP / 8.0);
                let at = |n: f32| {
                    let a = n * TAU / 8.0;
                    CENTER.plus(Vec2::new(a.sin() * 60.0, -a.cos() * 30.0))
                };
                let glide = smoothstep(u / 0.45);
                let (from, to) = (at(station), at(station + 1.0));
                boss.position = from.lerp(to, glide);
                boss.height = 3.0 + (time * TAU / 1.5).sin() * 0.6;
                let moving = if u < 0.45 { (u / 0.45 * PI).sin() } else { 0.0 };
                boss.yaw = -to.minus(from).normalized().x * 0.3 * moving;
                let held = (u - 0.45) * IDLE_LOOP / 8.0;
                boss.pitch = if (0.0..0.4).contains(&held) {
                    (held / 0.4 * PI).sin() * 0.18
                } else {
                    0.0
                };
            }
        }
        boss.direction = Vec2::from_angle(boss.yaw + PI * 0.5);
        boss.previous_position = boss.position;
        if guardian != Guardian::Pi {
            boss.rest_hands();
        }
        boss.update_core();
        boss.previous_core = boss.core;
        boss
    }
}

/// Quarter-turns Cursor tumbles round its zigzag each home loop: a whole
/// number of four, so the pointer facet comes home with the loop.
const CURSOR_STEPS: usize = 24;
/// The diagonal Cursor's step `step` rolls along: runs of three quarter-turns,
/// north-east and south-east by turns going east, then south-west and
/// north-west coming back.
fn cursor_heading(step: usize) -> Vec2 {
    let (x, y) = match (step % CURSOR_STEPS) / 3 {
        0 | 2 => (1.0, -1.0),
        1 | 3 => (1.0, 1.0),
        4 | 6 => (-1.0, 1.0),
        _ => (-1.0, -1.0),
    };
    Vec2::new(x, y).normalized()
}
/// Where Cursor's step `step` leaves from.
fn cursor_waypoint(step: usize) -> Vec2 {
    let start = CENTER.plus(Vec2::new(-55.0, -6.0));
    (0..step % CURSOR_STEPS).fold(start, |p, ix| {
        p.plus(cursor_heading(ix).scale(super::cube::SIDE))
    })
}

/// Count and fraction of a repeating gait period.
pub(super) fn beat(time: f32, period: f32) -> (f32, f32) {
    let beat = time / period;
    (beat.floor(), beat.fract())
}

/// Stretch while airborne, flatten on landing, crouch before take-off.
pub(super) fn landing_squash(u: f32, air: f32) -> f32 {
    if u < air {
        -0.3 * (u / air * PI).sin()
    } else if u < air + 0.25 {
        (1.0 - smoothstep((u - air) / 0.25)) * 0.8
    } else if u > 0.85 {
        smoothstep((u - 0.85) / 0.15) * 0.5
    } else {
        0.0
    }
}

/// Closed roaming routes and their tangents for a loop parameter in radians.
fn route(guardian: Guardian, s: f32) -> (Vec2, Vec2) {
    // Roaming spans most of the home while keeping each model inside it.
    let (x, y, dx, dy) = match guardian {
        Guardian::Claude => {
            let s = s - PI * 0.25;
            (
                s.sin() * 70.0,
                (s * 2.0).sin() * 38.0,
                s.cos() * 70.0,
                (s * 2.0).cos() * 76.0,
            )
        }
        // Every loop starts in the upper half, clear of the composer.
        Guardian::Codex => (
            s.sin() * 70.0,
            -s.cos() * 34.0 + (s * 2.0).sin() * 6.0,
            s.cos() * 70.0,
            s.sin() * 34.0 + (s * 2.0).cos() * 12.0,
        ),
        Guardian::Pi => (
            s.sin() * 68.0,
            -s.cos() * 38.0,
            s.cos() * 68.0,
            s.sin() * 38.0,
        ),
        Guardian::OpenCode => (
            s.sin() * 70.0,
            -s.cos() * 40.0,
            s.cos() * 70.0,
            s.sin() * 40.0,
        ),
        Guardian::DeepSeek => (
            -s.sin() * 66.0,
            -s.cos() * 38.0,
            -s.cos() * 66.0,
            s.sin() * 38.0,
        ),
        Guardian::Copilot => (
            s.sin() * 63.0 + (s * 7.0).sin() * 5.0,
            -s.cos() * 36.0,
            s.cos() * 63.0 + (s * 7.0).cos() * 35.0,
            s.sin() * 36.0,
        ),
        Guardian::Cursor | Guardian::Cline => {
            unreachable!("{guardian:?} moves between its own stations instead")
        }
    };
    (CENTER.plus(Vec2::new(x, y)), Vec2::new(dx, dy))
}
