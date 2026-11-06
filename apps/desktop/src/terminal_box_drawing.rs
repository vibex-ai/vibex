//! Vector geometry for the Unicode Box Drawing block.
//!
//! A font draws `┌` for its own em square, with its own stroke weight and its
//! own idea of where the arms meet. Adjacent cells then disagree by a fraction
//! of a pixel, which is exactly where a table or a panel border starts to look
//! broken: the horizontal run is one glyph tall here and two there, and the
//! corner does not meet the line it continues.
//!
//! This module ignores the glyph and draws the character from its arm
//! description instead. Every arm is a line from the cell edge to the cell
//! centre, at a thickness derived from the measured cell, so a horizontal run
//! is one continuous rectangle and a corner is exactly the two rectangles that
//! meet there. Rounded corners and diagonals are the two shapes a rectangle
//! cannot express; those are painted as paths.
//!
//! Only cells whose text is a single Box Drawing code point are handled here;
//! [`parse`] returns `None` for everything else.

use gpui::{Bounds, Hsla, PathBuilder, Pixels, Window, fill, point, px, size};

/// The style of one of a glyph's four arms.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Arm {
    #[default]
    None,
    Light,
    Heavy,
    Double,
}

impl Arm {
    fn is_arm(self) -> bool {
        self != Self::None
    }
}

/// A dashed arm's rhythm, in strokes per cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dash {
    /// Three evenly spaced dashes over the cell.
    Triple,
    /// Four evenly spaced dashes over the cell.
    Quadruple,
    /// Two evenly spaced dashes over the cell.
    Double,
}

/// The diagonal strokes a few Box Drawing characters carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diagonal {
    /// `╱`: upper right to lower left.
    UpperRightToLowerLeft,
    /// `╲`: upper left to lower right.
    UpperLeftToLowerRight,
    /// `╳`: both diagonals.
    Cross,
}

/// What a Box Drawing character is made of.
///
/// The four arms are independent because the block distinguishes light, heavy
/// and double weights per arm (`┍` is a light down arm with a heavy right arm).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BoxArms {
    pub left: Arm,
    pub right: Arm,
    pub up: Arm,
    pub down: Arm,
    /// Set for the dashed characters. Their arms still carry the weight.
    pub dash: Option<Dash>,
    /// `╭ ╮ ╯ ╰`: the corner is a quarter arc instead of a right angle.
    pub rounded: bool,
    /// Set for `╱ ╲ ╳`, whose arms are all `None`.
    pub diagonal: Option<Diagonal>,
}

impl BoxArms {
    /// Whether the glyph is a horizontal run that adjacent identical glyphs can
    /// extend. A merged run is one rectangle instead of one per cell.
    pub fn is_extendable_horizontal(&self) -> bool {
        self.diagonal.is_none()
            && !self.rounded
            && self.up == Arm::None
            && self.down == Arm::None
            && self.left == self.right
            && self.left.is_arm()
    }

    fn has_horizontal_double(&self) -> bool {
        self.left == Arm::Double || self.right == Arm::Double
    }

    fn has_vertical_double(&self) -> bool {
        self.up == Arm::Double || self.down == Arm::Double
    }

    fn is_empty(&self) -> bool {
        !self.left.is_arm()
            && !self.right.is_arm()
            && !self.up.is_arm()
            && !self.down.is_arm()
            && self.diagonal.is_none()
    }
}

/// A run of a Box Drawing character, in cells.
#[derive(Clone, Debug)]
pub struct BoxStroke {
    pub row: u16,
    pub column: u16,
    /// How many adjacent cells this run covers. Greater than one only for a
    /// merged horizontal run.
    pub columns: u16,
    pub arms: BoxArms,
    pub color: Hsla,
}

impl BoxStroke {
    pub fn new(row: u16, column: u16, arms: BoxArms, color: Hsla) -> Self {
        Self {
            row,
            column,
            columns: 1,
            arms,
            color,
        }
    }

    /// Whether a neighbouring cell continues this run exactly.
    pub fn continues(&self, row: u16, column: u16, arms: BoxArms, color: Hsla) -> bool {
        self.row == row
            && self.column + self.columns == column
            && self.arms == arms
            && self.color == color
            && arms.is_extendable_horizontal()
    }

    pub fn extend(&mut self) {
        self.columns = self.columns.saturating_add(1);
    }
}

/// The arm description of a Box Drawing character, or `None` for any other
/// character.
pub fn parse(ch: char) -> Option<BoxArms> {
    use Arm::{Double as D, Heavy as H, Light as L, None as N};
    let arms = |left, right, up, down| BoxArms {
        left,
        right,
        up,
        down,
        ..BoxArms::default()
    };
    let dashed = |left, right, up, down, dash| BoxArms {
        left,
        right,
        up,
        down,
        dash: Some(dash),
        ..BoxArms::default()
    };
    let rounded = |left, right, up, down| BoxArms {
        left,
        right,
        up,
        down,
        rounded: true,
        ..BoxArms::default()
    };
    let diagonal = |diagonal| BoxArms {
        diagonal: Some(diagonal),
        ..BoxArms::default()
    };
    let parsed = match ch {
        // Horizontal and vertical runs.
        '─' => arms(L, L, N, N),
        '━' => arms(H, H, N, N),
        '│' => arms(N, N, L, L),
        '┃' => arms(N, N, H, H),
        // Dashed runs.
        '┄' => dashed(L, L, N, N, Dash::Triple),
        '┅' => dashed(H, H, N, N, Dash::Triple),
        '┆' => dashed(N, N, L, L, Dash::Triple),
        '┇' => dashed(N, N, H, H, Dash::Triple),
        '┈' => dashed(L, L, N, N, Dash::Quadruple),
        '┉' => dashed(H, H, N, N, Dash::Quadruple),
        '┊' => dashed(N, N, L, L, Dash::Quadruple),
        '┋' => dashed(N, N, H, H, Dash::Quadruple),
        '╌' => dashed(L, L, N, N, Dash::Double),
        '╍' => dashed(H, H, N, N, Dash::Double),
        '╎' => dashed(N, N, L, L, Dash::Double),
        '╏' => dashed(N, N, H, H, Dash::Double),
        // Down-and-right and down-and-left corners.
        '┌' => arms(N, L, N, L),
        '┍' => arms(N, H, N, L),
        '┎' => arms(N, L, N, H),
        '┏' => arms(N, H, N, H),
        '┐' => arms(L, N, N, L),
        '┑' => arms(H, N, N, L),
        '┒' => arms(L, N, N, H),
        '┓' => arms(H, N, N, H),
        // Up-and-right and up-and-left corners.
        '└' => arms(N, L, L, N),
        '┕' => arms(N, H, L, N),
        '┖' => arms(N, L, H, N),
        '┗' => arms(N, H, H, N),
        '┘' => arms(L, N, L, N),
        '┙' => arms(H, N, L, N),
        '┚' => arms(L, N, H, N),
        '┛' => arms(H, N, H, N),
        // Vertical with a right arm.
        '├' => arms(N, L, L, L),
        '┝' => arms(N, H, L, L),
        '┞' => arms(N, L, H, L),
        '┟' => arms(N, L, L, H),
        '┠' => arms(N, L, H, H),
        '┡' => arms(N, H, H, L),
        '┢' => arms(N, H, L, H),
        '┣' => arms(N, H, H, H),
        // Vertical with a left arm.
        '┤' => arms(L, N, L, L),
        '┥' => arms(H, N, L, L),
        '┦' => arms(L, N, H, L),
        '┧' => arms(L, N, L, H),
        '┨' => arms(L, N, H, H),
        '┩' => arms(H, N, H, L),
        '┪' => arms(H, N, L, H),
        '┫' => arms(H, N, H, H),
        // Horizontal with a down arm.
        '┬' => arms(L, L, N, L),
        '┭' => arms(H, L, N, L),
        '┮' => arms(L, H, N, H),
        '┯' => arms(H, H, N, L),
        '┰' => arms(L, L, N, H),
        '┱' => arms(H, L, N, H),
        '┲' => arms(L, H, N, H),
        '┳' => arms(H, H, N, H),
        // Horizontal with an up arm.
        '┴' => arms(L, L, L, N),
        '┵' => arms(H, L, L, N),
        '┶' => arms(L, H, L, N),
        '┷' => arms(H, H, L, N),
        '┸' => arms(L, L, H, N),
        '┹' => arms(H, L, H, N),
        '┺' => arms(L, H, H, N),
        '┻' => arms(H, H, H, N),
        // Four-way junctions.
        '┼' => arms(L, L, L, L),
        '┽' => arms(H, L, L, L),
        '┾' => arms(L, H, L, L),
        '┿' => arms(H, H, L, L),
        '╀' => arms(L, L, H, L),
        '╁' => arms(L, L, L, H),
        '╂' => arms(L, L, H, H),
        '╃' => arms(H, L, H, L),
        '╄' => arms(L, H, H, L),
        '╅' => arms(H, L, L, H),
        '╆' => arms(L, H, L, H),
        '╇' => arms(H, H, H, L),
        '╈' => arms(H, H, L, H),
        '╉' => arms(L, L, H, H),
        '╊' => arms(H, L, H, H),
        '╋' => arms(H, H, H, H),
        // Double runs.
        '═' => arms(D, D, N, N),
        '║' => arms(N, N, D, D),
        // Double corners.
        '╒' => arms(N, D, N, L),
        '╓' => arms(N, L, N, D),
        '╔' => arms(N, D, N, D),
        '╕' => arms(D, N, N, L),
        '╖' => arms(L, N, N, D),
        '╗' => arms(D, N, N, D),
        '╘' => arms(N, D, L, N),
        '╙' => arms(N, L, D, N),
        '╚' => arms(N, D, D, N),
        '╛' => arms(D, N, L, N),
        '╜' => arms(L, N, D, N),
        '╝' => arms(D, N, D, N),
        // Double verticals and horizontals with the opposite single arm.
        '╞' => arms(N, D, L, L),
        '╟' => arms(N, L, D, D),
        '╠' => arms(N, D, D, D),
        '╡' => arms(D, N, L, L),
        '╢' => arms(L, N, D, D),
        '╣' => arms(D, N, D, D),
        '╤' => arms(D, D, N, L),
        '╥' => arms(L, L, N, D),
        '╦' => arms(D, D, N, D),
        '╧' => arms(D, D, L, N),
        '╨' => arms(L, L, D, N),
        '╩' => arms(D, D, D, N),
        '╪' => arms(D, D, L, L),
        '╫' => arms(L, L, D, D),
        '╬' => arms(D, D, D, D),
        // Rounded corners.
        '╭' => rounded(N, L, N, L),
        '╮' => rounded(L, N, N, L),
        '╯' => rounded(L, N, L, N),
        '╰' => rounded(N, L, L, N),
        // Diagonals.
        '╱' => diagonal(Diagonal::UpperRightToLowerLeft),
        '╲' => diagonal(Diagonal::UpperLeftToLowerRight),
        '╳' => diagonal(Diagonal::Cross),
        // Half lines.
        '╴' => arms(L, N, N, N),
        '╵' => arms(N, N, L, N),
        '╶' => arms(N, L, N, N),
        '╷' => arms(N, N, N, L),
        '╸' => arms(H, N, N, N),
        '╹' => arms(N, N, H, N),
        '╺' => arms(N, H, N, N),
        '╻' => arms(N, N, N, H),
        '╼' => arms(L, H, N, N),
        '╽' => arms(N, N, L, H),
        '╾' => arms(H, L, N, N),
        '╿' => arms(N, N, H, L),
        _ => return None,
    };
    (!parsed.is_empty()).then_some(parsed)
}

/// Whether `ch` is one of the characters this module draws.
pub fn is_box_drawing(ch: char) -> bool {
    parse(ch).is_some()
}

/// Paints a row's worth of runs.
///
/// `bounds` is the origin of the grid's first cell; `cell_width` and
/// `cell_height` are the measured cell. The caller owns the content mask.
pub fn paint(
    bounds: Bounds<Pixels>,
    cell_width: f32,
    cell_height: f32,
    strokes: &[BoxStroke],
    window: &mut Window,
) {
    if cell_width <= 0.0 || cell_height <= 0.0 {
        return;
    }
    let geometry = Geometry::new(cell_height, window.scale_factor());
    for stroke in strokes {
        geometry.paint(bounds, cell_width, cell_height, stroke, window);
    }
}

struct Geometry {
    scale: f32,
    light: f32,
    heavy: f32,
    double_offset: f32,
}

impl Geometry {
    fn new(cell_height: f32, scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        // A 13px code font has an 18px cell; a 1px light rule and a 2px heavy
        // one keep a table legible without turning a dense TUI into a grid of
        // bars.
        let light = (cell_height * 0.06).clamp(1.0, 2.0);
        Self {
            scale,
            light,
            heavy: light * 2.0,
            double_offset: light * 1.5,
        }
    }

    fn snap(&self, value: f32) -> f32 {
        (value * self.scale).round() / self.scale
    }

    fn thickness(&self, arm: Arm) -> f32 {
        match arm {
            Arm::Heavy => self.heavy,
            _ => self.light,
        }
    }

    /// A filled rectangle with every edge on a device pixel.
    fn quad(&self, x0: f32, x1: f32, y0: f32, y1: f32, color: Hsla, window: &mut Window) {
        let left = self.snap(x0.min(x1));
        let right = self.snap(x0.max(x1));
        let top = self.snap(y0.min(y1));
        let bottom = self.snap(y0.max(y1));
        if right <= left || bottom <= top {
            return;
        }
        window.paint_quad(fill(
            Bounds::new(
                point(px(left), px(top)),
                size(px(right - left), px(bottom - top)),
            ),
            color,
        ));
    }

    fn horizontal(
        &self,
        x0: f32,
        x1: f32,
        center_y: f32,
        thickness: f32,
        color: Hsla,
        window: &mut Window,
    ) {
        self.quad(
            x0,
            x1,
            center_y - thickness / 2.0,
            center_y + thickness / 2.0,
            color,
            window,
        );
    }

    fn vertical(
        &self,
        x_center: f32,
        y0: f32,
        y1: f32,
        thickness: f32,
        color: Hsla,
        window: &mut Window,
    ) {
        self.quad(
            x_center - thickness / 2.0,
            x_center + thickness / 2.0,
            y0,
            y1,
            color,
            window,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn dashed(
        &self,
        from: (f32, f32),
        to: (f32, f32),
        thickness: f32,
        dash: Dash,
        cell_width: f32,
        cell_height: f32,
        color: Hsla,
        window: &mut Window,
    ) {
        let horizontal = (from.1 - to.1).abs() < 0.01;
        let length = if horizontal { cell_width } else { cell_height };
        let (segments, gap_fraction) = match dash {
            Dash::Triple => (3.0, 0.33),
            Dash::Quadruple => (4.0, 0.30),
            Dash::Double => (2.0, 0.35),
        };
        let period = length / segments;
        let dash_length = (period * (1.0 - gap_fraction)).max(self.light);
        let gap = (period - dash_length).max(self.light * 0.5);
        let mut builder =
            PathBuilder::stroke(px(thickness)).dash_array(&[px(dash_length), px(gap)]);
        builder.move_to(point(px(from.0), px(from.1)));
        builder.line_to(point(px(to.0), px(to.1)));
        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }

    fn arc(
        &self,
        from: (f32, f32),
        to: (f32, f32),
        control_a: (f32, f32),
        control_b: (f32, f32),
        color: Hsla,
        window: &mut Window,
    ) {
        let mut builder = PathBuilder::stroke(px(self.light));
        builder.move_to(point(px(from.0), px(from.1)));
        builder.cubic_bezier_to(
            point(px(to.0), px(to.1)),
            point(px(control_a.0), px(control_a.1)),
            point(px(control_b.0), px(control_b.1)),
        );
        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }

    fn diagonal(&self, from: (f32, f32), to: (f32, f32), color: Hsla, window: &mut Window) {
        let mut builder = PathBuilder::stroke(px(self.light));
        builder.move_to(point(px(from.0), px(from.1)));
        builder.line_to(point(px(to.0), px(to.1)));
        if let Ok(path) = builder.build() {
            window.paint_path(path, color);
        }
    }

    fn paint(
        &self,
        bounds: Bounds<Pixels>,
        cell_width: f32,
        cell_height: f32,
        stroke: &BoxStroke,
        window: &mut Window,
    ) {
        let arms = stroke.arms;
        let width = cell_width * f32::from(stroke.columns);
        let x0 = f32::from(bounds.origin.x) + f32::from(stroke.column) * cell_width;
        let y0 = f32::from(bounds.origin.y) + f32::from(stroke.row) * cell_height;
        let x1 = x0 + width;
        let y1 = y0 + cell_height;
        let cx = x0 + width / 2.0;
        let cy = y0 + cell_height / 2.0;
        let color = stroke.color;
        let o = self.double_offset;

        if let Some(diagonal) = arms.diagonal {
            match diagonal {
                Diagonal::UpperRightToLowerLeft => self.diagonal((x1, y0), (x0, y1), color, window),
                Diagonal::UpperLeftToLowerRight => self.diagonal((x0, y0), (x1, y1), color, window),
                Diagonal::Cross => {
                    self.diagonal((x1, y0), (x0, y1), color, window);
                    self.diagonal((x0, y0), (x1, y1), color, window);
                }
            }
            return;
        }

        if arms.rounded {
            self.paint_rounded(x0, x1, y0, y1, cx, cy, arms, color, window);
            return;
        }

        let has_h_double = arms.has_horizontal_double();
        let has_v_double = arms.has_vertical_double();

        // Horizontal arms. A double arm is two parallel rules; each one stops
        // at the perpendicular double line on its own side, which is what makes
        // `╔` close its outer and inner corners instead of drawing a cross.
        for (arm, is_left) in [(arms.left, true), (arms.right, false)] {
            if !arm.is_arm() {
                continue;
            }
            match arm {
                Arm::Double => {
                    for side in [-1.0_f32, 1.0] {
                        let line_y = cy + side * o;
                        let start = if arms.left.is_arm() {
                            x0
                        } else {
                            cx + side * o
                        };
                        let end = if arms.right.is_arm() {
                            x1
                        } else {
                            cx - side * o
                        };
                        if let Some(dash) = arms.dash {
                            let (from, to) = if is_left { (x0, end) } else { (start, x1) };
                            self.dashed(
                                (from, line_y),
                                (to, line_y),
                                self.light,
                                dash,
                                cell_width,
                                cell_height,
                                color,
                                window,
                            );
                        } else {
                            self.horizontal(start, end, line_y, self.light, color, window);
                        }
                    }
                }
                Arm::Light | Arm::Heavy => {
                    let thickness = self.thickness(arm);
                    let start = if is_left {
                        x0
                    } else if has_v_double {
                        // Cross the near double rule and stop on the far one.
                        cx - o
                    } else {
                        cx
                    };
                    let end = if !is_left {
                        x1
                    } else if has_v_double {
                        cx + o
                    } else {
                        cx
                    };
                    if let Some(dash) = arms.dash {
                        self.dashed(
                            (start, cy),
                            (end, cy),
                            thickness,
                            dash,
                            cell_width,
                            cell_height,
                            color,
                            window,
                        );
                    } else {
                        self.horizontal(start, end, cy, thickness, color, window);
                    }
                }
                Arm::None => {}
            }
        }

        // Vertical arms, with the same junction rule on the other axis.
        for (arm, is_up) in [(arms.up, true), (arms.down, false)] {
            if !arm.is_arm() {
                continue;
            }
            match arm {
                Arm::Double => {
                    for side in [-1.0_f32, 1.0] {
                        let line_x = cx + side * o;
                        let start = if arms.up.is_arm() { y0 } else { cy + side * o };
                        let end = if arms.down.is_arm() {
                            y1
                        } else {
                            cy - side * o
                        };
                        if let Some(dash) = arms.dash {
                            let (from, to) = if is_up { (y0, end) } else { (start, y1) };
                            self.dashed(
                                (line_x, from),
                                (line_x, to),
                                self.light,
                                dash,
                                cell_width,
                                cell_height,
                                color,
                                window,
                            );
                        } else {
                            self.vertical(line_x, start, end, self.light, color, window);
                        }
                    }
                }
                Arm::Light | Arm::Heavy => {
                    let thickness = self.thickness(arm);
                    let start = if is_up {
                        y0
                    } else if has_h_double {
                        cy - o
                    } else {
                        cy
                    };
                    let end = if !is_up {
                        y1
                    } else if has_h_double {
                        cy + o
                    } else {
                        cy
                    };
                    if let Some(dash) = arms.dash {
                        self.dashed(
                            (cx, start),
                            (cx, end),
                            thickness,
                            dash,
                            cell_width,
                            cell_height,
                            color,
                            window,
                        );
                    } else {
                        self.vertical(cx, start, end, thickness, color, window);
                    }
                }
                Arm::None => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_rounded(
        &self,
        x0: f32,
        x1: f32,
        y0: f32,
        y1: f32,
        cx: f32,
        cy: f32,
        arms: BoxArms,
        color: Hsla,
        window: &mut Window,
    ) {
        let radius = (cx - x0).min(cy - y0) * 0.55;
        let k = radius * 0.5523;
        // From the horizontal arm, round the corner, then leave along the
        // vertical arm. Each corner is the same arc rotated.
        let (from, to, control_a, control_b) = if arms.right.is_arm() && arms.down.is_arm() {
            (
                (cx + radius, cy),
                (cx, cy + radius),
                (cx + radius - k, cy),
                (cx, cy + radius - k),
            )
        } else if arms.left.is_arm() && arms.down.is_arm() {
            (
                (cx - radius, cy),
                (cx, cy + radius),
                (cx - radius + k, cy),
                (cx, cy + radius - k),
            )
        } else if arms.left.is_arm() && arms.up.is_arm() {
            (
                (cx - radius, cy),
                (cx, cy - radius),
                (cx - radius + k, cy),
                (cx, cy - radius + k),
            )
        } else {
            (
                (cx + radius, cy),
                (cx, cy - radius),
                (cx + radius - k, cy),
                (cx, cy - radius + k),
            )
        };

        // The straight halves run from the cell edge to where the arc starts.
        if arms.right.is_arm() {
            self.horizontal(cx + radius, x1, cy, self.light, color, window);
        }
        if arms.left.is_arm() {
            self.horizontal(x0, cx - radius, cy, self.light, color, window);
        }
        if arms.down.is_arm() {
            self.vertical(cx, cy + radius, y1, self.light, color, window);
        }
        if arms.up.is_arm() {
            self.vertical(cx, y0, cy - radius, self.light, color, window);
        }
        self.arc(from, to, control_a, control_b, color, window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_whole_box_drawing_block() {
        for codepoint in 0x2500..=0x257F {
            let ch = char::from_u32(codepoint).expect("valid scalar");
            assert!(
                is_box_drawing(ch),
                "U+{codepoint:04X} ({ch}) should be drawn from geometry"
            );
        }
        assert!(!is_box_drawing('a'));
        assert!(!is_box_drawing('█'));
        assert!(!is_box_drawing('é'));
    }

    #[test]
    fn distinguishes_light_heavy_and_double_arms() {
        assert_eq!(
            parse('─').unwrap(),
            BoxArms {
                left: Arm::Light,
                right: Arm::Light,
                ..BoxArms::default()
            }
        );
        assert_eq!(
            parse('┃').unwrap(),
            BoxArms {
                up: Arm::Heavy,
                down: Arm::Heavy,
                ..BoxArms::default()
            }
        );
        assert_eq!(
            parse('╬').unwrap(),
            BoxArms {
                left: Arm::Double,
                right: Arm::Double,
                up: Arm::Double,
                down: Arm::Double,
                ..BoxArms::default()
            }
        );
        // A mixed weight corner keeps each arm's own weight.
        assert_eq!(
            parse('┍').unwrap(),
            BoxArms {
                right: Arm::Heavy,
                down: Arm::Light,
                ..BoxArms::default()
            }
        );
    }

    #[test]
    fn only_horizontal_runs_can_be_extended() {
        assert!(parse('─').unwrap().is_extendable_horizontal());
        assert!(parse('═').unwrap().is_extendable_horizontal());
        assert!(parse('╌').unwrap().is_extendable_horizontal());
        assert!(!parse('┼').unwrap().is_extendable_horizontal());
        assert!(!parse('│').unwrap().is_extendable_horizontal());
        assert!(!parse('╭').unwrap().is_extendable_horizontal());
        assert!(!parse('╱').unwrap().is_extendable_horizontal());
    }

    #[test]
    fn a_run_merges_only_with_an_identical_neighbour() {
        let color = Hsla::white();
        let mut stroke = BoxStroke::new(3, 2, parse('─').unwrap(), color);
        assert!(stroke.continues(3, 3, parse('─').unwrap(), color));
        assert!(
            !stroke.continues(3, 4, parse('─').unwrap(), color),
            "not adjacent"
        );
        assert!(
            !stroke.continues(4, 3, parse('─').unwrap(), color),
            "different row"
        );
        assert!(
            !stroke.continues(3, 3, parse('┼').unwrap(), color),
            "a junction owns its own cell"
        );
        stroke.extend();
        assert_eq!(stroke.columns, 2);
        assert!(stroke.continues(3, 4, parse('─').unwrap(), color));
    }

    #[test]
    fn rounded_corners_and_half_lines_are_distinguished() {
        assert!(parse('╭').unwrap().rounded);
        assert!(parse('╯').unwrap().rounded);
        assert_eq!(parse('╭').unwrap().right, Arm::Light);
        assert_eq!(parse('╭').unwrap().down, Arm::Light);
        assert_eq!(
            parse('╴').unwrap().left,
            Arm::Light,
            "a half line has one arm and no opposite"
        );
        assert_eq!(parse('╴').unwrap().right, Arm::None);
        assert_eq!(parse('╼').unwrap().left, Arm::Light);
        assert_eq!(parse('╼').unwrap().right, Arm::Heavy);
    }

    #[test]
    fn dashed_characters_name_their_rhythm() {
        assert_eq!(parse('┄').unwrap().dash, Some(Dash::Triple));
        assert_eq!(parse('┈').unwrap().dash, Some(Dash::Quadruple));
        assert_eq!(parse('╌').unwrap().dash, Some(Dash::Double));
        assert_eq!(parse('─').unwrap().dash, None);
    }
}
