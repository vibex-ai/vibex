//! The wordmark a reader lands on.
//!
//! The first screen of a client is the only one that is allowed to be a
//! *picture*: there is nothing to read yet, so the space is spent saying who
//! this is and what to do next rather than on an empty transcript. The mark is
//! drawn from text — a terminal has no image pipeline, and a mark made of
//! characters keeps its shape at every font size — and it moves, because
//! movement is what says the client is alive and waiting and a static block of
//! glyphs cannot.
//!
//! # The two marks
//!
//! [`MarkStyle`] is the reader's choice, and the two styles are two readings of
//! the same word rather than two logos. One is a solid mark with a light
//! crossing it; the other is the same letters with a corroded fill, torn and
//! thrown out of register in bursts. Both keep the letterforms, because the
//! mark's job is to say the product's name — a style that dissolved the word
//! would be a picture of a fault rather than a greeting.
//!
//! # The light
//!
//! One soft band of silver-white crosses the word, pauses, and comes round
//! again. The word never changes while it does: a pass that drew the letters as
//! it went would make a page that is waiting look like a page that is loading.
//!
//! Three things about the light are deliberate and easy to get wrong:
//!
//! * **it brightens the mark; it does not dim it.** The cells under the light
//!   are lit *in addition to* the mark's own colour, so the mark at rest has to
//!   keep headroom for the light to spend. A mark that rests at full strength
//!   has nowhere left to light up, and a band that mixes the mark toward the
//!   background — which is what this used to do — draws a shadow crossing the
//!   word rather than a light over it;
//! * **the light is the canvas's opposite, not a hue.** Silver-white on a dark
//!   page and ink on a light one, which is the same statement twice: the
//!   strongest thing that can be drawn where it falls;
//! * **the loop has no seam, and no wasted frame.** The light starts and ends
//!   two widths off the word, so the first and last frames of a pass are the
//!   resting mark; and [`moving`] names exactly the frames that draw something
//!   other than rest, which is what lets the client run the loop's clock
//!   between passes without repainting for it
//!   ([`crate::app::App::advance_transcript_animation`]).
//!
//! # The tear
//!
//! The glitch style moves in the characters rather than in the light, and the
//! three rules that make it read as a fault crossing the mark rather than as a
//! second, broken mark are:
//!
//! * **it is a burst, not a loop.** The tear runs for a few frames and then the
//!   mark is whole again for a long stretch, because a fault that never stopped
//!   would be the mark's own appearance rather than something happening to it —
//!   and the quiet stretch is most of the loop for the same reason the light
//!   waits between passes;
//! * **it dies away.** The first frames of a burst throw whole rows out of
//!   register and rot the fills; the last frames barely move. A burst that held
//!   its strength for its whole length reads as a cut rather than as a recovery;
//! * **the outline survives.** Only the filled cells of a letter are eaten. The
//!   frames eat the letter's body and leave its edges, which is what keeps the
//!   word legible while it falls apart — and what makes it a glitch of *this*
//!   mark rather than a rectangle of noise.
//!
//! Unlike the light, the tear needs no colour: its movement is carried by the
//! characters themselves, so a terminal that can draw no colour still sees the
//! mark come apart and settle. It is the one animation here that survives
//! `NO_COLOR`, and it survives it honestly.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::{ColorMode, GlyphMode, TuiTheme};

/// Which character art the landing mark is drawn with.
///
/// The reader's choice, remembered between runs like every other look. It is a
/// property of the drawing rather than of the theme: the same palette draws
/// both marks, and switching between them is not a rebuild of anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MarkStyle {
    /// The shipped mark: solid letters with a light crossing them.
    #[default]
    Classic,
    /// The same letters with a corroded fill, torn in bursts.
    Glitch,
}

impl MarkStyle {
    /// The styles a chooser offers, in display order.
    pub const ALL: [MarkStyle; 2] = [MarkStyle::Classic, MarkStyle::Glitch];

    /// The value the interface file stores.
    pub const fn id(self) -> &'static str {
        match self {
            MarkStyle::Classic => "classic",
            MarkStyle::Glitch => "glitch",
        }
    }

    /// The style an id names, when it is one this build knows.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|style| style.id() == id)
    }
}

/// The wordmark, one string per row.
///
/// Six rows is the height that fits a short terminal without pushing the prompt
/// off the screen.
const WORDMARK: [&str; 6] = [
    "██╗   ██╗ ██╗ ██████╗  ███████╗ ██╗  ██╗",
    "██║   ██║ ██║ ██╔══██╗ ██╔════╝ ╚██╗██╔╝",
    "██║   ██║ ██║ ██████╔╝ █████╗    ╚███╔╝ ",
    "╚██╗ ██╔╝ ██║ ██╔══██╗ ██╔══╝    ██╔██╗ ",
    " ╚████╔╝  ██║ ██████╔╝ ███████╗ ██╔╝ ██╗",
    "  ╚═══╝   ╚═╝ ╚═════╝  ╚══════╝ ╚═╝  ╚═╝",
];

/// The same word with a corroded fill.
///
/// The letters and their edges are [`WORDMARK`]'s exactly; what differs is the
/// body of each stroke, where one cell in nine is half-tone and one in
/// twenty-three is all but gone. The mark is therefore the same mark at rest —
/// a reader who switches style switches texture, not wordmark — and it is that
/// texture the tear below eats.
const WORDMARK_GLITCH: [&str; 6] = [
    "▒█╗   ██╗ ██╗ ████▓█╗  ▒███▓██╗ ██╗  ██╗",
    "██║   ▓█║ ██║ █▓╔══██╗ █▓╔════╝ ╚▓█╗██╔╝",
    "██║   ██║ ██║ ██████╔╝ █████╗    ╚██▒╔╝ ",
    "╚██╗ ██╔╝ ██║ ██╔══██╗ ██╔══╝    ██╔▓█╗ ",
    " ╚█▒██╔╝  ██║ █▓████╔╝ █▓█▒███╗ █▓╔╝ ██╗",
    "  ╚═══╝   ╚═╝ ╚═════╝  ╚══════╝ ╚═╝  ╚═╝",
];

/// The mark for a terminal that cannot draw the block glyphs.
///
/// Two rows, plain ASCII, in the spirit of the mark: a V and the word. It is
/// not a translation of the art above — that would be six rows of noise in a
/// legacy console font — it is the same statement made smaller. Both styles
/// share it: what a legacy font loses is the texture, and the tear is drawn in
/// characters it can already print.
const WORDMARK_ASCII: [&str; 2] = ["V  V  i  b  e  x", " VV   vibex.dev"];

// ---------------------------------------------------------------------------
// The light
// ---------------------------------------------------------------------------

/// Frames one light takes to cross the word.
pub const SWEEP_FRAMES: u32 = 20;

/// Frames one whole loop takes: the crossing, and the quiet stretch after it.
///
/// The quiet stretch is most of the loop on purpose. A light that came round
/// again the moment it left would stop being something that happens to the page
/// and start being a flicker on it. At the tick the client runs while a light
/// is moving, and the slower one it runs between lights, a loop is about ten
/// seconds with the light itself taking a quarter of that.
pub const LOOP_FRAMES: u32 = 60;

/// Columns either side of the light's centre that it reaches across.
const WASH_WIDTH: f32 = 5.0;

/// How far down the mark's own grey scale the resting bottom row sits.
///
/// The top row rests at `REST_HEAD` of the way there rather than at the
/// foreground, which is the headroom the light spends.
const REST_HEAD: f32 = 0.25;

/// Above this much light a cell is drawn bold in a palette that cannot blend.
const LIT: f32 = 0.45;

/// The colour a light crossing the mark takes.
///
/// The catalogue has no token named for a light, and the one it carries for the
/// job is `border`: authored as the canvas's opposite in every shipped theme —
/// `#ffffff` in all ten dark ones, `#000000` in all ten light ones — which is
/// what light is, and what makes a specular read as a specular rather than as a
/// slightly lighter grey. On a light page that inverts to ink, which is the
/// same statement: the strongest thing that can be drawn on the canvas it falls
/// on. The token is named for hairlines because that is what it was authored
/// for; its value is what a light needs, and minting a second white here would
/// put a colour outside the catalogue that every theme would then have to be
/// checked against.
fn silver(theme: &TuiTheme) -> Color {
    theme.roles.border
}

/// Whether this terminal can be shown a light at all.
///
/// With colour switched off there is nothing to spend: a light drawn in one
/// colour, with no weight to carry it, is a mark that would only flicker
/// between two identical frames. The wordmark is still the mark; only the
/// movement is dropped.
fn lights_up(theme: &TuiTheme) -> bool {
    !matches!(theme.capability.mode, ColorMode::None)
}

/// Whether the mark at `phase` draws anything other than its resting self.
///
/// Every frame outside a pass is the resting mark exactly, and every frame
/// inside one differs from it. That is the contract the client's clock between
/// passes relies on: it can keep the loop turning without repainting for it.
/// The frames at either end of a pass differ from rest by a level or two of one
/// channel, because the band starts and ends two widths off the word rather
/// than on its edge — the difference is what a seam would be made of, and it is
/// why there is none to see.
///
/// The glitch style keeps the same contract on its own clock: a burst's frames
/// are frames that tear, and every frame between bursts is the resting mark.
/// The two styles count frames differently and the client asks the question
/// through the style it is drawing, so a burst is a repaint and the still
/// stretch after it is not.
pub const fn moving(style: MarkStyle, phase: u32) -> bool {
    match style {
        MarkStyle::Classic => phase % LOOP_FRAMES < SWEEP_FRAMES,
        MarkStyle::Glitch => phase % GLITCH_LOOP_FRAMES < GLITCH_BURST_FRAMES,
    }
}

/// The art a terminal of `available` columns is drawn with, if it fits.
///
/// A mark that cannot be drawn whole is not drawn at all: half a wordmark reads
/// as a rendering fault, while the lines under it say the product's name
/// anyway. A console font has the one fallback for both styles: the letters it
/// cannot draw are the same letters either way, and the texture is what it was
/// never going to show.
fn art(style: MarkStyle, glyphs: GlyphMode, available: u16) -> Option<&'static [&'static str]> {
    let available = usize::from(available);
    let art: &'static [&'static str] = match (glyphs, style) {
        (GlyphMode::Ascii, _) => &WORDMARK_ASCII,
        (GlyphMode::Unicode, MarkStyle::Classic) => &WORDMARK,
        (GlyphMode::Unicode, MarkStyle::Glitch) => &WORDMARK_GLITCH,
    };
    (available >= width_of(art) + 2).then_some(art)
}

fn width_of(art: &[&str]) -> usize {
    art.iter().map(|row| row.chars().count()).max().unwrap_or(0)
}

/// How wide the mark drawn into `available` columns is, or zero when the
/// terminal is too narrow to hold it.
pub fn width(style: MarkStyle, glyphs: GlyphMode, available: u16) -> usize {
    art(style, glyphs, available).map(width_of).unwrap_or(0)
}

/// Rows of the mark, drawn in the reader's chosen style.
///
/// `phase` advances a frame at a time and is read through the style's own loop,
/// so the light comes round again — or the tear comes back — for as long as the
/// page waits. `bright` is whether the page the mark sits on is waiting for
/// input at all; a mark on a page that is not is drawn at rest and never
/// animated.
pub fn rows(
    theme: &TuiTheme,
    style: MarkStyle,
    phase: u32,
    available: u16,
    bright: bool,
) -> Vec<Line<'static>> {
    let Some(art) = art(style, theme.glyphs(), available) else {
        return Vec::new();
    };
    match style {
        MarkStyle::Classic => classic_rows(theme, phase, art, bright),
        MarkStyle::Glitch => glitch_rows(theme, phase, art, bright),
    }
}

/// The mark with a light crossing it.
fn classic_rows(
    theme: &TuiTheme,
    phase: u32,
    art: &'static [&'static str],
    bright: bool,
) -> Vec<Line<'static>> {
    let width = width_of(art);
    let height = art.len();
    let animating = bright && lights_up(theme);
    let wash = wash_position(phase, width);

    art.iter()
        .enumerate()
        .map(|(row, art)| {
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(width);
            let mut column = 0usize;
            for character in art.chars() {
                if character == ' ' {
                    // A cell that is not part of the mark is a gap, not a dim
                    // letter.
                    spans.push(Span::raw(" "));
                } else {
                    let light = if animating {
                        light_at(column, wash)
                    } else {
                        0.0
                    };
                    spans.push(Span::styled(
                        character.to_string(),
                        style_for(theme, row, height, light),
                    ));
                }
                column += 1;
            }
            while column < width {
                spans.push(Span::raw(" "));
                column += 1;
            }
            Line::from(spans)
        })
        .collect()
}

/// Where the light's centre is, in columns, or nowhere between passes.
fn wash_position(phase: u32, width: usize) -> f32 {
    let swept = phase % LOOP_FRAMES;
    if swept >= SWEEP_FRAMES {
        return f32::NEG_INFINITY;
    }
    let crossed = swept as f32 / SWEEP_FRAMES as f32;
    // Two widths off the left edge and two past the right one, so the band is
    // already dark where the word begins and dark again where it ends: the
    // first and last frames of a pass are the resting mark, and the loop has no
    // seam to see.
    crossed * (width as f32 + 4.0 * WASH_WIDTH) - 2.0 * WASH_WIDTH
}

/// How much light a cell carries: 0.0 at rest, 1.0 under the centre of a band.
fn light_at(column: usize, wash: f32) -> f32 {
    (-((column as f32 - wash) / WASH_WIDTH).powi(2)).exp()
}

/// The style one cell of the mark is drawn in.
///
/// A cell's colour is its resting weight walked toward the light.
/// [`ColorMode::blends`] decides whether that ramp can be drawn at all; where
/// it cannot, the resting weight becomes a step between two theme greys and the
/// light becomes weight, which is the only brightness a shallow palette has.
fn style_for(theme: &TuiTheme, row: usize, height: usize, light: f32) -> Style {
    if !lights_up(theme) {
        return Style::default().fg(theme.roles.foreground);
    }
    // The lower half rests a step down its own greys; a palette that cannot
    // blend carries that step as `DIM` instead of as a colour.
    let low = height > 1 && row * 2 >= height;
    if !theme.capability.mode.blends() {
        let colour = if low {
            theme.roles.gray_bright
        } else {
            theme.roles.foreground
        };
        return if light > LIT {
            Style::default().fg(colour).add_modifier(Modifier::BOLD)
        } else if low {
            Style::default().fg(colour).add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(colour)
        };
    }
    let rest = resting_colour(theme, row, height);
    Style::default().fg(theme.blend(rest, silver(theme), light))
}

/// The same cell with nothing falling on it.
///
/// Shared by both styles: a tear is a thing that happens to the mark, so the
/// mark it happens to has to be the same one — and the reader who switches
/// style switches the drawing, not the palette.
fn rest_style(theme: &TuiTheme, row: usize, height: usize) -> Style {
    if !lights_up(theme) {
        return Style::default().fg(theme.roles.foreground);
    }
    let low = height > 1 && row * 2 >= height;
    if !theme.capability.mode.blends() {
        let colour = if low {
            theme.roles.gray_bright
        } else {
            theme.roles.foreground
        };
        return if low {
            Style::default().fg(colour).add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(colour)
        };
    }
    Style::default().fg(resting_colour(theme, row, height))
}

/// The colour one row of the mark rests at.
///
/// A gradient rather than one ink, which is the headroom the light is spent
/// from: a mark that rested flat has nowhere left to brighten.
fn resting_colour(theme: &TuiTheme, row: usize, height: usize) -> Color {
    let depth = if height <= 1 {
        REST_HEAD
    } else {
        REST_HEAD + (1.0 - REST_HEAD) * (row as f32 / (height - 1) as f32)
    };
    theme.blend(theme.roles.foreground, theme.roles.gray_dim, depth)
}

// ---------------------------------------------------------------------------
// The tear
// ---------------------------------------------------------------------------

/// Frames one burst takes.
pub const GLITCH_BURST_FRAMES: u32 = 10;

/// Frames one whole glitch loop takes: the tear, and the still stretch after it.
///
/// The same shape as the light's loop, and for the same reason: a fault that
/// was always happening would be the mark's own appearance. A loop is about
/// five seconds at the tick the client runs while one is moving, a quarter of
/// which is the burst.
pub const GLITCH_LOOP_FRAMES: u32 = 40;

/// How many steps down the burst dies over.
const TEAR_STEPS: u32 = 3;

/// How hard the tear is `swept` frames into a loop, or zero between bursts.
///
/// The strength steps down over the burst rather than holding: a mark thrown
/// just as far on its last frame as on its first has been cut, not disturbed.
fn burst(swept: u32) -> u32 {
    if swept >= GLITCH_BURST_FRAMES {
        return 0;
    }
    let left = GLITCH_BURST_FRAMES - swept;
    left.saturating_mul(TEAR_STEPS)
        .div_ceil(GLITCH_BURST_FRAMES)
        .clamp(1, TEAR_STEPS)
}

/// The row a burst always throws out of register.
///
/// One row of every moving frame is displaced without exception. A frame that
/// happened to displace nothing and eat nothing would be a repaint the clock
/// asked for and the reader could not see, and [`moving`] promises the frames
/// it names are exactly the frames that differ from rest.
fn torn_row(phase: u32, height: usize) -> usize {
    (roll(u64::from(phase) ^ 0x5DEE_CE66_D0F1_1234) % height.max(1) as u64) as usize
}

/// How far one row is displaced this frame, in columns.
///
/// Most rows of most frames hold still: a whole mark thrown at once reads as a
/// different mark, and the fault is legible only while the rest of the word
/// says what it is happening to.
fn tear(row: usize, height: usize, phase: u32, intensity: u32) -> i32 {
    if intensity == 0 {
        return 0;
    }
    let roll = roll((row as u64) << 32 | u64::from(phase));
    let magnitude = 1 + (roll % 3) as i32;
    let direction = if roll & 0x100 == 0 { 1 } else { -1 };
    if row == torn_row(phase, height) {
        return magnitude * direction;
    }
    if roll % 5 >= u64::from(intensity) {
        return 0;
    }
    magnitude * direction
}

/// Whether this cell is the edge of a stroke rather than its body.
///
/// The edges are the box-drawing characters the letters are outlined with;
/// everything else drawn is body. Naming the edge rather than the body is what
/// keeps the rule honest on the console fallback, whose letters are made of no
/// block glyphs at all.
fn is_edge(character: char) -> bool {
    matches!(character, '╗' | '╔' | '╚' | '╝' | '║' | '═')
}

/// The glyph a torn cell is eaten into, if this cell is eaten this frame.
///
/// Only the body of a stroke is eaten. The edge stays, which is what keeps the
/// word legible while it comes apart — and it is the body that carries the
/// mark's texture, so the cells that change are the cells the reader was
/// already reading as "this is the mark".
fn eaten(row: usize, column: usize, phase: u32, intensity: u32, glyphs: GlyphMode) -> Option<char> {
    let roll = roll((row as u64) << 40 | (column as u64) << 8 | u64::from(phase));
    if roll % 30 >= u64::from(intensity) * 2 {
        return None;
    }
    let glyphs = glitch_glyphs(glyphs);
    Some(glyphs[((roll >> 32) as usize) % glyphs.len()])
}

/// The characters a torn cell is replaced with.
///
/// Block shades where the terminal has them, because the mark is already drawn
/// in them and the tear should read as the same material; punctuation where it
/// does not, because a console font draws the blocks as substitution boxes.
fn glitch_glyphs(glyphs: GlyphMode) -> &'static [char] {
    const UNICODE: [char; 12] = ['░', '▒', '▓', '▚', '▞', '▖', '▗', '▘', '▝', '╳', '#', '%'];
    const ASCII: [char; 12] = ['#', '%', '*', '=', '+', '-', '.', '/', '<', '>', '|', '~'];
    match glyphs {
        GlyphMode::Unicode => &UNICODE,
        GlyphMode::Ascii => &ASCII,
    }
}

/// The style a torn cell is drawn in.
///
/// Two accents rather than one, because a tear is a signal arriving out of
/// register and the eye reads two hues in the same word as exactly that. Not
/// `danger`: the mark is not reporting anything, and a page that greets the
/// reader in red has told them something untrue about their session.
fn tear_style(theme: &TuiTheme, roll: u64) -> Style {
    let colour = if roll & 0x2 == 0 {
        theme.roles.accent_attention
    } else {
        theme.roles.accent_user
    };
    Style::default().fg(colour).add_modifier(Modifier::BOLD)
}

/// The mark torn and eaten for one frame.
///
/// A frame between bursts is the resting mark exactly — the same rows
/// [`classic_rows`] draws with no light on them — so the client can hold the
/// clock still between bursts without the page ever showing a seam.
fn glitch_rows(
    theme: &TuiTheme,
    phase: u32,
    art: &'static [&'static str],
    bright: bool,
) -> Vec<Line<'static>> {
    let width = width_of(art);
    let height = art.len();
    // Everything below is a function of how far into the loop the clock is
    // rather than of the clock itself, so a burst is the same burst every time
    // it comes round and the loop repeats exactly.
    let swept = phase % GLITCH_LOOP_FRAMES;
    let intensity = if bright { burst(swept) } else { 0 };

    art.iter()
        .enumerate()
        .map(|(row, art)| {
            let cells = displaced(art, width, tear(row, height, swept, intensity));
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(width);
            for (column, character) in cells.into_iter().enumerate() {
                if character == ' ' {
                    spans.push(Span::raw(" "));
                    continue;
                }
                let eaten = if intensity > 0 && !is_edge(character) {
                    eaten(row, column, swept, intensity, theme.glyphs())
                } else {
                    None
                };
                match eaten {
                    Some(glyph) => {
                        let roll = roll((row as u64) << 40 | (column as u64) << 8);
                        spans.push(Span::styled(glyph.to_string(), tear_style(theme, roll)));
                    }
                    None => spans.push(Span::styled(
                        character.to_string(),
                        rest_style(theme, row, height),
                    )),
                }
            }
            Line::from(spans)
        })
        .collect()
}

/// One row of the mark moved `offset` columns, still exactly `width` wide.
///
/// The row is a fixed-width line of cells rather than a string, because a
/// displacement is a property of the grid: what leaves one edge has to leave
/// the line, or the mark would grow a column every time a row moved and the
/// block it is centred in would drift.
fn displaced(art: &str, width: usize, offset: i32) -> Vec<char> {
    let mut cells = vec![' '; width];
    for (column, character) in art.chars().enumerate() {
        let target = column as i64 + i64::from(offset);
        if target >= 0 && (target as usize) < width {
            cells[target as usize] = character;
        }
    }
    cells
}

/// A small, reproducible scatter for one cell of one frame.
///
/// Reproducible because the frame is a function of the phase and nothing else:
/// the client draws the same frame twice for the same clock, which is what lets
/// a test say what a burst looks like — and what keeps a resize, a re-render or
/// a scroll from reshuffling a tear that is already on screen.
fn roll(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorCapability;
    use vibex_ui::GpuiThemeMode;

    fn theme(mode: ColorMode, glyphs: GlyphMode) -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-dark"),
            GpuiThemeMode::Dark,
            ColorCapability { mode, glyphs },
        )
    }

    fn light_theme() -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-light"),
            GpuiThemeMode::Light,
            ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Unicode,
            },
        )
    }

    fn text_of(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    fn painted(lines: &[Line<'static>]) -> String {
        format!("{lines:?}")
    }

    fn first_colour(line: &Line<'static>) -> Color {
        line.spans
            .iter()
            .find_map(|span| span.style.fg)
            .expect("no drawn cell on this row")
    }

    fn brightness(colour: Color) -> i32 {
        match colour {
            Color::Rgb(r, g, b) => i32::from(r) + i32::from(g) + i32::from(b),
            other => panic!("true colour produced {other:?}"),
        }
    }

    #[test]
    fn the_mark_fits_the_terminal_it_is_drawn_in() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let wide = rows(&unicode, MarkStyle::Classic, 0, 80, true);
        assert_eq!(wide.len(), WORDMARK.len());
        assert_eq!(
            width(MarkStyle::Classic, GlyphMode::Unicode, 80),
            WORDMARK[0].chars().count()
        );
        assert!(
            wide.iter()
                .all(|line| line.width() == width(MarkStyle::Classic, GlyphMode::Unicode, 80)),
            "a row of the mark is not the width the mark is centred on"
        );

        // A pane too narrow for the word gets the lines under it and nothing
        // else, rather than half a wordmark.
        assert!(rows(&unicode, MarkStyle::Classic, 0, 30, true).is_empty());
        assert_eq!(width(MarkStyle::Classic, GlyphMode::Unicode, 30), 0);

        // A console font has no block glyphs: the mark falls back to words
        // rather than to six rows of substitution characters.
        let ascii = theme(ColorMode::TrueColor, GlyphMode::Ascii);
        let art = rows(&ascii, MarkStyle::Classic, 0, 80, true);
        assert_eq!(art.len(), WORDMARK_ASCII.len());
        assert!(
            art.iter().all(|line| line.to_string().is_ascii()),
            "{art:?}"
        );
    }

    #[test]
    fn the_light_crosses_the_letters_and_never_redraws_them() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let letters = |phase: u32| text_of(&rows(&unicode, MarkStyle::Classic, phase, 80, true));
        let resting = letters(LOOP_FRAMES);
        // Every frame of two whole loops spells the same word.
        for phase in 0..(2 * LOOP_FRAMES) {
            assert_eq!(letters(phase), resting, "phase {phase} redrew the mark");
        }
        // The light moves, whatever else is true.
        let lit = |phase: u32| painted(&rows(&unicode, MarkStyle::Classic, phase, 80, true));
        assert_ne!(lit(0), lit(SWEEP_FRAMES / 2), "the light did not move");
        assert_ne!(
            lit(SWEEP_FRAMES / 2),
            lit(SWEEP_FRAMES - 1),
            "the light stopped halfway"
        );
    }

    #[test]
    fn the_loop_has_no_seam_and_no_wasted_frame() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        // The resting mark is read from the frame the light has just left on,
        // which is the first frame of the quiet stretch. A whole loop on from
        // the start of a pass is the start of the *next* pass, not rest.
        let resting = painted(&rows(&unicode, MarkStyle::Classic, SWEEP_FRAMES, 80, true));
        for phase in SWEEP_FRAMES..LOOP_FRAMES {
            assert_eq!(
                painted(&rows(&unicode, MarkStyle::Classic, phase, 80, true)),
                resting,
                "phase {phase} of the quiet stretch does not rest"
            );
        }
        // The loop comes round exactly: what the client draws at a phase and at
        // that phase plus a loop is the same picture.
        for phase in 0..LOOP_FRAMES {
            assert_eq!(
                painted(&rows(&unicode, MarkStyle::Classic, phase, 80, true)),
                painted(&rows(
                    &unicode,
                    MarkStyle::Classic,
                    phase + LOOP_FRAMES,
                    80,
                    true
                )),
                "phase {phase} does not repeat"
            );
        }
        // `moving` names the frames that differ from rest, in both directions:
        // a quiet frame that drew something would be a repaint the client asked
        // for and did not get, and a moving frame that drew the resting mark
        // would be a repaint it paid for and did not need.
        for phase in 0..(2 * LOOP_FRAMES) {
            let frame = painted(&rows(&unicode, MarkStyle::Classic, phase, 80, true));
            if moving(MarkStyle::Classic, phase) {
                assert_ne!(frame, resting, "phase {phase} moves but draws rest");
            } else {
                assert_eq!(frame, resting, "phase {phase} is quiet but draws");
            }
        }
    }

    #[test]
    fn the_light_is_the_canvas_s_opposite() {
        // Halfway through a pass the band's centre is on column twenty, so that
        // cell carries the light at full strength: the colour to read the light
        // off.
        let lit_cell = |theme: &TuiTheme| {
            rows(theme, MarkStyle::Classic, SWEEP_FRAMES / 2, 80, true)[0].spans[20]
                .style
                .fg
                .expect("the cell is drawn")
        };
        let rest_top = |theme: &TuiTheme| {
            first_colour(&rows(theme, MarkStyle::Classic, SWEEP_FRAMES, 80, true)[0])
        };

        let dark = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        assert_eq!(
            lit_cell(&dark),
            dark.roles.border,
            "the light is not the colour the catalogue keeps for it"
        );
        assert!(
            brightness(lit_cell(&dark)) > brightness(rest_top(&dark)),
            "the light does not rise above the resting mark"
        );
        // The mark rests as a gradient rather than a flat block, which is the
        // headroom that light is spending.
        let resting = rows(&dark, MarkStyle::Classic, SWEEP_FRAMES, 80, true);
        assert_ne!(
            first_colour(&resting[0]),
            first_colour(&resting[WORDMARK.len() - 1]),
            "the mark rests flat"
        );

        // On a light page the same ramp is ink: a light cannot be brighter than
        // the paper it falls on, so the strongest thing that can be drawn there
        // is the darkest.
        let light = light_theme();
        assert_eq!(lit_cell(&light), light.roles.border);
        assert!(
            brightness(lit_cell(&light)) < brightness(rest_top(&light)),
            "the light on a light page does not darken the mark"
        );
    }

    #[test]
    fn a_terminal_that_cannot_blend_gets_weight_instead() {
        // A shallow palette has no room for the ramp, so the light is carried
        // by modifiers, which is the only brightness it has. What it must not
        // do is draw the fade anyway and let the terminal quantise it into a
        // flicker.
        for mode in [ColorMode::Ansi16, ColorMode::Ansi256] {
            let theme = theme(mode, GlyphMode::Unicode);
            let lit = rows(&theme, MarkStyle::Classic, SWEEP_FRAMES / 2, 80, true);
            assert!(
                lit.iter().any(|line| line
                    .spans
                    .iter()
                    .any(|span| span.style.add_modifier.contains(Modifier::BOLD))),
                "{mode:?} did not mark the light"
            );
            // The resting mark still steps down its own greys, so the page is
            // not left with the flat block a gradient-less palette would give.
            let resting = rows(&theme, MarkStyle::Classic, LOOP_FRAMES, 80, true);
            assert_ne!(
                first_colour(&resting[0]),
                first_colour(&resting[WORDMARK.len() - 1]),
                "{mode:?} left the mark flat"
            );
        }

        // Colour switched off entirely is the floor: the mark is drawn whole
        // and still, because a light with neither colour nor weight to spend
        // would only flicker between two identical frames.
        let plain = theme(ColorMode::None, GlyphMode::Unicode);
        let resting = painted(&rows(&plain, MarkStyle::Classic, LOOP_FRAMES, 80, true));
        for phase in [0, SWEEP_FRAMES / 2, SWEEP_FRAMES + 3] {
            assert_eq!(
                painted(&rows(&plain, MarkStyle::Classic, phase, 80, true)),
                resting,
                "a colourless terminal animated the mark at phase {phase}"
            );
        }
    }

    #[test]
    fn the_tear_draws_the_same_word_in_the_same_place() {
        // The two styles are two readings of one mark, so they have to occupy
        // the same block: a reader who switches style has switched the drawing,
        // not the page's layout.
        let dark = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let torn = rows(&dark, MarkStyle::Glitch, 0, 80, true);
        assert_eq!(torn.len(), WORDMARK_GLITCH.len());
        assert_eq!(
            width(MarkStyle::Glitch, GlyphMode::Unicode, 80),
            width(MarkStyle::Classic, GlyphMode::Unicode, 80)
        );
        assert!(
            torn.iter()
                .all(|line| line.width() == width(MarkStyle::Glitch, GlyphMode::Unicode, 80)),
            "a row of the torn mark is not the width the mark is centred on"
        );
        // The same narrow-terminal and console-font behaviour as the light.
        assert!(rows(&dark, MarkStyle::Glitch, 0, 30, true).is_empty());
        let ascii = theme(ColorMode::TrueColor, GlyphMode::Ascii);
        assert!(
            rows(&ascii, MarkStyle::Glitch, 0, 80, true)
                .iter()
                .all(|line| line.to_string().is_ascii())
        );

        // The letters are the shipped ones: only the body of each stroke is
        // drawn as a half-tone, so the word still reads as the word.
        for (torn_row, plain_row) in WORDMARK_GLITCH.iter().zip(WORDMARK.iter()) {
            assert_eq!(torn_row.chars().count(), plain_row.chars().count());
            for (torn_cell, plain_cell) in torn_row.chars().zip(plain_row.chars()) {
                assert!(
                    torn_cell == plain_cell
                        || (plain_cell == '█' && matches!(torn_cell, '▓' | '▒')),
                    "{torn_row} is not {plain_row} with a corroded fill"
                );
            }
        }
    }

    #[test]
    fn the_tear_comes_in_bursts_and_leaves_the_mark_whole() {
        let dark = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let frame = |phase: u32| painted(&rows(&dark, MarkStyle::Glitch, phase, 80, true));
        // The mark is whole again the frame the burst ends, and stays whole for
        // the rest of the loop.
        let resting = frame(GLITCH_BURST_FRAMES);
        for phase in GLITCH_BURST_FRAMES..GLITCH_LOOP_FRAMES {
            assert_eq!(
                frame(phase),
                resting,
                "phase {phase} of the still stretch is not the whole mark"
            );
        }
        // Every frame of a burst is a frame the reader can see: a burst that
        // happened to move nothing would be a repaint bought for nothing.
        for phase in 0..GLITCH_BURST_FRAMES {
            assert_ne!(frame(phase), resting, "phase {phase} of the burst is rest");
        }
        // The loop comes round exactly, so the burst is the same burst every
        // time — and `moving` names exactly the frames that differ from rest.
        for phase in 0..GLITCH_LOOP_FRAMES {
            assert_eq!(
                frame(phase),
                frame(phase + GLITCH_LOOP_FRAMES),
                "phase {phase} does not repeat"
            );
            assert_eq!(
                moving(MarkStyle::Glitch, phase),
                phase < GLITCH_BURST_FRAMES,
                "`moving` disagrees with the burst at phase {phase}"
            );
        }

        // A page that is not waiting is drawn at rest and never animated.
        assert_eq!(
            painted(&rows(&dark, MarkStyle::Glitch, 0, 80, false)),
            resting,
            "a page that is not waiting tore its mark"
        );

        // The burst dies away rather than holding: the first frame of a burst
        // is not its last.
        let torn_cells = |phase: u32| {
            rows(&dark, MarkStyle::Glitch, phase, 80, true)
                .iter()
                .flat_map(|line| line.spans.iter())
                .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
                .count()
        };
        assert!(
            torn_cells(0) > torn_cells(GLITCH_BURST_FRAMES - 1),
            "the burst did not die away"
        );
    }

    #[test]
    fn the_tear_eats_the_body_and_leaves_the_edges() {
        // The word has to stay legible while it falls apart, so the eaten cells
        // are the strokes' bodies: every edge the mark was drawn with is still
        // there, displaced with its row but never replaced by noise.
        let dark = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let width = width(MarkStyle::Glitch, GlyphMode::Unicode, 80);
        let height = WORDMARK_GLITCH.len();
        for phase in [0, 1, 4, GLITCH_BURST_FRAMES - 1] {
            let swept = phase % GLITCH_LOOP_FRAMES;
            let intensity = burst(swept);
            let drawn = rows(&dark, MarkStyle::Glitch, phase, 80, true);
            for (row, line) in drawn.iter().enumerate() {
                let expected = displaced(
                    WORDMARK_GLITCH[row],
                    width,
                    tear(row, height, swept, intensity),
                );
                let actual = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                for (column, character) in expected.into_iter().enumerate() {
                    if is_edge(character) {
                        assert_eq!(
                            actual.chars().nth(column),
                            Some(character),
                            "phase {phase} ate the edge at row {row}, column {column}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_colourless_terminal_still_sees_the_tear() {
        // The light is colour, so a terminal with none has nothing to spend and
        // the classic mark holds still. The tear is carried by the characters
        // themselves, so it moves where the light cannot.
        let plain = theme(ColorMode::None, GlyphMode::Unicode);
        let torn = painted(&rows(&plain, MarkStyle::Glitch, 0, 80, true));
        assert_ne!(
            torn,
            painted(&rows(
                &plain,
                MarkStyle::Glitch,
                GLITCH_BURST_FRAMES,
                80,
                true
            )),
            "a colourless terminal did not see the tear"
        );
        // And the mark it tears up is still the whole word: the edges survive
        // here too, which is all a monochrome terminal has to read a letter by.
        assert!(
            torn.contains('╗') && torn.contains('╚'),
            "the tear left no letter to read"
        );
    }
}
