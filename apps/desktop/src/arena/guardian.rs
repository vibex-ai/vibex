//! Guardian identities, attack choreography, and the shared animation state.

use std::f32::consts::TAU;

use super::geometry::{Vec2, smoothstep};

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

    pub fn next(self) -> Self {
        Self::ALL[(self as usize + 1) % Self::ALL.len()]
    }

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

    pub fn logo(self) -> &'static [u8] {
        match self {
            Self::Claude => include_bytes!("../../assets/icons/claude.svg"),
            Self::Codex => include_bytes!("../../assets/icons/openai.svg"),
            Self::Pi => include_bytes!("../../assets/icons/agents/pi.svg"),
            Self::OpenCode => include_bytes!("../../assets/icons/opencode.svg"),
            Self::DeepSeek => include_bytes!("../../assets/icons/agents/deepseek-harness.svg"),
            Self::Copilot => include_bytes!("../../assets/icons/copilot.svg"),
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
    Fallen,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Attack {
    LeftFist,
    RightFist,
    Clap,
    Rush,
    Leap,
    Cross,
    Pulse,
    Stomp,
    Sweep,
    Dash,
    Volley,
    Dive,
}

impl Attack {
    pub fn strike_duration(self) -> f32 {
        match self {
            Self::Rush => 3.0,
            Self::Dash => 0.36,
            Self::Leap | Self::Dive => 0.65,
            _ => 0.18,
        }
    }
    pub fn impact_radius(self) -> f32 {
        match self {
            Self::Clap => 6.0,
            Self::Stomp | Self::Leap | Self::Dive => 6.5,
            _ => 4.5,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Boss {
    pub guardian: Guardian,
    pub position: Vec2,
    pub previous_position: Vec2,
    pub core: Vec2,
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
    spin_from: f32,
}

impl Boss {
    pub fn new(guardian: Guardian) -> Self {
        let position = Vec2::new(48.0, 22.0);
        let mut boss = Self {
            guardian,
            position,
            previous_position: position,
            core: position,
            from: position,
            target: Vec2::new(48.0, 43.0),
            direction: Vec2::new(0.0, 1.0),
            exposed: 0.0,
            state: BossState::Dormant,
            attack: Attack::LeftFist,
            time: 0.0,
            duration: 1.0,
            attacks: 0,
            spin: 0.0,
            spin_from: 0.0,
        };
        boss.update_core();
        boss
    }
    pub fn radius(&self) -> f32 {
        match self.guardian {
            Guardian::Claude | Guardian::OpenCode => 6.0,
            Guardian::Codex | Guardian::Copilot => 6.5,
            Guardian::Pi | Guardian::DeepSeek => 4.8,
        }
    }
    pub fn progress(&self) -> f32 {
        (self.time / self.duration).clamp(0.0, 1.0)
    }
    pub fn airborne(&self) -> bool {
        self.state == BossState::Striking && matches!(self.attack, Attack::Leap | Attack::Dive)
    }
    pub fn enter(&mut self, state: BossState, duration: f32) {
        self.state = state;
        self.time = 0.0;
        self.duration = duration;
        self.spin_from = self.spin;
    }
    pub fn animate(&mut self, delta: f32) {
        if self.guardian != Guardian::Codex {
            return;
        }
        self.spin = match self.state {
            BossState::Windup => self.spin_from - smoothstep(self.progress()) * 0.55,
            BossState::Rushing => self.spin + delta * 7.0,
            BossState::Recovery => {
                let rest = (self.spin_from / TAU).round() * TAU;
                self.spin_from + (rest - self.spin_from) * smoothstep(self.time / 0.45)
            }
            _ => self.spin,
        };
    }
    pub fn attack_origin(&self) -> Vec2 {
        if self.attack == Attack::Sweep {
            self.core
        } else {
            self.position
        }
    }
    pub fn prepare(&mut self, player: Vec2) {
        self.attack = match self.guardian {
            Guardian::Claude => {
                [Attack::LeftFist, Attack::RightFist, Attack::Clap][self.attacks as usize % 3]
            }
            Guardian::Codex => {
                if self.attacks % 3 == 2 {
                    Attack::Leap
                } else {
                    Attack::Rush
                }
            }
            Guardian::Pi => {
                if self.attacks.is_multiple_of(2) {
                    Attack::Cross
                } else {
                    Attack::Pulse
                }
            }
            Guardian::OpenCode => {
                if self.attacks.is_multiple_of(2) {
                    Attack::Stomp
                } else {
                    Attack::Sweep
                }
            }
            Guardian::DeepSeek => Attack::Dash,
            Guardian::Copilot => {
                if self.attacks.is_multiple_of(2) {
                    Attack::Volley
                } else {
                    Attack::Dive
                }
            }
        };
        self.from = self.position;
        self.target = player.clamped(6.0);
        self.direction = self.target.minus(self.attack_origin()).normalized();
        let duration = match self.attack {
            Attack::LeftFist | Attack::RightFist | Attack::Stomp => 0.95,
            Attack::Clap | Attack::Cross | Attack::Pulse | Attack::Dive => 1.15,
            Attack::Dash if !self.attacks.is_multiple_of(3) => 0.70,
            _ => 1.05,
        };
        self.attacks = self.attacks.wrapping_add(1);
        self.enter(BossState::Windup, duration);
    }
    pub fn update_core(&mut self) {
        self.core = if self.guardian == Guardian::Codex && self.exposed > 0.0 {
            self.position.minus(self.direction.scale(5.0))
        } else {
            self.position.plus(Vec2::new(0.0, -4.0))
        };
    }
}
