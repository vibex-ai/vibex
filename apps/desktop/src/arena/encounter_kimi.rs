use super::*;

const CHASE: f32 = 15.0;
const DASH: f32 = 72.0;
pub(in crate::arena) const TRAIL_RADIUS: f32 = 2.6;
/// How long a block of context lies on the floor, and how far apart they are.
const TRAIL_LIFE: f32 = 7.0;
const TRAIL_GAP: f32 = 5.5;
/// How long the tile skids and rocks after a charge, its blue square bare.
const SKID: f32 = 1.4;

impl Arena {
    /// Kimi slides after the archer and lays its long context down behind
    /// it: a trail of blue blocks that kill on touch until they fade, which an
    /// arrow through them clears. Every few seconds it charges straight
    /// across the floor; it skids out of the charge rocking on its edge, and
    /// for that moment the blue square at its corner is bare.
    pub(super) fn tick_kimi(&mut self) {
        let t = self.boss.progress();
        let before = self.boss.position;
        match (self.boss.state, self.boss.attack) {
            (BossState::Watching, _) => {
                self.boss.face(self.player.position, STEP * 5.0);
                let toward = self.player.position.minus(self.boss.position).normalized();
                self.boss.position = self.map.move_body(
                    self.boss.position,
                    self.boss.position.plus(toward.scale(CHASE * STEP)),
                    self.boss.radius(),
                );
                self.boss.pitch *= 0.9;
                self.boss.bank *= 0.9;
                // It chases a while before it ever charges.
                let patience = if self.boss.attacks == 0 { 3.0 } else { 0.0 };
                if self.boss.time >= self.boss.duration.max(patience) {
                    self.boss.begin(Attack::Charge, self.player.position, 0.55);
                }
            }
            (BossState::Windup, Attack::Charge) => {
                if !self.boss.target_locked() {
                    self.boss.face(self.player.position, STEP * 8.0);
                    self.boss.direction =
                        self.player.position.minus(self.boss.position).normalized();
                    self.boss.target = self.map.beam_end(self.boss.position, self.boss.direction);
                }
                // It rocks back on its heel before it goes.
                self.boss.pitch = 0.18 * smoothstep(t);
                if t >= 1.0 {
                    self.boss.enter(BossState::Rushing, 2.0);
                }
            }
            (BossState::Rushing, Attack::Charge) => {
                self.boss.pitch += (-0.15 - self.boss.pitch) * (STEP * 10.0).min(1.0);
                if let Some(contact) = self.charge(DASH) {
                    // Into a wall or a column: a harder stop, a longer skid.
                    self.impact_with(contact, 4.0, 0.04);
                    self.boss.exposed = SKID + 0.4;
                    self.boss.enter(BossState::Recovery, SKID + 0.4);
                } else if t >= 1.0 {
                    self.boss.exposed = SKID;
                    self.boss.enter(BossState::Recovery, SKID);
                }
            }
            (BossState::Recovery, _) => {
                // Skidding to a stop, rocking on its edge as it drifts round
                // to face back the way it came.
                self.boss.face(self.player.position, STEP * 4.0);
                self.boss.bank = (self.boss.time * 14.0).sin() * 0.25 * (1.0 - t);
                self.boss.pitch *= 0.92;
                if t >= 1.0 {
                    self.boss.bank = 0.0;
                    self.boss.enter(BossState::Watching, 3.2);
                }
            }
            _ => {}
        }
        // The context it leaves behind.
        self.boss.stride += self.boss.position.minus(before).length();
        if self.boss.stride >= TRAIL_GAP {
            self.boss.stride = 0.0;
            let behind = before.minus(self.boss.direction.scale(self.boss.radius() + 1.5));
            if self.map.inside(behind, 1.0) {
                self.hazard(HazardKind::Trail, behind, 0.0, TRAIL_LIFE, TRAIL_RADIUS);
            }
        }
    }
}
