//! Each encounter owns its tells, committed movement and repeatable opening.
//! A shared single arrow becomes a different tool against each physical form.

use super::*;

impl Arena {
    pub(super) fn tick_boss(&mut self) {
        self.boss.previous_position = self.boss.position;
        self.boss.previous_core = self.boss.core;
        self.boss.exposed = (self.boss.exposed - STEP).max(0.0);
        self.boss.time += STEP;
        self.boss.shield_angle += STEP * if self.boss.exposed > 0.0 { 0.25 } else { 1.5 };
        for ix in 0..2 {
            if self.boss.exposed <= 0.0 {
                self.boss.seal_time[ix] = (self.boss.seal_time[ix] - STEP).max(0.0);
                if self.boss.seal_time[ix] <= 0.0 {
                    self.boss.seals &= !(1 << ix);
                }
            }
        }
        match self.boss.guardian {
            Guardian::Claude => self.tick_claude(),
            Guardian::Codex => self.tick_codex(),
            Guardian::Pi => self.tick_pi(),
            Guardian::OpenCode => self.tick_opencode(),
            Guardian::DeepSeek => self.tick_deepseek(),
            Guardian::Copilot => self.tick_copilot(),
        }
        self.boss.update_core();
    }

    fn follow_target(&mut self) {
        if !self.boss.target_locked() {
            self.boss.target = self.player.position;
            if matches!(
                self.boss.attack,
                Attack::Leap | Attack::Stomp | Attack::Dash
            ) {
                self.boss.target = self.map.move_body(
                    self.boss.position,
                    self.player.position,
                    self.boss.radius(),
                );
            }
            self.boss.face(self.player.position, STEP * 3.2);
        }
    }

    fn rest_or_prepare(&mut self) {
        if self.boss.time >= self.boss.duration {
            self.boss.prepare(self.player.position);
            if self.boss.guardian == Guardian::DeepSeek && self.boss.attack == Attack::Pulse {
                self.boss.duration = 1.65;
            }
        }
    }

    fn beam_tell(&mut self) {
        let origin = self.boss.attack_origin();
        if !self.boss.target_locked() {
            self.boss.direction = self.boss.target.minus(origin).normalized();
        }
        self.boss.beam_end = self.map.beam_end(origin, self.boss.direction);
    }

    fn beam_strike(&mut self) {
        let origin = self.boss.attack_origin();
        self.boss.beam_end = self.map.beam_end(origin, self.boss.direction);
        self.hazards
            .retain(|hazard| hazard.kind != HazardKind::Beam);
        self.hazard(
            HazardKind::Beam,
            origin,
            self.boss.beam_end,
            0.0,
            STEP * 2.0,
            self.boss.beam_radius(),
        );
        if self.boss.fired == 0 {
            self.boss.fired = 1;
            self.effect(EffectKind::Sweep, origin, 0.6, 5.0);
        }
    }

    fn recover(&mut self, duration: f32) {
        self.hazards
            .retain(|hazard| hazard.kind != HazardKind::Beam && hazard.kind != HazardKind::Gravity);
        self.boss.enter(BossState::Recovery, duration);
    }
}

#[path = "encounter_claude.rs"]
mod claude;
#[path = "encounter_codex.rs"]
mod codex;
#[path = "encounter_copilot.rs"]
mod copilot;
#[path = "encounter_deepseek.rs"]
mod deepseek;
#[path = "encounter_opencode.rs"]
mod opencode;
#[path = "encounter_pi.rs"]
mod pi;
