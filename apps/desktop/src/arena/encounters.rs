//! Each encounter owns its tells, committed movement and repeatable opening.
//! A shared single arrow becomes a different tool against each physical form.

use super::*;

impl Arena {
    pub(super) fn tick_boss(&mut self) {
        self.boss.previous_position = self.boss.position;
        self.boss.previous_core = self.boss.core;
        self.boss.exposed = (self.boss.exposed - STEP).max(0.0);
        self.boss.time += STEP;
        if self.boss.squash > 0.0 {
            self.boss.squash = (self.boss.squash - STEP * 4.0).max(0.0);
        }
        self.boss.flash = (self.boss.flash - STEP).max(0.0);
        self.boss.spin = self.boss.spin.rem_euclid(TAU);
        match self.boss.guardian {
            Guardian::Claude => self.tick_claude(),
            Guardian::Codex => self.tick_codex(),
            Guardian::Pi => self.tick_pi(),
            Guardian::OpenCode => self.tick_opencode(),
            Guardian::DeepSeek => self.tick_deepseek(),
            Guardian::Copilot => self.tick_copilot(),
            Guardian::Cursor => self.tick_cursor(),
            Guardian::Cline => self.tick_cline(),
            Guardian::Grok => self.tick_grok(),
        }
        self.boss.update_core();
    }

    /// Tracks the archer until the windup locks, keeping the landing on clear
    /// terrain and within the given reach.
    fn follow_target(&mut self, reach: f32) {
        if self.boss.target_locked() {
            return;
        }
        let from = self.boss.position;
        let delta = self.player.position.minus(from);
        let goal = if delta.length() > reach {
            from.plus(delta.normalized().scale(reach))
        } else {
            self.player.position
        };
        self.boss.target = self.map.move_body(from, goal, self.boss.radius());
        self.boss.direction = self.boss.target.minus(from).normalized();
    }

    /// A ballistic arc from `from` to `target`; returns true on landing.
    fn arc(&mut self, peak: f32) -> bool {
        let t = self.boss.progress();
        self.boss.position = self.boss.from.lerp(self.boss.target, t);
        self.boss.height = (t * PI).sin() * peak + self.boss.from_height * (1.0 - t);
        self.boss.squash = -0.3 * (t * PI).sin();
        if t >= 1.0 {
            self.boss.height = 0.0;
            self.boss.squash = 1.0;
            return true;
        }
        false
    }

    /// Rolls or dashes along the committed direction; returns the collision
    /// point when the body meets the rim or an intact column.
    fn charge(&mut self, speed: f32) -> Option<Vec2> {
        let before = self.boss.position;
        let next = before.plus(self.boss.direction.scale(speed * STEP));
        let radius = self.boss.radius();
        let pillar = self.map.pillar_hit(before, next, radius);
        if pillar.is_none() && self.map.inside(next, radius + 0.5) {
            self.boss.position = next;
            if ((self.boss.time / 0.06) as u32) > self.boss.fired {
                self.boss.fired = (self.boss.time / 0.06) as u32;
                self.effect(EffectKind::Dust, before, 0.5, 3.0);
            }
            return None;
        }
        if let Some(ix) = pillar {
            self.break_pillar(ix);
        }
        let contact = before.plus(self.boss.direction.scale(radius));
        self.boss.position =
            self.map
                .move_body(before, before.minus(self.boss.direction.scale(4.0)), radius);
        Some(contact)
    }

    fn rotate_yaw(&mut self, desired: f32, rate: f32) {
        let delta = (desired - self.boss.yaw + PI).rem_euclid(TAU) - PI;
        self.boss.yaw += delta.clamp(-rate, rate);
    }
}

#[path = "encounter_claude.rs"]
mod claude;
#[path = "encounter_cline.rs"]
mod cline;
#[path = "encounter_codex.rs"]
mod codex;
#[path = "encounter_copilot.rs"]
mod copilot;
#[path = "encounter_cursor.rs"]
mod cursor;
#[path = "encounter_deepseek.rs"]
mod deepseek;
#[path = "encounter_grok.rs"]
pub(super) mod grok;
#[path = "encounter_opencode.rs"]
mod opencode;
#[path = "encounter_pi.rs"]
mod pi;
