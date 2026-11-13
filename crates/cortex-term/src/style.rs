//! Colours and text attributes, and how a colour is brought down to what a
//! terminal can show.
//!
//! This module names no colour of its own: which colours the interface uses
//! is the theme's business (`docs/architecture/09-tui.md`, "Colour and
//! glyphs"), and the theme lives in `cortex-tui`.

/// A colour as a component asks for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    /// The terminal's own foreground or background.
    #[default]
    Default,
    /// One of the sixteen colours of the terminal's palette: 0 to 7 normal,
    /// 8 to 15 bright. A number above 15 is taken modulo 16.
    Ansi(u8),
    /// One of the 256 colours of the extended palette.
    Indexed(u8),
    /// A colour given exactly.
    Rgb(u8, u8, u8),
}

/// How many colours a terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ColorDepth {
    /// No colour at all: attributes only. What `NO_COLOR` asks for.
    Mono,
    /// The sixteen palette colours.
    Ansi16,
    /// The 256-colour palette.
    Indexed256,
    /// Any colour, given as red, green and blue.
    TrueColor,
}

/// How a piece of text looks: two colours and five attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "five independent attributes of a terminal cell, each on or off"
)]
pub struct Style {
    /// The colour of the text.
    pub fg: Color,
    /// The colour behind it.
    pub bg: Color,
    /// Heavier text.
    pub bold: bool,
    /// Fainter text.
    pub dim: bool,
    /// Slanted text.
    pub italic: bool,
    /// A line under the text.
    pub underline: bool,
    /// Foreground and background swapped.
    pub reverse: bool,
}

impl Style {
    /// The terminal's own look: no colour, no attribute.
    pub const PLAIN: Self = Self {
        fg: Color::Default,
        bg: Color::Default,
        bold: false,
        dim: false,
        italic: false,
        underline: false,
        reverse: false,
    };

    /// This style with the text in `color`.
    #[must_use]
    pub const fn fg(mut self, color: Color) -> Self {
        self.fg = color;
        self
    }

    /// This style with `color` behind the text.
    #[must_use]
    pub const fn bg(mut self, color: Color) -> Self {
        self.bg = color;
        self
    }

    /// This style, bold.
    #[must_use]
    pub const fn bold(mut self) -> Self {
        self.bold = true;
        self
    }

    /// This style, dim.
    #[must_use]
    pub const fn dim(mut self) -> Self {
        self.dim = true;
        self
    }

    /// This style, italic.
    #[must_use]
    pub const fn italic(mut self) -> Self {
        self.italic = true;
        self
    }

    /// This style, underlined.
    #[must_use]
    pub const fn underline(mut self) -> Self {
        self.underline = true;
        self
    }

    /// This style, reversed.
    #[must_use]
    pub const fn reverse(mut self) -> Self {
        self.reverse = true;
        self
    }

    /// This style as a terminal of `depth` can show it: colours brought
    /// down to the nearest it has, or dropped for [`ColorDepth::Mono`].
    #[must_use]
    pub fn for_depth(self, depth: ColorDepth) -> Self {
        Self {
            fg: self.fg.for_depth(depth),
            bg: self.bg.for_depth(depth),
            ..self
        }
    }
}

/// The sixteen palette colours as xterm draws them by default, which is
/// what a colour is brought down to when the terminal has only those.
const ANSI_RGB: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

/// The six levels of each channel in the 256-colour cube.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

impl Color {
    /// This colour as a terminal of `depth` can show it.
    #[must_use]
    pub fn for_depth(self, depth: ColorDepth) -> Self {
        match (self, depth) {
            (Self::Default, _) => Self::Default,
            (_, ColorDepth::Mono) => Self::Default,
            (Self::Ansi(index), _) => Self::Ansi(index % 16),
            (color, ColorDepth::TrueColor) => color,
            (Self::Indexed(index), ColorDepth::Indexed256) => Self::Indexed(index),
            (Self::Rgb(r, g, b), ColorDepth::Indexed256) => Self::Indexed(nearest_indexed(r, g, b)),
            (Self::Indexed(index), ColorDepth::Ansi16) if index < 16 => Self::Ansi(index),
            (Self::Indexed(index), ColorDepth::Ansi16) => {
                let (r, g, b) = indexed_rgb(index);
                Self::Ansi(nearest_ansi(r, g, b))
            }
            (Self::Rgb(r, g, b), ColorDepth::Ansi16) => Self::Ansi(nearest_ansi(r, g, b)),
        }
    }
}

/// The red, green and blue of a colour of the 256-colour palette.
fn indexed_rgb(index: u8) -> (u8, u8, u8) {
    match index {
        0..=15 => ANSI_RGB[usize::from(index)],
        16..=231 => {
            let cube = index - 16;
            (
                CUBE[usize::from(cube / 36)],
                CUBE[usize::from(cube / 6 % 6)],
                CUBE[usize::from(cube % 6)],
            )
        }
        _ => {
            let level = 8 + (index - 232) * 10;
            (level, level, level)
        }
    }
}

fn distance((r1, g1, b1): (u8, u8, u8), (r2, g2, b2): (u8, u8, u8)) -> u32 {
    let square = |a: u8, b: u8| {
        let d = i32::from(a) - i32::from(b);
        d.unsigned_abs() * d.unsigned_abs()
    };
    square(r1, r2) + square(g1, g2) + square(b1, b2)
}

/// The palette colour nearest to a colour.
fn nearest_ansi(r: u8, g: u8, b: u8) -> u8 {
    let mut best = 0;
    for (index, rgb) in (0u8..).zip(ANSI_RGB) {
        if distance(rgb, (r, g, b)) < distance(ANSI_RGB[usize::from(best)], (r, g, b)) {
            best = index;
        }
    }
    best
}

/// The colour of the cube or the grey ramp nearest to a colour. The first
/// sixteen are left out: their look is the terminal's to choose.
fn nearest_indexed(r: u8, g: u8, b: u8) -> u8 {
    let level = |channel: u8| {
        let mut best = 0;
        for (index, value) in CUBE.iter().enumerate() {
            if channel.abs_diff(*value) < channel.abs_diff(CUBE[best]) {
                best = index;
            }
        }
        best
    };
    let (lr, lg, lb) = (level(r), level(g), level(b));
    // Each level is below 6, so the index is at most 231.
    let cube = u8::try_from(16 + 36 * lr + 6 * lg + lb).unwrap_or(231);
    let grey_level = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let grey_step = grey_level.saturating_sub(3) / 10;
    let grey = u8::try_from(232 + grey_step.min(23)).unwrap_or(255);
    if distance(indexed_rgb(grey), (r, g, b)) < distance(indexed_rgb(cube), (r, g, b)) {
        grey
    } else {
        cube
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_colour_comes_down_to_the_nearest_the_terminal_has() {
        let purple = Color::Rgb(0x7a, 0x3c, 0xfb);
        assert_eq!(purple.for_depth(ColorDepth::TrueColor), purple);
        assert_eq!(purple.for_depth(ColorDepth::Indexed256), Color::Indexed(99));
        assert_eq!(purple.for_depth(ColorDepth::Ansi16), Color::Ansi(12));
        assert_eq!(purple.for_depth(ColorDepth::Mono), Color::Default);
        assert_eq!(
            Color::Rgb(128, 128, 128).for_depth(ColorDepth::Indexed256),
            Color::Indexed(244)
        );
        assert_eq!(
            Color::Indexed(9).for_depth(ColorDepth::Ansi16),
            Color::Ansi(9)
        );
        assert_eq!(
            Color::Indexed(196).for_depth(ColorDepth::Ansi16),
            Color::Ansi(9)
        );
        assert_eq!(
            Color::Ansi(17).for_depth(ColorDepth::TrueColor),
            Color::Ansi(1)
        );
        // Every colour of the cube and the ramp finds itself.
        for index in 16..=255u8 {
            let (r, g, b) = indexed_rgb(index);
            assert_eq!(indexed_rgb(nearest_indexed(r, g, b)), (r, g, b), "{index}");
        }
    }

    #[test]
    fn mono_keeps_the_attributes() {
        let style = Style::PLAIN
            .fg(Color::Ansi(1))
            .bg(Color::Rgb(1, 2, 3))
            .bold()
            .underline();
        let mono = style.for_depth(ColorDepth::Mono);
        assert_eq!(mono, Style::PLAIN.bold().underline());
    }
}
