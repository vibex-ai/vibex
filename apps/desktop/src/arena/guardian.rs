//! Six encounter identities and their visible, spatial combat state.

use super::{
    geometry::{Vec2, smoothstep},
    map::CENTER,
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
}

impl Guardian {
    pub const ALL: [Self; 6] = [
        Self::Claude,
        Self::Codex,
        Self::Pi,
        Self::OpenCode,
        Self::DeepSeek,
        Self::Copilot,
    ];
    pub fn agent(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Pi => "Pi",
            Self::OpenCode => "OpenCode",
            Self::DeepSeek => "DeepSeek",
            Self::Copilot => "Copilot",
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
            _ => None,
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
    LeftFist,
    RightFist,
    Rush,
    Leap,
    Pulse,
    Stomp,
    Sweep,
    Dash,
    Volley,
    TendrilSweep,
    Inhale,
    Spokes,
    Barrage,
    VisorBeam,
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
    /// Independent hand anchors keep reaching attacks separate from the torso.
    pub hands: [Vec2; 2],
    pub hand_heights: [f32; 2],
    /// Ground-projected endpoints and independent endpoint elevations.
    pub tentacles: [Vec2; 6],
    pub tentacle_heights: [f32; 6],
    pub severed: u8,
    pub seals: u8,
    pub seal_time: [f32; 2],
    pub strain: f32,
    pub tether: f32,
    pub shield_angle: f32,
    pub beam_end: Vec2,
    pub fired: u32,
    pub ward: u8,
}

impl Boss {
    pub fn new(guardian: Guardian) -> Self {
        let position = CENTER.plus(Vec2::new(0.0, -12.0));
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
            attack: Attack::LeftFist,
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
            tentacles: [position; 6],
            tentacle_heights: [0.0; 6],
            severed: 0,
            seals: 0,
            seal_time: [0.0; 2],
            strain: 0.0,
            tether: 0.0,
            shield_angle: 0.0,
            beam_end: position,
            fired: 0,
            ward: 0,
        };
        boss.rest_hands();
        boss.rest_tentacles();
        boss.update_core();
        boss.previous_core = boss.core;
        boss
    }
    pub fn radius(&self) -> f32 {
        match self.guardian {
            Guardian::Claude | Guardian::Codex | Guardian::OpenCode | Guardian::Copilot => 8.0,
            Guardian::Pi | Guardian::DeepSeek => 9.0,
        }
    }
    pub fn progress(&self) -> f32 {
        (self.time / self.duration).clamp(0.0, 1.0)
    }
    pub fn airborne(&self) -> bool {
        self.height > 5.0
            || self.state == BossState::Submerged
            || self.guardian == Guardian::DeepSeek && self.height < -0.5
    }
    pub fn enter(&mut self, state: BossState, duration: f32) {
        self.from_height = self.height;
        self.state = state;
        self.time = 0.0;
        self.duration = duration;
        self.fired = 0;
    }
    pub fn face(&mut self, target: Vec2, rate: f32) {
        let direction = target.minus(self.position).normalized();
        let desired = direction.angle() - PI * 0.5;
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
    pub fn resting_hand(&self, side: f32) -> Vec2 {
        let (sin, cos) = self.yaw.sin_cos();
        self.position.plus(Vec2::new(
            side * 16.0 * cos - 2.4 * sin,
            (side * 16.0 * sin + 2.4 * cos) * 0.62,
        ))
    }
    pub fn rest_hands(&mut self) {
        self.hands = [self.resting_hand(-1.0), self.resting_hand(1.0)];
        self.hand_heights = [3.0, 3.0];
    }
    pub fn resting_tentacle(&self, ix: usize) -> Vec2 {
        let (x, y) = [
            (-15.0, -4.0),
            (15.0, -4.0),
            (-19.0, 1.0),
            (19.0, 1.0),
            (-14.0, 8.0),
            (14.0, 8.0),
        ][ix];
        self.local(x, y, -self.height)
    }
    pub fn tentacle_rest_height(&self, ix: usize) -> f32 {
        if ix < 2 { 3.0 } else { 1.5 }
    }
    pub fn rest_tentacles(&mut self) {
        for ix in 0..6 {
            self.tentacles[ix] = self.resting_tentacle(ix);
            self.tentacle_heights[ix] = self.tentacle_rest_height(ix);
        }
    }
    pub fn tentacle_root(&self, ix: usize) -> (Vec2, f32) {
        let (x, y, z) = [
            (-8.5, -2.5, 11.0),
            (8.5, -2.5, 11.0),
            (-8.0, 1.5, 9.5),
            (8.0, 1.5, 9.5),
            (-4.2, 3.2, 8.8),
            (4.2, 3.2, 8.8),
        ][ix];
        (self.local(x, y, -self.height), self.height + z)
    }
    pub fn tentacle_joint(&self, ix: usize) -> Vec2 {
        let (ground, height) = self.tentacle_root(ix);
        let shoulder = ground.plus(Vec2::new(0.0, -height));
        let tip = self.tentacles[ix].plus(Vec2::new(0.0, -self.tentacle_heights[ix]));
        shoulder.lerp(tip, 0.72)
    }
    pub fn tentacle_strike(&self, ix: usize, progress: f32) -> Vec2 {
        let side = if ix.is_multiple_of(2) { -1.0 } else { 1.0 };
        self.target.plus(
            self.direction
                .perpendicular()
                .scale(side * (7.0 + smoothstep(progress) * 5.0)),
        )
    }
    pub fn tentacle_vulnerable(&self, ix: usize) -> bool {
        self.guardian == Guardian::Claude
            && self.exposed <= 0.0
            && self.severed & (1 << ix) == 0
            && ((self.attack == Attack::TendrilSweep
                && self.state == BossState::Recovery
                && ix / 2 == self.ward as usize)
                || (self.attack == Attack::Inhale && self.state == BossState::Striking))
    }
    pub fn is_inhaling(&self) -> bool {
        self.attack == Attack::Inhale && self.state == BossState::Striking
    }
    pub fn update_core(&mut self) {
        self.core = match self.guardian {
            Guardian::Claude => self.local(0.0, 3.7, 6.3),
            Guardian::Codex => self.local(0.0, 0.0, 11.0),
            Guardian::Pi => self.local(-2.6, -0.6, 17.0),
            Guardian::OpenCode => self.local(0.0, 5.7, 9.0),
            Guardian::DeepSeek => self.local(0.0, -15.5, 15.0),
            Guardian::Copilot => self.local(0.0, 4.2, 3.8),
        };
    }
    pub fn attack_origin(&self) -> Vec2 {
        match self.guardian {
            Guardian::Pi => self.local(-2.6, 3.4, 17.0),
            Guardian::Copilot => self.local(0.0, 5.0, 13.0),
            _ => self.local(0.0, 5.7, 9.0),
        }
    }
    pub fn pulse_origin(&self) -> Vec2 {
        if self.guardian == Guardian::DeepSeek {
            CENTER
        } else {
            self.position
        }
    }
    pub fn beam_radius(&self) -> f32 {
        match self.guardian {
            Guardian::Codex => 1.0,
            Guardian::Pi => 1.2,
            Guardian::Copilot => 1.4,
            _ => 1.5,
        }
    }
    pub fn target_locked(&self) -> bool {
        let lock = match self.guardian {
            Guardian::Pi => 0.48,
            Guardian::DeepSeek => 0.52,
            _ => 0.55,
        };
        self.state != BossState::Windup || self.progress() >= lock
    }
    pub fn impact_radius(&self) -> f32 {
        match (self.guardian, self.attack) {
            (Guardian::Claude, _) => 4.3,
            (Guardian::Codex, Attack::Leap) => 7.0,
            (Guardian::Pi, _) => 5.0,
            (Guardian::OpenCode, _) => 8.0,
            (Guardian::DeepSeek, _) => 9.0,
            (Guardian::Copilot, _) => 7.5,
            _ => 4.5,
        }
    }
    pub fn hover_height(&self, time: f32) -> f32 {
        match self.guardian {
            Guardian::Claude => 2.5 + (time * 2.0).sin() * 0.5,
            Guardian::Copilot => 6.0 + (time * 3.0).sin() * 0.7,
            _ => 0.0,
        }
    }
    pub fn rolling_height(pitch: f32) -> f32 {
        pitch.cos().abs() * 7.5 + pitch.sin().abs() * 4.5 - 7.5
    }
    pub fn prepare(&mut self, player: Vec2) {
        self.attack = match self.guardian {
            Guardian::Claude => {
                self.ward = ((self.attacks / 2) % 3) as u8;
                [
                    Attack::TendrilSweep,
                    Attack::Rush,
                    Attack::TendrilSweep,
                    Attack::Inhale,
                ][self.attacks as usize % 4]
            }
            Guardian::Codex => [
                Attack::Pulse,
                Attack::Rush,
                Attack::Spokes,
                Attack::Rush,
                Attack::Barrage,
            ][self.attacks as usize % 5],
            Guardian::Pi => match self.seals {
                1 => Attack::RightFist,
                2 => Attack::LeftFist,
                _ => [
                    Attack::LeftFist,
                    Attack::RightFist,
                    Attack::Barrage,
                    Attack::Sweep,
                ][self.attacks as usize % 4],
            },
            Guardian::OpenCode => [
                Attack::Sweep,
                Attack::Barrage,
                Attack::Inhale,
                Attack::Stomp,
            ][self.attacks as usize % 4],
            Guardian::DeepSeek => {
                if self.attacks % 4 == 3 {
                    Attack::Pulse
                } else {
                    Attack::Leap
                }
            }
            Guardian::Copilot => {
                [Attack::Volley, Attack::VisorBeam, Attack::Dash][self.attacks as usize % 3]
            }
        };
        self.attacks = self.attacks.wrapping_add(1);
        self.from = self.position;
        let reach = player.minus(self.position);
        self.target = if self.guardian == Guardian::Claude && reach.length() > 32.0 {
            self.position.plus(reach.normalized().scale(32.0))
        } else {
            player
        };
        self.direction = player.minus(self.position).normalized();
        let duration = match self.attack {
            Attack::LeftFist | Attack::RightFist => 0.85,
            Attack::Rush => 0.9,
            Attack::Leap => 1.0,
            Attack::Pulse => 0.95,
            Attack::Stomp => 0.65,
            Attack::Sweep => 1.25,
            Attack::Dash => 0.85,
            Attack::Volley => 0.85,
            Attack::TendrilSweep => 1.0,
            Attack::Inhale => 0.9,
            Attack::Spokes => 1.0,
            Attack::Barrage => 0.9,
            Attack::VisorBeam => 1.2,
        };
        self.enter(BossState::Windup, duration);
    }
    /// Smooth, deterministic roaming. No combat entity or clock is needed on
    /// the home; the displayed pose is copied verbatim when the user enters.
    pub fn idle(guardian: Guardian, time: f32, reduced: bool) -> Self {
        let mut boss = Self::new(guardian);
        if reduced {
            boss.position = CENTER.plus(Vec2::new(0.0, -34.0));
            boss.previous_position = boss.position;
            boss.rest_hands();
            boss.rest_tentacles();
            boss.update_core();
            boss.previous_core = boss.core;
            return boss;
        }
        let t = time.rem_euclid(36.0) * TAU / 36.0;
        let (x, y, tangent) = match guardian {
            Guardian::Claude => {
                let phase = t - PI * 0.25;
                (
                    phase.sin(),
                    (phase * 2.0).sin(),
                    Vec2::new(phase.cos() * 52.0, (phase * 2.0).cos() * 68.0),
                )
            }
            Guardian::Codex => {
                let phase = t + PI;
                (
                    phase.sin(),
                    phase.cos() * 0.9,
                    Vec2::new(phase.cos() * 52.0, -phase.sin() * 30.6),
                )
            }
            Guardian::Pi => {
                let phase = t - PI * 0.25;
                (
                    -phase.cos(),
                    (phase * 2.0).sin() * 0.9,
                    Vec2::new(phase.sin() * 52.0, (phase * 2.0).cos() * 61.2),
                )
            }
            Guardian::OpenCode => {
                let t = t + PI;
                let x_curve = 0.65 + 0.35 * t.sin().abs();
                let y_curve = 0.65 + 0.35 * t.cos().abs();
                (
                    t.sin() / x_curve,
                    t.cos() / y_curve * 0.9,
                    Vec2::new(
                        t.cos() * 33.8 / x_curve.powi(2),
                        -t.sin() * 19.89 / y_curve.powi(2),
                    ),
                )
            }
            Guardian::DeepSeek => {
                let phase = t + PI;
                (
                    phase.sin(),
                    phase.cos() * 0.9,
                    Vec2::new(phase.cos() * 52.0, -phase.sin() * 30.6),
                )
            }
            Guardian::Copilot => {
                let phase = t + PI;
                (
                    phase.sin(),
                    phase.cos(),
                    Vec2::new(phase.cos() * 52.0, -phase.sin() * 34.0),
                )
            }
        };
        boss.position = CENTER.plus(Vec2::new(x * 52.0, y * 34.0));
        boss.previous_position = boss.position;
        // Upright forms keep their identifying front readable while roaming.
        // The whale turns along its spatial route.
        boss.yaw = if matches!(
            guardian,
            Guardian::Claude
                | Guardian::Pi
                | Guardian::Codex
                | Guardian::OpenCode
                | Guardian::Copilot
        ) {
            t.sin() * 0.48 + (t * 2.0).sin() * 0.06
        } else {
            tangent.angle() - PI * 0.5
        };
        boss.direction = Vec2::from_angle(boss.yaw + PI * 0.5);
        boss.stride = t * 24.0;
        boss.height = match guardian {
            Guardian::Copilot => 8.0 + (t * 6.0).sin(),
            Guardian::Claude => 2.5 + (t * 4.0).sin() * 0.5,
            Guardian::Pi => 0.5 + (t * 4.0).sin() * 0.25,
            Guardian::DeepSeek => 1.0 + (t * 4.0).sin(),
            _ => 0.0,
        };
        boss.spin = if guardian == Guardian::Codex {
            t * 6.0
        } else {
            0.0
        };
        if guardian == Guardian::OpenCode {
            boss.pitch = (t * 2.0).sin() * 0.10;
            boss.height = Self::rolling_height(boss.pitch);
        }
        boss.bank = if matches!(guardian, Guardian::DeepSeek | Guardian::Copilot) {
            (t * 2.0).sin() * 0.16
        } else {
            0.0
        };
        boss.rest_hands();
        boss.rest_tentacles();
        boss.update_core();
        boss.previous_core = boss.core;
        boss
    }
    pub fn opening(&mut self, duration: f32) {
        self.exposed = duration;
        self.enter(BossState::Recovery, duration + 0.3);
        self.update_core();
        self.previous_core = self.core;
    }
    pub fn settle(&mut self) {
        let t = smoothstep(self.progress());
        self.height *= 1.0 - t * 0.08;
        self.pitch *= 0.92;
        self.bank *= 0.92;
    }
}
