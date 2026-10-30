use super::*;
use crate::arena::map::{GATE, ISLANDS, PLATES, SWITCHES, THRONE};

/// What the pilot wants this frame, read from visible state only.
struct Plan {
    goal: Vec2,
    target: Option<Vec2>,
    /// Keep drawing without releasing yet.
    hold: bool,
    charge: f32,
    sprint: bool,
    /// Whether to recall a loosed arrow now, when the encounter decides.
    recall: Option<bool>,
}

#[derive(Default)]
struct Pilot {
    flying: f32,
}

impl Pilot {
    fn tick(&mut self, arena: &mut Arena) {
        if arena.phase != Phase::Battle {
            arena.tick(Controls::default());
            return;
        }
        let plan = plan(arena);
        let player = arena.player.position;
        let mut controls = Controls {
            movement: steer(arena, plan.goal),
            ..Controls::default()
        };
        let escape = escape(arena, plan.goal);
        if let Some((direction, urgent)) = escape {
            controls.movement = direction;
            if urgent && arena.player.roll_cooldown <= 0.0 && !arena.player.swimming {
                arena.roll(direction);
            }
        } else {
            controls.sprint = plan.sprint || plan.goal.minus(player).length() > 14.0;
        }
        match arena.arrow.state {
            ArrowState::Ready => {
                self.flying = 0.0;
                let shot = plan
                    .target
                    .filter(|target| escape.is_none() && clear_shot(arena, *target));
                if let Some(target) = shot {
                    controls.aim = Some(target);
                    controls.shoot = true;
                    controls.movement = Vec2::default();
                    if !plan.hold && arena.player.charge >= plan.charge {
                        arena.release_shot(Some(target));
                        controls.shoot = false;
                    }
                } else if arena.player.charge > 0.0 {
                    // Letting go: a short draw looses nothing.
                    arena.release_shot(plan.target);
                }
            }
            ArrowState::Flying => {
                self.flying += STEP;
                controls.recall = self.flying > 0.8;
            }
            _ => {
                controls.recall = plan
                    .recall
                    .unwrap_or(escape.is_none() || arena.player.swimming)
            }
        }
        arena.tick(controls);
    }
}

fn plan(arena: &Arena) -> Plan {
    let boss = &arena.boss;
    let player = arena.player.position;
    let charge = MIN_CHARGE + 0.04;
    let speed = 100.0 + charge * 45.0;
    let draw = (charge - arena.player.charge).max(0.0);
    let arrival = |point: Vec2| draw + point.minus(player).length() / speed;
    let around = |center: Vec2, distance: f32| {
        let outward = player.minus(center);
        let outward = if outward.length() < 1.0 {
            CENTER.minus(center).normalized()
        } else {
            outward.normalized()
        };
        arena
            .map
            .move_body(center, center.plus(outward.scale(distance)), 2.0)
    };
    let mut plan = Plan {
        goal: around(boss.position, 26.0),
        target: None,
        hold: false,
        charge,
        sprint: false,
        recall: None,
    };
    match boss.guardian {
        Guardian::Claude => {
            plan.goal = around(boss.position, 20.0);
            let grounded = match boss.state {
                BossState::Watching => boss.duration - boss.time + 0.22,
                BossState::Windup => boss.duration - boss.time,
                _ => 0.0,
            };
            let height =
                |size: f32| Vec2::new(0.0, -crate::arena::guardian::spark_height(size, 0.0));
            let center = if boss.generation >= 4 {
                boss.core
            } else {
                boss.body_center()
            };
            let landing = boss.target.plus(height(boss.scale()));
            if grounded > arrival(center) + 0.03 {
                plan.target = Some(center);
            } else if boss.state == BossState::Striking
                && boss.target.minus(player).length() > boss.impact_radius() + 4.0
            {
                // Draw while it flies; loose as it lands.
                plan.target = Some(landing);
                plan.hold = true;
            } else {
                plan.target = boss
                    .shards
                    .iter()
                    .filter(|s| {
                        s.alive
                            && !s.airborne
                            && s.position.minus(player).length() < 30.0
                            && s.duration - s.time > arrival(s.center()) + 0.03
                    })
                    .map(|s| s.center())
                    .min_by(|a, b| {
                        a.minus(player)
                            .length()
                            .total_cmp(&b.minus(player).length())
                    });
            }
        }
        Guardian::Codex => {
            if boss.exposed > 0.0 {
                plan.goal = around(boss.position, 22.0);
                let grounded = match boss.state {
                    BossState::Watching => boss.duration - boss.time + 0.5,
                    BossState::Windup => boss.duration - boss.time,
                    _ => 0.0,
                };
                let arrive = arrival(boss.core);
                let pulse = (boss.shield + arrive).rem_euclid(1.5);
                if grounded > arrive + 0.04
                    && (0.62..1.42).contains(&pulse)
                    && boss.exposed > arrive
                {
                    plan.target = Some(boss.core);
                }
            } else if arena.brazier_lit() {
                // Line up flame and knot, then shoot through the fire.
                let knot = boss.body_center();
                let flame = flame_center();
                let spot = flame.plus(flame.minus(knot).normalized().scale(8.0));
                plan.goal = spot;
                plan.sprint = true;
                if player.minus(spot).length() < 1.5 {
                    plan.target = Some(knot);
                    plan.charge = 0.3;
                }
            } else {
                // Bait the leap onto the forge plate the knot can reach.
                let knot = boss.position;
                plan.goal = *PLATES
                    .iter()
                    .min_by(|a, b| a.minus(knot).length().total_cmp(&b.minus(knot).length()))
                    .unwrap();
                plan.sprint = true;
            }
        }
        Guardian::Pi => {
            let gate = 99.0;
            let active = 1 - boss.guard;
            let across = if active == 0 { 1.0 } else { -1.0 };
            let (guard_point, _) = boss.guard_point(boss.guard);
            let bare = boss.hand_heights[boss.guard] >= 10.0
                || boss.hands[boss.guard].minus(guard_point).length() > 5.0;
            if boss.exposed > 0.0 && bare {
                plan.goal = player;
                if boss.exposed > arrival(boss.core) + 0.03 {
                    plan.target = Some(boss.core);
                    plan.charge = MIN_CHARGE;
                }
            } else if arena.arrow.state == ArrowState::Ready && boss.exposed <= 0.0 {
                // Crossing the midline forces the stems to trade places.
                plan.goal = Vec2::new(THRONE.x + across * 9.0, gate);
            } else {
                plan.goal = Vec2::new(THRONE.x - across * 9.0, gate);
            }
        }
        Guardian::OpenCode => {
            plan.goal = around(boss.position, 27.0);
            let front = player.minus(boss.core).dot(boss.direction) > 4.0;
            if boss.exposed > arrival(boss.core) + 0.05 && front {
                plan.target = Some(boss.core);
            }
        }
        Guardian::DeepSeek => {
            let intact: Vec<(Vec2, f32)> = ISLANDS
                .iter()
                .enumerate()
                .filter(|(ix, _)| arena.map.sunken & (1 << ix) == 0)
                .map(|(_, island)| *island)
                .collect();
            let floes = if intact.is_empty() {
                ISLANDS.to_vec()
            } else {
                intact
            };
            let (floe, radius) = floes
                .into_iter()
                .min_by(|a, b| {
                    a.0.minus(player)
                        .length()
                        .total_cmp(&b.0.minus(player).length())
                })
                .unwrap();
            let on = arena.map.on_island(player);
            plan.goal = floe;
            match (boss.state, boss.attack) {
                (BossState::Submerged, Attack::Breach) if !boss.target_locked() => {
                    // Bait the eruption into open water beside the floe.
                    let out = CENTER.minus(floe).normalized();
                    plan.goal = arena
                        .map
                        .move_body(floe, floe.plus(out.scale(radius + 18.0)), 1.0);
                }
                (BossState::Striking, Attack::Breach) if on && boss.progress() > 0.5 => {
                    let mut landed = *boss;
                    landed.position = boss.target;
                    landed.height = 0.0;
                    landed.pitch = 0.0;
                    landed.update_core();
                    plan.target = Some(landed.core);
                    plan.hold = true;
                }
                (BossState::Recovery, _) if on && boss.exposed > arrival(boss.core) + 0.03 => {
                    plan.goal = player;
                    plan.target = Some(boss.core);
                }
                _ => {}
            }
        }
        Guardian::Copilot => {
            if boss.exposed > 0.0 {
                plan.goal = arena.map.move_body(
                    boss.position,
                    boss.position.minus(boss.direction.scale(20.0)),
                    2.0,
                );
                let behind = player.minus(boss.core).dot(boss.direction) < -2.0;
                if behind && boss.exposed > arrival(boss.core) + 0.03 {
                    plan.target = Some(boss.core);
                }
            } else {
                plan.goal = CENTER.lerp(around(boss.position, 26.0), 0.5);
            }
        }
        Guardian::Devin => {
            let mouth = boss.body_center();
            let clear_of_vines = |from: Vec2, to: Vec2| {
                (0..3).all(|arm| {
                    (0..boss.vines[arm] as usize).all(|k| {
                        segment_distance(boss.vine_point(arm, k), from, to)
                            > crate::arena::devin::SEGMENT_REACH + 0.6
                    })
                })
            };
            plan.goal = around(boss.position, 30.0);
            if boss.swallowed {
                plan.recall = Some(true);
            } else if let Some(arm) = (0..3).find(|arm| boss.torn & (1 << arm) != 0) {
                // Into the gap the torn petal left, and shoot through it.
                let gap = boss.petal_point(arm).minus(boss.position).normalized();
                plan.goal =
                    arena
                        .map
                        .move_body(boss.position, boss.position.plus(gap.scale(28.0)), 2.0);
                plan.sprint = true;
                let through = player.minus(boss.core).normalized().dot(gap) > 0.75;
                if through && boss.exposed > arrival(boss.core) + 0.03 {
                    plan.target = Some(boss.core);
                }
            } else if boss.attack == Attack::Inhale
                && boss.state == BossState::Windup
                && boss.duration - boss.time > arrival(mouth) + 0.05
                && clear_of_vines(player, mouth)
            {
                plan.target = Some(mouth);
            }
        }
        Guardian::Antigravity => {
            let spinning = boss.attack == Attack::Frenzy
                && matches!(boss.state, BossState::Striking | BossState::Recovery);
            plan.goal = around(boss.position, if spinning { 26.0 } else { 30.0 });
            if spinning {
                // Loose so the arrow comes through the opening as it turns.
                let arrive = arrival(boss.core);
                let mut future = *boss;
                if boss.state == BossState::Striking {
                    future.yaw += super::encounters::antigravity::SPIN_RATE * arrive;
                }
                future.direction = Vec2::from_angle(future.yaw + PI * 0.5);
                let align = player
                    .minus(boss.core)
                    .normalized()
                    .dot(future.direction)
                    .abs();
                let needed = if arena.player.charge > 0.0 { 0.78 } else { 0.9 };
                if boss.exposed > arrive + 0.03 && align > needed {
                    plan.target = Some(boss.core);
                }
            }
        }
        Guardian::CodeBuddy => {
            let bug = arena
                .hazards
                .iter()
                .find(|h| h.kind == HazardKind::Bug)
                .map(|h| h.position);
            plan.goal = match bug {
                // Past its own bug, on the far side from the cat, so it
                // waddles over the bug to reach us.
                Some(bug) => arena.map.move_body(
                    bug,
                    bug.plus(bug.minus(boss.position).normalized().scale(15.0)),
                    2.0,
                ),
                None => around(boss.position, 22.0),
            };
            if boss.exposed > arrival(boss.core) + 0.03 {
                plan.target = Some(boss.core);
            }
        }
        Guardian::Kimi => {
            plan.goal = around(boss.position, 28.0);
            let front = player.minus(boss.core).dot(boss.direction) > 3.0;
            if front && boss.exposed > arrival(boss.core) + 0.03 {
                plan.target = Some(boss.core);
            }
        }
        Guardian::ZCode => {
            let sweeping = boss.attack == Attack::Sweep
                && matches!(boss.state, BossState::Windup | BossState::Striking);
            if boss.exposed > 0.0 {
                // Just off the line it fires down when it closes again.
                plan.goal = Vec2::new(GATE.x + 14.0, 70.0);
                if boss.exposed > arrival(boss.core) + 0.03 && player.y > boss.core.y + 2.0 {
                    plan.target = Some(boss.core);
                }
            } else if sweeping {
                // In a column's shadow from the eye.
                let eye = arena.zcode_eye();
                plan.goal = arena
                    .map
                    .pillars()
                    .iter()
                    .enumerate()
                    .filter(|(ix, _)| arena.map.intact(*ix))
                    .map(|(_, p)| {
                        p.position
                            .plus(p.position.minus(eye).normalized().scale(5.5))
                    })
                    .min_by(|a, b| {
                        a.minus(player)
                            .length()
                            .total_cmp(&b.minus(player).length())
                    })
                    .unwrap_or(player);
                plan.sprint = true;
            } else {
                // Stand on a switch nobody holds, so the next fist lands there.
                let held = |switch: Vec2| {
                    (0..2).any(|ix| boss.pinned[ix] && boss.hands[ix].minus(switch).length() < 1.0)
                };
                plan.goal = SWITCHES
                    .into_iter()
                    .filter(|s| !held(*s))
                    .min_by(|a, b| {
                        a.minus(player)
                            .length()
                            .total_cmp(&b.minus(player).length())
                    })
                    .unwrap_or(player);
                plan.sprint = true;
            }
        }
        Guardian::Grok => {
            let frenzy = boss.attack == Attack::Frenzy && boss.state == BossState::Striking;
            if frenzy || boss.state == BossState::Recovery {
                // Well outside the whirl, which keeps closing in.
                plan.goal = around(boss.position, if frenzy { 31.0 } else { 20.0 });
                // Loose so the arrow meets the back as the spin brings it round.
                let arrive = arrival(boss.core);
                let mut future = *boss;
                if frenzy {
                    future.yaw += 8.0 * arrive;
                    let closing = player.minus(boss.position).normalized();
                    future.position = boss.position.plus(closing.scale(7.0 * arrive));
                }
                future.direction = Vec2::from_angle(future.yaw + PI * 0.5);
                future.update_core();
                // Start drawing only when the back will face us squarely, and
                // keep drawing while it still mostly will.
                let align = player.minus(future.core).normalized().dot(future.direction);
                let needed = if arena.player.charge > 0.0 {
                    -0.1
                } else {
                    -0.6
                };
                if boss.exposed > arrive + 0.03 && align < needed {
                    plan.target = Some(future.core);
                }
            } else {
                // Get behind it so it has to whip round.
                let behind = boss.position.minus(boss.direction.scale(22.0));
                plan.goal = arena.map.move_body(boss.position, behind, 2.0);
                plan.sprint = true;
            }
        }
        Guardian::Cline => {
            let body = boss.body_center();
            match arena.arrow.state {
                ArrowState::Ready => {
                    // Loose past the head so the arrow lands behind it.
                    let across = body.minus(player).normalized().perpendicular();
                    plan.goal = around(boss.position, 30.0);
                    plan.target = Some(body.plus(across.scale(10.0)));
                }
                ArrowState::Returning => plan.recall = Some(true),
                _ => {
                    // Put the head between the arrow and the archer, then
                    // recall the arrow through its back.
                    let arrow = arena.arrow.position;
                    let through = body.minus(arrow).normalized();
                    plan.goal = arena.map.move_body(
                        boss.position,
                        boss.position.plus(through.scale(26.0)),
                        2.0,
                    );
                    plan.sprint = true;
                    let between = body.minus(arrow).dot(player.minus(body)) > 0.0;
                    plan.recall = Some(
                        between && segment_distance(body, arrow, player) < boss.radius() * 0.45,
                    );
                }
            }
        }
        Guardian::Cursor => {
            let facing = crate::arena::cube::ground_direction(boss.facet, boss.yaw);
            if boss.exposed > arrival(boss.core) + 0.03
                && facing.is_none_or(|f| player.minus(boss.core).dot(f) > 2.0)
            {
                plan.target = Some(boss.core);
            }
            // Beside the line the facet faces, on its side: the laser misses
            // and the facet is in sight.
            plan.goal = match facing {
                Some(f) => {
                    let across = f.perpendicular();
                    let sign = if player.minus(boss.position).dot(across) >= 0.0 {
                        1.0
                    } else {
                        -1.0
                    };
                    arena.map.move_body(
                        boss.position,
                        boss.position
                            .plus(f.scale(18.0))
                            .plus(across.scale(sign * 15.0)),
                        2.0,
                    )
                }
                None => around(boss.position, 26.0),
            };
        }
    }
    plan
}

fn clear_shot(arena: &Arena, target: Vec2) -> bool {
    let player = arena.player.position;
    let distance = target.minus(player).length();
    let near = target.minus(target.minus(player).normalized().scale(CORE_RADIUS + 0.1));
    (3.0..95.0).contains(&distance)
        && !arena.map.cover_hit(player, near)
        && (arena.boss.guardian != Guardian::DeepSeek || arena.map.on_island(player))
}

/// Committed landing point and radius of the current strike, if any.
fn strike(arena: &Arena) -> Option<(Vec2, f32)> {
    let boss = &arena.boss;
    if !boss.target_locked() {
        return None;
    }
    let radius = boss.impact_radius();
    let aerial = matches!(boss.state, BossState::Windup | BossState::Striking);
    match (boss.guardian, boss.attack) {
        (Guardian::Claude, _) if aerial => Some((boss.target, radius)),
        (Guardian::Codex | Guardian::OpenCode, Attack::Leap) if aerial => {
            Some((boss.target, radius))
        }
        (Guardian::Pi | Guardian::ZCode, Attack::LeftFist | Attack::RightFist) if aerial => {
            Some((boss.target, radius))
        }
        (Guardian::OpenCode, Attack::Bite) if aerial => {
            Some((boss.target.plus(boss.direction.scale(3.0)), radius))
        }
        (Guardian::OpenCode, Attack::Ambush)
            if matches!(boss.state, BossState::Submerged | BossState::Striking) =>
        {
            Some((boss.target, radius + 3.5))
        }
        (Guardian::DeepSeek, Attack::Breach)
            if matches!(boss.state, BossState::Submerged | BossState::Striking) =>
        {
            Some((boss.target, radius))
        }
        (Guardian::Cursor | Guardian::Antigravity, Attack::Leap) if aerial => {
            Some((boss.target, radius))
        }
        (Guardian::Grok, Attack::Throw) if aerial => {
            Some((boss.target, crate::arena::combat::GROK_BLADE_RADIUS + 1.0))
        }
        (Guardian::Grok, Attack::Frenzy) if boss.state == BossState::Striking => Some((
            boss.position,
            crate::arena::combat::GROK_FRENZY_RADIUS + 2.0,
        )),
        (Guardian::Cursor, Attack::Laser)
            if boss.state == BossState::Windup && boss.facet == crate::arena::cube::DOWN =>
        {
            Some((boss.position, 7.0))
        }
        _ => None,
    }
}

/// Committed rolling or dashing route, if any.
fn route(arena: &Arena) -> Option<(Vec2, Vec2, f32)> {
    let boss = &arena.boss;
    let committed =
        boss.state == BossState::Rushing || boss.state == BossState::Windup && boss.target_locked();
    match (boss.guardian, boss.attack) {
        (Guardian::Codex, Attack::Rush) if committed => Some((
            boss.position,
            arena.map.beam_end(boss.position, boss.direction),
            boss.radius(),
        )),
        (Guardian::Copilot, Attack::Dash) if committed => {
            Some((boss.position, boss.target, boss.radius()))
        }
        (Guardian::Kimi, Attack::Charge) if committed => {
            Some((boss.position, boss.target, boss.radius()))
        }
        (Guardian::Grok, Attack::Pull) if boss.state == BossState::Rushing => {
            Some((boss.position, boss.target, boss.radius()))
        }
        (Guardian::Cline, Attack::Glide) if boss.state == BossState::Rushing => {
            Some((boss.position, boss.target, boss.radius()))
        }
        (Guardian::Cursor, Attack::Tumble) if boss.state == BossState::Rushing => {
            Some((boss.position, boss.target, boss.radius()))
        }
        _ => None,
    }
}

/// Visible danger at a ground point.
fn threat(arena: &Arena, q: Vec2) -> f32 {
    let boss = &arena.boss;
    let mut score = 0.0;
    if let Some((point, radius)) = strike(arena)
        && q.minus(point).length() < radius + PLAYER_RADIUS + 1.5
    {
        score += 10.0;
    }
    if let Some((from, to, radius)) = route(arena)
        && segment_distance(q, from, to) < radius + PLAYER_RADIUS + 1.8
    {
        score += 10.0;
    }
    if boss.guardian != Guardian::Pi
        && !boss.airborne()
        && q.minus(boss.position).length() < boss.radius() + 3.0
    {
        score += 10.0;
    }
    if boss.guardian == Guardian::OpenCode
        && !boss.airborne()
        && q.minus(boss.position).length() < 19.0
    {
        score += 3.0;
    }
    for shard in boss.shards.iter().filter(|s| s.alive) {
        let close = if shard.airborne {
            q.minus(shard.target).length() < 2.0 + 4.0 * shard.size + PLAYER_RADIUS + 1.5
        } else {
            q.minus(shard.position).length() < shard.radius() + 2.0
        };
        if close {
            score += 10.0;
        }
    }
    for h in &arena.hazards {
        let distance = q.minus(h.position).length();
        match h.kind {
            HazardKind::Goo if distance < h.radius => score += 0.4,
            HazardKind::Drop | HazardKind::Icicle
                if h.time < h.delay + 0.02 && distance < h.radius + PLAYER_RADIUS + 1.2 =>
            {
                score += 10.0
            }
            HazardKind::Trail if distance < h.radius + PLAYER_RADIUS + 1.2 => score += 10.0,
            HazardKind::Hairball
                if h.time < h.delay + 0.02 && distance < h.radius + PLAYER_RADIUS + 1.2 =>
            {
                score += 10.0
            }
            HazardKind::Bug
                if h.time > h.delay - 1.0 && distance < h.radius + PLAYER_RADIUS + 2.0 =>
            {
                score += 10.0
            }
            _ => {}
        }
    }
    for wave in &arena.waves {
        if wave.kind == WaveKind::Water && arena.map.on_island(q) {
            continue;
        }
        let distance = q.minus(wave.position).length();
        let gap = distance - wave.radius;
        if distance < wave.maximum + PLAYER_RADIUS + 0.6 && gap > -1.6 && gap < 4.0 {
            score += 6.0;
        }
    }
    if boss.guardian == Guardian::Devin {
        // The vines, where they are and where the turn is taking them.
        let mut ahead = *boss;
        ahead.spin += 0.35;
        for flower in [boss, &ahead] {
            for arm in 0..3 {
                for k in 0..flower.vines[arm] as usize {
                    if q.minus(flower.vine_point(arm, k)).length()
                        < crate::arena::devin::SEGMENT_REACH + PLAYER_RADIUS + 2.0
                    {
                        score += 10.0;
                    }
                }
            }
        }
    }
    if boss.guardian == Guardian::ZCode
        && boss.attack == Attack::Sweep
        && boss.state == BossState::Striking
    {
        // Ground the sweep is about to cross, unless a column shades it.
        let eye = arena.zcode_eye();
        let now = PI - PI * boss.progress();
        let at = q.minus(eye).angle();
        if (now - 0.4..=now).contains(&at) && !arena.map.cover_hit(eye, q) {
            score += 8.0;
        }
    }
    if boss.guardian == Guardian::Grok && boss.detached && boss.state == BossState::Striking {
        // The blade's swing still to come.
        if segment_distance(q, boss.hands[0], boss.target)
            < crate::arena::combat::GROK_BLADE_RADIUS + PLAYER_RADIUS + 3.0
        {
            score += 10.0;
        }
    }
    for beam in &arena.beams {
        if segment_distance(q, beam.from, beam.to) < beam.width + PLAYER_RADIUS + 1.5 {
            score += 10.0;
        }
    }
    for p in &arena.projectiles {
        let r = q.minus(p.position);
        let v = p.velocity;
        let t = (r.dot(v) / v.dot(v).max(0.001)).clamp(0.0, 0.6);
        if r.minus(v.scale(t)).length() < 1.7 + PLAYER_RADIUS + 1.0 {
            score += 8.0;
        }
    }
    if boss.guardian == Guardian::Copilot && !arena.map.inside(q, 9.0) {
        score += 1.0;
    }
    score
}

fn steer(arena: &Arena, goal: Vec2) -> Vec2 {
    let player = arena.player.position;
    if goal.minus(player).length() < 1.0 {
        return Vec2::default();
    }
    let desired = goal.minus(player).angle();
    (0..16)
        .map(|ix| {
            let direction = Vec2::from_angle(desired + ix as f32 * TAU / 16.0);
            let target = player.plus(direction.scale(4.0));
            let projected = arena.map.walk(player, target, PLAYER_RADIUS);
            let blocked = target.minus(projected).length() * 8.0;
            (
                direction,
                projected.minus(goal).length() + blocked + threat(arena, projected) * 4.0,
            )
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap()
        .0
}

/// The safest nearby direction when standing in visible danger, and
/// whether the danger is serious enough to roll.
fn escape(arena: &Arena, goal: Vec2) -> Option<(Vec2, bool)> {
    let player = arena.player.position;
    let here = threat(arena, player);
    if here < 1.0 {
        return None;
    }
    let rolling = !arena.player.swimming && arena.player.roll_cooldown <= 0.0;
    let reach = if rolling { 14.0 } else { 6.0 };
    let (direction, _) = (0..24)
        .map(|ix| {
            let direction = Vec2::from_angle(ix as f32 * TAU / 24.0);
            let mid = arena.map.walk(
                player,
                player.plus(direction.scale(reach * 0.5)),
                PLAYER_RADIUS,
            );
            let end = arena
                .map
                .walk(player, player.plus(direction.scale(reach)), PLAYER_RADIUS);
            let score = threat(arena, end) * 1.5
                + threat(arena, mid) * 0.5
                + end.minus(goal).length() * 0.02
                - end.minus(player).length() * 0.1;
            (direction, score)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    Some((direction, here >= 6.0))
}

/// The pilot reads visible tells and sends ordinary input only. It never
/// changes positions, timers, armor, health, phase, damage or the random seed.
fn winnable(guardian: Guardian) {
    for seed in [1, 17, 127] {
        let mut arena = Arena::from_preview(Boss::new(guardian), 0.0, seed);
        let mut pilot = Pilot::default();
        for _ in 0..(300.0 / STEP) as usize {
            if arena.phase == Phase::Victory {
                break;
            }
            pilot.tick(&mut arena);
        }
        if std::env::var_os("VIBEX_PILOT_LOG").is_some() {
            eprintln!(
                "{guardian:?} seed {seed}: {:?} after {:.1}s, {} deaths",
                arena.phase, arena.run_time, arena.deaths
            );
        }
        assert_eq!(
            arena.phase,
            Phase::Victory,
            "{guardian:?} seed {seed}: deaths={} player={:?} boss={:?} arrow={:?}",
            arena.deaths,
            arena.player.position,
            arena.boss,
            arena.arrow
        );
    }
}

#[test]
fn the_spark_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::Claude);
}
#[test]
fn the_knot_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::Codex);
}
#[test]
fn the_glyph_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::Pi);
}
#[test]
fn the_chest_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::OpenCode);
}
#[test]
fn the_whale_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::DeepSeek);
}
#[test]
fn the_helmet_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::Copilot);
}
#[test]
fn the_cube_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::Cursor);
}
#[test]
fn the_robot_head_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::Cline);
}
#[test]
fn the_slashed_ring_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::Grok);
}
#[test]
fn the_gate_slab_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::ZCode);
}
#[test]
fn the_tile_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::Kimi);
}
#[test]
fn the_cat_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::CodeBuddy);
}
#[test]
fn the_arch_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::Antigravity);
}
#[test]
fn the_flower_is_winnable_through_ordinary_inputs() {
    winnable(Guardian::Devin);
}
