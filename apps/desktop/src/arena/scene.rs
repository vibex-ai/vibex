//! One text canvas for the arena, rather than an element tree per glyph.

use std::{cell::Cell, f32::consts::TAU, rc::Rc};

use gpui::{
    AnyElement, App, Bounds, FontFeatures, Hsla, IntoElement, Pixels, Point, ShapedLine,
    Styled as _, TextRun, Window, canvas, font, point, px,
};
use gpui_component::ActiveTheme as _;

use super::combat::{Arena, ArrowState, Attack, EffectKind, Guardian, HEIGHT, Phase, Vec2, WIDTH};

const EMBER: &[&str] = &[
    "  ░  ▒ ░  ░  ",
    "   ▓ ▓ ▓ ▓   ",
    " ▒▒ ▓▓▓▓▓ ▒▒ ",
    "  ▓▓▓▓▓▓▓▓▓  ",
    "▒▓▓▓▓▓▓▓▓▓▓▓▒",
    "  ▓▓▓▓▓▓▓▓▓  ",
    " ▒▒ ▓▓▓▓▓ ▒▒ ",
    "   ▓ ▓ ▓ ▓   ",
    "  ░  ▒ ░  ░  ",
];
const KNOT: &[&str] = &[
    "    ▒████▒    ",
    "  ▒██░  ░██▒  ",
    " ▓█░ ▒██▒ ░█▓ ",
    " ██ ▓█  █▓ ██ ",
    " ▓██▓   ▓██▓  ",
    " ██ ▓█  █▓ ██ ",
    " ▓█░ ▒██▒ ░█▓ ",
    "  ▒██░  ░██▒  ",
    "    ▒████▒    ",
];
const PRISM: &[&str] = &[
    "      ░      ",
    "      ▒      ",
    "     ▒▓▒     ",
    "   ░▒▓█▓▒░   ",
    "░▒▓███████▓▒░",
    "   ░▒▓█▓▒░   ",
    "     ▒▓▒     ",
    "      ▒      ",
    "      ░      ",
];
const HERO: &[&str] = &[" ▴ ", "‹@›", " ╵ "];
const PILLAR: &[&str] = &["╔═╗", "║░║", "╚═╝"];

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Tone {
    #[default]
    Dust,
    Stone,
    Rune,
    Body,
    Hero,
    Core,
    Danger,
    Magic,
    Arrow,
    Ghost,
}

#[derive(Clone, Copy)]
struct Glyph {
    ch: char,
    tone: Tone,
}

struct Frame {
    columns: usize,
    rows: usize,
    cells: Vec<Glyph>,
}

impl Frame {
    fn new(columns: usize, rows: usize) -> Self {
        Self {
            columns,
            rows,
            cells: vec![
                Glyph {
                    ch: ' ',
                    tone: Tone::Dust
                };
                columns * rows
            ],
        }
    }

    fn put(&mut self, x: i32, y: i32, ch: char, tone: Tone) {
        if x >= 0 && y >= 0 && x < self.columns as i32 && y < self.rows as i32 {
            self.cells[y as usize * self.columns + x as usize] = Glyph { ch, tone };
        }
    }

    fn world(&mut self, position: Vec2, ch: char, tone: Tone) {
        self.put(
            position.x.round() as i32,
            (position.y / 2.0).round() as i32,
            ch,
            tone,
        );
    }

    fn sprite(&mut self, center: Vec2, sprite: &[&str], tone: Tone) {
        let width = sprite
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        let left = center.x.round() as i32 - width as i32 / 2;
        let top = (center.y / 2.0).round() as i32 - sprite.len() as i32 / 2;
        for (row, line) in sprite.iter().enumerate() {
            for (column, ch) in line.chars().enumerate() {
                if ch != ' ' {
                    self.put(left + column as i32, top + row as i32, ch, tone);
                }
            }
        }
    }

    fn line(&mut self, from: Vec2, to: Vec2, ch: char, tone: Tone, dashed: bool) {
        let delta = to.minus(from);
        let steps = delta.length().ceil().max(1.0) as usize;
        for ix in 0..=steps.min(240) {
            if !dashed || ix.is_multiple_of(3) {
                self.world(from.plus(delta.scale(ix as f32 / steps as f32)), ch, tone);
            }
        }
    }

    fn ring(&mut self, center: Vec2, radius: f32, ch: char, tone: Tone) {
        let steps = (radius * 5.0).ceil().clamp(12.0, 180.0) as usize;
        for ix in 0..steps {
            let angle = TAU * ix as f32 / steps as f32;
            self.world(
                center.plus(Vec2::new(angle.cos(), angle.sin()).scale(radius)),
                ch,
                tone,
            );
        }
    }

    fn floor(&mut self, time: f32, preview: bool) {
        for row in 0..self.rows {
            for column in 0..self.columns {
                let hash = (column * 73 + row * 137 + column * row * 19) % 127;
                let wave = (column as f32 * 0.095 + row as f32 * 0.19 - time * 0.4).sin();
                let (ch, tone) = if preview {
                    if hash < 10 && wave > 0.45 {
                        ('▪', Tone::Stone)
                    } else {
                        ('▫', Tone::Dust)
                    }
                } else if row % 4 == 0 && column % 8 < 5 {
                    ('─', Tone::Dust)
                } else if hash < 6 {
                    ('░', Tone::Stone)
                } else if hash < 30 {
                    ('·', Tone::Dust)
                } else {
                    (' ', Tone::Dust)
                };
                self.put(column as i32, row as i32, ch, tone);
            }
        }
        if preview {
            return;
        }
        for x in 2..self.columns - 2 {
            self.put(x as i32, 1, '─', Tone::Rune);
            self.put(x as i32, self.rows as i32 - 2, '─', Tone::Rune);
        }
        for y in 1..self.rows - 1 {
            self.put(2, y as i32, '│', Tone::Rune);
            self.put(self.columns as i32 - 3, y as i32, '│', Tone::Rune);
        }
        for (x, y, ch) in [(2, 1, '╭'), (93, 1, '╮'), (2, 26, '╰'), (93, 26, '╯')] {
            self.put(x, y, ch, Tone::Rune);
        }
        self.ring(Vec2::new(48.0, 28.0), 19.0, '·', Tone::Stone);
        self.ring(Vec2::new(48.0, 28.0), 21.0, '·', Tone::Dust);
        for position in [
            Vec2::new(10.0, 9.0),
            Vec2::new(86.0, 9.0),
            Vec2::new(10.0, 46.0),
            Vec2::new(86.0, 46.0),
        ] {
            self.sprite(position, PILLAR, Tone::Stone);
        }
    }

    fn battle(arena: &Arena, reduced_motion: bool) -> Self {
        let mut frame = Self::new(WIDTH as usize, HEIGHT as usize / 2);
        frame.floor(0.0, false);
        if let Some(windup) = arena.boss.windup {
            let progress = 1.0 - windup.remaining / windup.duration;
            match windup.attack {
                Attack::Sunburst => {
                    frame.ring(windup.origin, 8.0 + progress * 3.0, '·', Tone::Danger);
                    for ix in 0..8 {
                        let angle = TAU * ix as f32 / 8.0;
                        let direction = Vec2::new(angle.cos(), angle.sin());
                        frame.line(
                            windup.origin.plus(direction.scale(8.0)),
                            windup.origin.plus(direction.scale(13.0)),
                            '·',
                            Tone::Danger,
                            true,
                        );
                    }
                }
                Attack::Fan | Attack::Charge => {
                    let direction = windup.target.minus(windup.origin).normalized();
                    let end = windup.origin.plus(direction.scale(100.0));
                    frame.line(windup.origin, end, '·', Tone::Danger, true);
                    if windup.attack == Attack::Charge {
                        let side = Vec2::new(-direction.y, direction.x).scale(3.0);
                        frame.line(
                            windup.origin.plus(side),
                            end.plus(side),
                            ':',
                            Tone::Danger,
                            true,
                        );
                        frame.line(
                            windup.origin.minus(side),
                            end.minus(side),
                            ':',
                            Tone::Danger,
                            true,
                        );
                    }
                    frame.world(windup.target, '×', Tone::Danger);
                }
                Attack::Cross => {
                    frame.line(
                        Vec2::new(3.0, windup.target.y),
                        Vec2::new(WIDTH - 3.0, windup.target.y),
                        '·',
                        Tone::Danger,
                        true,
                    );
                    frame.line(
                        Vec2::new(windup.target.x, 3.0),
                        Vec2::new(windup.target.x, HEIGHT - 3.0),
                        ':',
                        Tone::Danger,
                        true,
                    );
                    frame.ring(windup.target, 2.5, '×', Tone::Danger);
                }
            }
        }
        if arena.arrow.state == ArrowState::Returning {
            frame.line(
                arena.arrow.position,
                arena.player.position,
                '·',
                Tone::Magic,
                true,
            );
        }
        for effect in &arena.effects {
            let progress = 1.0 - effect.remaining / effect.duration;
            match effect.kind {
                EffectKind::Cross => {
                    frame.line(
                        Vec2::new(3.0, effect.position.y),
                        Vec2::new(WIDTH - 3.0, effect.position.y),
                        '═',
                        Tone::Danger,
                        false,
                    );
                    frame.line(
                        Vec2::new(effect.position.x, 3.0),
                        Vec2::new(effect.position.x, HEIGHT - 3.0),
                        '║',
                        Tone::Danger,
                        false,
                    );
                }
                EffectKind::Focus => {
                    if !reduced_motion {
                        frame.ring(effect.position, 6.0 + progress * 25.0, '·', Tone::Magic);
                    }
                }
                EffectKind::Roll => {
                    if !reduced_motion {
                        frame.sprite(effect.position, &["·:·"], Tone::Ghost);
                    }
                }
                EffectKind::Impact | EffectKind::Catch | EffectKind::Victory => {
                    let tone = if effect.kind == EffectKind::Impact {
                        Tone::Danger
                    } else {
                        Tone::Magic
                    };
                    frame.ring(effect.position, 2.0 + progress * 7.0, '+', tone);
                }
            }
        }
        if arena.boss.guardian == Guardian::Prism {
            frame.sprite(arena.boss.mirage(), PRISM, Tone::Ghost);
            frame.world(arena.boss.mirage(), '◇', Tone::Ghost);
        }
        if arena.phase != Phase::Victory {
            frame.sprite(
                arena.boss.position,
                guardian_sprite(arena.boss.guardian),
                Tone::Body,
            );
            frame.world(
                arena.boss.core,
                if arena.boss.exposed > 0.0 {
                    '◆'
                } else {
                    '◇'
                },
                if arena.boss.exposed > 0.0 {
                    Tone::Core
                } else {
                    Tone::Stone
                },
            );
            if arena.boss.exposed > 0.0 {
                frame.ring(arena.boss.core, 3.0, '·', Tone::Core);
            }
        } else {
            frame.sprite(
                arena.boss.position,
                &[" · + · ", "+  ◆  +", " · + · "],
                Tone::Core,
            );
        }
        for projectile in &arena.projectiles {
            let tail = projectile
                .position
                .minus(projectile.velocity.normalized().scale(1.8));
            frame.world(tail, '·', Tone::Danger);
            frame.world(projectile.position, '✦', Tone::Danger);
        }
        if arena.arrow.state != ArrowState::Ready {
            let direction = arena.arrow.velocity;
            let glyph = if direction.x.abs() > direction.y.abs() {
                if direction.x > 0.0 { '›' } else { '‹' }
            } else if direction.y > 0.0 {
                '↓'
            } else {
                '↑'
            };
            let tone = if arena.arrow.state == ArrowState::Returning {
                Tone::Magic
            } else {
                Tone::Arrow
            };
            frame.world(arena.arrow.position, glyph, tone);
            if arena.arrow.state == ArrowState::Lodged {
                frame.ring(arena.arrow.position, 1.8, '·', Tone::Arrow);
            }
        }
        let hero_tone = if arena.phase == Phase::Defeat {
            Tone::Ghost
        } else if arena.player.invulnerable > 0.0 {
            Tone::Magic
        } else {
            Tone::Hero
        };
        frame.sprite(arena.player.position, HERO, hero_tone);
        if arena.player.charge > 0.0 {
            frame.ring(
                arena.player.position,
                3.0,
                if arena.player.charge >= 0.22 {
                    '+'
                } else {
                    '·'
                },
                Tone::Arrow,
            );
            frame.world(
                arena.player.position.plus(arena.player.facing.scale(3.5)),
                '↑',
                Tone::Arrow,
            );
        }
        frame
    }

    fn preview(columns: usize, rows: usize, phase: f32) -> Self {
        let mut frame = Self::new(columns, rows);
        frame.floor(phase * TAU, true);
        let boss = Vec2::new(columns as f32 * 0.65, rows as f32 * 0.75);
        let hero = Vec2::new(
            columns as f32 * 0.35 + (phase * TAU).sin() * 2.0,
            rows as f32 * 0.95,
        );
        frame.ring(boss, 11.0 + (phase * TAU).sin() * 2.0, '·', Tone::Stone);
        frame.sprite(boss, KNOT, Tone::Rune);
        frame.world(boss, '◇', Tone::Body);
        frame.sprite(hero, HERO, Tone::Body);
        let travel = (0.5 - (phase * TAU).cos() * 0.5).clamp(0.0, 1.0);
        let arrow = hero.plus(boss.minus(hero).scale(travel));
        frame.line(hero, arrow, '·', Tone::Stone, true);
        frame.world(arrow, if phase < 0.5 { '›' } else { '‹' }, Tone::Body);
        frame
    }
}

fn guardian_sprite(guardian: Guardian) -> &'static [&'static str] {
    match guardian {
        Guardian::Ember => EMBER,
        Guardian::Knot => KNOT,
        Guardian::Prism => PRISM,
    }
}

#[derive(Clone, Copy)]
struct Palette {
    foreground: Hsla,
    muted: Hsla,
    primary: Hsla,
    danger: Hsla,
    core: Hsla,
    magic: Hsla,
}

impl Palette {
    fn new(cx: &App) -> Self {
        Self {
            foreground: cx.theme().foreground,
            muted: cx.theme().muted_foreground,
            primary: cx.theme().primary,
            danger: cx.theme().danger,
            core: cx.theme().warning,
            magic: cx.theme().info,
        }
    }

    fn color(self, tone: Tone) -> Hsla {
        match tone {
            Tone::Dust => self.muted.opacity(0.16),
            Tone::Stone => self.muted.opacity(0.34),
            Tone::Rune => self.muted.opacity(0.64),
            Tone::Body => self.foreground.opacity(0.78),
            Tone::Hero => self.primary,
            Tone::Core => self.core,
            Tone::Danger => self.danger,
            Tone::Magic => self.magic,
            Tone::Arrow => self.foreground,
            Tone::Ghost => self.muted.opacity(0.38),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Geometry {
    pub origin: Point<Pixels>,
    pub unit: Pixels,
}

impl Geometry {
    pub fn battle(bounds: Bounds<Pixels>) -> Self {
        let unit = (bounds.size.width / WIDTH).min(bounds.size.height / HEIGHT);
        Self {
            origin: bounds.origin
                + point(
                    (bounds.size.width - unit * WIDTH) * 0.5,
                    (bounds.size.height - unit * HEIGHT) * 0.5,
                ),
            unit,
        }
    }

    pub fn world(self, point: Point<Pixels>) -> Option<Vec2> {
        if self.unit <= px(0.0) {
            return None;
        }
        let x = (point.x - self.origin.x) / self.unit;
        let y = (point.y - self.origin.y) / self.unit;
        ((0.0..WIDTH).contains(&x) && (0.0..HEIGHT).contains(&y)).then_some(Vec2::new(x, y))
    }
}

struct GridLayout {
    lines: Vec<GridRow>,
    geometry: Geometry,
}

struct GridRow {
    line: ShapedLine,
    /// UTF-8 offset, grid column, and semantic ink for each non-blank cell.
    columns: Vec<(usize, usize, Hsla)>,
}

fn shape(
    frame: Frame,
    geometry: Geometry,
    preview: bool,
    window: &mut Window,
    cx: &mut App,
) -> GridLayout {
    if geometry.unit <= px(0.0) {
        return GridLayout {
            lines: Vec::new(),
            geometry,
        };
    }
    let palette = Palette::new(cx);
    let mut glyph_font = font("Lilex");
    glyph_font.features = FontFeatures::disable_ligatures();
    let probe = window.text_system().shape_line(
        "M".into(),
        window.rem_size(),
        &[TextRun {
            len: 1,
            font: glyph_font.clone(),
            color: palette.foreground,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    let font_size = geometry.unit * (window.rem_size() / probe.width().max(px(1.0)));
    let mut lines = Vec::with_capacity(frame.rows);
    for (row, cells) in frame.cells.chunks(frame.columns).enumerate() {
        let mut text = String::with_capacity(frame.columns * 3);
        let mut columns = Vec::with_capacity(frame.columns);
        // Fade the entire scene into the home surface, including its sprites.
        let fade = if preview {
            let y = row as f32 / frame.rows.max(1) as f32;
            let ramp = ((1.0 - y) / 0.52).clamp(0.0, 1.0);
            ramp * ramp * (3.0 - 2.0 * ramp) * 0.70
        } else {
            1.0
        };
        for (column, glyph) in cells.iter().enumerate() {
            if glyph.ch == ' ' {
                continue;
            }
            columns.push((text.len(), column, palette.color(glyph.tone).opacity(fade)));
            text.push(glyph.ch);
        }
        let run = TextRun {
            len: text.len(),
            font: glyph_font.clone(),
            color: palette.foreground,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_line(text.into(), font_size, &[run], None);
        lines.push(GridRow { line, columns });
    }
    GridLayout { lines, geometry }
}

fn paint(_: Bounds<Pixels>, layout: GridLayout, window: &mut Window, _: &mut App) {
    for (row_ix, row) in layout.lines.iter().enumerate() {
        let line = &row.line;
        let top = layout.geometry.origin.y + layout.geometry.unit * (row_ix as f32 * 2.0);
        let baseline =
            top + (layout.geometry.unit * 2.0 - line.ascent - line.descent) * 0.5 + line.ascent;
        let mut previous = None;
        let mut base_x = px(0.0);
        for run in &line.runs {
            for glyph in &run.glyphs {
                let ix = row
                    .columns
                    .partition_point(|(offset, _, _)| *offset <= glyph.index)
                    .saturating_sub(1);
                let Some((_, column, color)) = row.columns.get(ix) else {
                    continue;
                };
                if previous != Some(ix) {
                    previous = Some(ix);
                    base_x = glyph.position.x;
                }
                // Fallback symbols may have a different advance from Lilex.
                // Pin every glyph to its cell so a diamond cannot shift a row.
                let origin = point(
                    layout.geometry.origin.x
                        + layout.geometry.unit * *column as f32
                        + glyph.position.x
                        - base_x,
                    baseline + glyph.position.y,
                );
                if glyph.is_emoji {
                    let _ = window.paint_emoji(origin, run.font_id, glyph.id, line.font_size);
                } else {
                    let _ =
                        window.paint_glyph(origin, run.font_id, glyph.id, line.font_size, *color);
                }
            }
        }
    }
}

pub(super) fn battle(
    arena: &Arena,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    reduced_motion: bool,
) -> AnyElement {
    let frame = Frame::battle(arena, reduced_motion);
    canvas(
        move |layout_bounds, window, cx| {
            bounds.set(Some(layout_bounds));
            shape(frame, Geometry::battle(layout_bounds), false, window, cx)
        },
        paint,
    )
    .size_full()
    .into_any_element()
}

pub(super) fn preview(phase: f32) -> AnyElement {
    canvas(
        move |bounds, window, cx| {
            // Glyph geometry derives from rem; drawing is clipped by the banner.
            let unit = (window.rem_size() * 0.40).max(bounds.size.width / 240.0);
            let columns = ((bounds.size.width / unit).ceil() as usize).clamp(24, 240);
            let rows = ((bounds.size.height / (unit * 2.0)).ceil() as usize).clamp(8, 32);
            let frame = Frame::preview(columns, rows, phase);
            shape(
                frame,
                Geometry {
                    origin: bounds.origin,
                    unit,
                },
                true,
                window,
                cx,
            )
        },
        paint,
    )
    .size_full()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::size;

    #[test]
    fn pointer_coordinates_follow_the_letterboxed_arena() {
        for (width, height) in [(960.0, 560.0), (320.0, 420.0), (1400.0, 400.0)] {
            let bounds = Bounds::new(point(px(17.0), px(31.0)), size(px(width), px(height)));
            let geometry = Geometry::battle(bounds);
            let center = geometry.origin + point(geometry.unit * 48.0, geometry.unit * 28.0);
            let world = geometry.world(center).unwrap();
            assert!((world.x - 48.0).abs() < 0.001);
            assert!((world.y - 28.0).abs() < 0.001);
            assert!(
                geometry
                    .world(geometry.origin - point(px(1.0), px(1.0)))
                    .is_none()
            );
        }
    }

    #[test]
    fn every_guardian_and_extreme_preview_stays_in_a_bounded_grid() {
        for guardian in Guardian::ALL {
            let arena = Arena::new(guardian);
            let frame = Frame::battle(&arena, false);
            assert_eq!(frame.cells.len(), 96 * 28);
            assert!(frame.cells.iter().any(|glyph| glyph.ch == '@'));
            assert!(frame.cells.iter().any(|glyph| glyph.ch == '◇'));
        }
        for columns in [24, 80, 240] {
            let frame = Frame::preview(columns, 16, 0.5);
            assert_eq!(frame.cells.len(), columns * 16);
        }
    }
}
