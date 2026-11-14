//! Themes: colour as semantic tokens (`docs/architecture/09-tui.md`, "Colour
//! and glyphs").
//!
//! A view never names a colour. It asks the [`Theme`] for the [`Style`] of a
//! [`Token`]: what the thing it draws *is* (the accent, muted text, an added
//! line's sign), and the theme answers for the terminal it was made for. So
//! one theme value carries every rule that depends on the terminal:
//!
//! - **The depth.** Colours are brought down to what the terminal shows. The
//!   accent at 16 colours is the one the [`Palette`] names, never the
//!   automatic reduction, which turns the logo's purple into bright blue.
//!   Without colour (`NO_COLOR`), tokens that were only a colour get an
//!   attribute instead (muted text is dim, an error bold), so that meaning is
//!   never carried by colour alone.
//! - **The background,** dark or light, from the colour the terminal reported
//!   ([`Background::of`]). It picks the palette, sets the accent's text tint
//!   ([`Token::AccentText`]) to reach [`TEXT_CONTRAST`] against that very
//!   colour, and mixes the diff tints into it. When the terminal does not say,
//!   the theme is the dark one, made for Windows Terminal's default scheme.
//!
//! A theme must be made for the depth the presenter draws at
//! (`Capabilities::color`): its colours are already brought down, and the
//! presenter's own reduction leaves them as they are.

use cortex_term::{Capabilities, Color, ColorDepth, Style};

/// A colour as red, green and blue.
pub type Rgb = (u8, u8, u8);

/// The accent: the logo's purple, `#7A3CFB`.
pub const ACCENT: Rgb = (0x7a, 0x3c, 0xfb);

/// The contrast that text must reach against the background: 4.5 to 1.
pub const TEXT_CONTRAST: f64 = 4.5;

/// The background assumed when the terminal does not report one: Windows
/// Terminal's default dark scheme, Campbell (`#0C0C0C`). The theme is then the
/// dark one (`09-tui.md`: "When the background cannot be determined, the
/// dark-background tint is used").
pub const ASSUMED_BACKGROUND: Rgb = (0x0c, 0x0c, 0x0c);

/// Whether the terminal's background is dark or light.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Background {
    /// Light text on a dark background.
    Dark,
    /// Dark text on a light background.
    Light,
}

impl Background {
    /// Dark when white text reads better on `rgb` than black text does.
    #[must_use]
    pub fn of(rgb: Rgb) -> Self {
        if contrast(rgb, (255, 255, 255)) >= contrast(rgb, (0, 0, 0)) {
            Self::Dark
        } else {
            Self::Light
        }
    }
}

/// What a view draws, by what it is. [`Theme::style`] gives each its style.
///
/// The first group is 09-tui's table of tokens: the colours a theme
/// assigns. The rest are the look's elements, each drawn in one or two of
/// those colours with an attribute, so that the rules for 16 colours and for
/// none are kept here and not in every view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Token {
    /// Answers: the terminal's own foreground.
    Text,
    /// Reasoning, tool detail, notices, hints, line numbers, box borders and
    /// rules: bright black, or dim without colour.
    Muted,
    /// The accent, for marks: the spinner, the active plan item's glyph, the
    /// selected menu row's `◆`.
    Accent,
    /// Text in the accent colour, such as `fast` in the hint row: the
    /// accent's tint that reaches [`TEXT_CONTRAST`] against the background.
    /// At 16 colours on a dark background no magenta does, so it is the
    /// default colour, bold; and bold without colour.
    AccentText,
    /// Your `◆`: the accent, bold.
    User,
    /// A tool call's imperative, a dialog's question, and your message: bold.
    Title,
    /// Inline code: bright cyan on a dark background, cyan and bold on a light
    /// one; bold without colour.
    Code,
    /// Headings: bold.
    Heading,
    /// `✓` and an edit's count of added lines (`+5`): green.
    Success,
    /// `?` and warnings: yellow.
    Warning,
    /// `✗`, errors, a failure's measure and an edit's count of removed
    /// lines (`−1`): red; bold without colour.
    Error,
    /// The colour of an added line's sign, and of the line where there is no
    /// tint: green.
    DiffAdd,
    /// The colour of a removed line's sign, and of the line where there is no
    /// tint: red.
    DiffDel,
    /// The tint behind an added line, in true colour against a background
    /// the terminal reported: green mixed into it. No style otherwise.
    DiffAddBg,
    /// The tint behind a removed line, in true colour against a background
    /// the terminal reported: red mixed into it. No style otherwise.
    DiffDelBg,
    /// Changed words inside a changed line: bold.
    DiffEmph,

    /// The answer's `◇`: the terminal's own foreground, neither dim nor the
    /// accent; bold without colour.
    Answer,
    /// Reasoning's words: muted and italic.
    Reasoning,
    /// A link's text: underlined.
    Link,
    /// A box's border and a rule: muted.
    Border,
    /// The border of the selected box in the viewer: the accent.
    BorderSelected,
    /// The hint row, a box's label and a placeholder: muted.
    Hint,
    /// A key to press, as in a dialog's answers: accent text, bold.
    Key,
    /// The selected row of a menu, and the focused item of a panel: accent
    /// text, bold.
    Selected,
    /// An added line, the whole row: the tint, or green where there is none.
    DiffAddLine,
    /// A removed line, the whole row: the tint, or red where there is none;
    /// dim without colour.
    DiffDelLine,
    /// An added line's `+`: green on the tint, or bold green where there is
    /// none.
    DiffAddSign,
    /// A removed line's `−`: red on the tint, or bold red where there is none.
    DiffDelSign,
    /// An added line's number: muted on the tint.
    DiffAddNumber,
    /// A removed line's number: muted on the tint.
    DiffDelNumber,
    /// A context line of a diff, with its number: muted.
    DiffContext,
    /// Code: a keyword.
    SyntaxKeyword,
    /// Code: a type, or a name the language gives (`Some`, `self`).
    SyntaxType,
    /// Code: a string or a character.
    SyntaxString,
    /// Code: a number or another constant.
    SyntaxNumber,
    /// Code: a comment.
    SyntaxComment,
}

impl Token {
    /// Every token, in order.
    pub const ALL: [Self; 36] = [
        Self::Text,
        Self::Muted,
        Self::Accent,
        Self::AccentText,
        Self::User,
        Self::Title,
        Self::Code,
        Self::Heading,
        Self::Success,
        Self::Warning,
        Self::Error,
        Self::DiffAdd,
        Self::DiffDel,
        Self::DiffAddBg,
        Self::DiffDelBg,
        Self::DiffEmph,
        Self::Answer,
        Self::Reasoning,
        Self::Link,
        Self::Border,
        Self::BorderSelected,
        Self::Hint,
        Self::Key,
        Self::Selected,
        Self::DiffAddLine,
        Self::DiffDelLine,
        Self::DiffAddSign,
        Self::DiffDelSign,
        Self::DiffAddNumber,
        Self::DiffDelNumber,
        Self::DiffContext,
        Self::SyntaxKeyword,
        Self::SyntaxType,
        Self::SyntaxString,
        Self::SyntaxNumber,
        Self::SyntaxComment,
    ];
}

/// The colours a theme assigns, by token: an ANSI colour, a 256-colour index
/// or an exact colour. The default theme's are [`Palette::dark`] and
/// [`Palette::light`]; a theme of the user's own fills the same fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// The accent.
    pub accent: Color,
    /// The accent at 16 colours. The theme names it, because the automatic
    /// reduction of the logo's purple is bright blue.
    pub accent_16: Color,
    /// The colour of text in the accent; `None` computes the accent's tint
    /// that reaches [`TEXT_CONTRAST`] against the background. Not used at 16
    /// colours, where the text is in `accent_16` on a light background and
    /// bold in the default colour on a dark one.
    pub accent_text: Option<Color>,
    /// Muted text.
    pub muted: Color,
    /// Inline code.
    pub code: Color,
    /// Inline code is bold as well: where its colour alone is under the
    /// contrast target for text.
    pub code_bold: bool,
    /// Success.
    pub success: Color,
    /// Warnings.
    pub warning: Color,
    /// Errors.
    pub error: Color,
    /// An added line's sign, and the line where there is no tint.
    pub diff_add: Color,
    /// A removed line's sign, and the line where there is no tint.
    pub diff_del: Color,
    /// The colour mixed into the background behind an added line.
    pub diff_add_tint: Rgb,
    /// The colour mixed into the background behind a removed line.
    pub diff_del_tint: Rgb,
    /// How much of the tint colour is mixed in, in percent.
    pub diff_tint_percent: u8,
    /// Keywords in code.
    pub syntax_keyword: Color,
    /// Types in code.
    pub syntax_type: Color,
    /// Strings in code.
    pub syntax_string: Color,
    /// Numbers in code.
    pub syntax_number: Color,
    /// Comments in code.
    pub syntax_comment: Color,
}

impl Palette {
    /// The default theme's colours on a dark background: the bright colours
    /// of the terminal's palette, which reach the contrast target where the
    /// normal ones do not (Campbell's red is 3.2 to 1 on its background).
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            accent: Color::Rgb(ACCENT.0, ACCENT.1, ACCENT.2),
            accent_16: Color::Ansi(13),
            accent_text: None,
            muted: Color::Ansi(8),
            code: Color::Ansi(14),
            code_bold: false,
            success: Color::Ansi(10),
            warning: Color::Ansi(11),
            error: Color::Ansi(9),
            diff_add: Color::Ansi(10),
            diff_del: Color::Ansi(9),
            diff_add_tint: (0x2e, 0xa0, 0x43),
            diff_del_tint: (0xe0, 0x3e, 0x3e),
            diff_tint_percent: 20,
            syntax_keyword: Color::Ansi(13),
            syntax_type: Color::Ansi(14),
            syntax_string: Color::Ansi(10),
            syntax_number: Color::Ansi(11),
            syntax_comment: Color::Ansi(8),
        }
    }

    /// The default theme's colours on a light background: the normal colours
    /// of the terminal's palette, and inline code bold.
    #[must_use]
    pub const fn light() -> Self {
        Self {
            accent: Color::Rgb(ACCENT.0, ACCENT.1, ACCENT.2),
            accent_16: Color::Ansi(5),
            accent_text: None,
            muted: Color::Ansi(8),
            code: Color::Ansi(6),
            code_bold: true,
            success: Color::Ansi(2),
            warning: Color::Ansi(3),
            error: Color::Ansi(1),
            diff_add: Color::Ansi(2),
            diff_del: Color::Ansi(1),
            diff_add_tint: (0x2e, 0xa0, 0x43),
            diff_del_tint: (0xe0, 0x3e, 0x3e),
            diff_tint_percent: 13,
            syntax_keyword: Color::Ansi(5),
            syntax_type: Color::Ansi(6),
            syntax_string: Color::Ansi(2),
            syntax_number: Color::Ansi(3),
            syntax_comment: Color::Ansi(8),
        }
    }

    /// The default theme's colours for a background.
    #[must_use]
    pub const fn for_background(background: Background) -> Self {
        match background {
            Background::Dark => Self::dark(),
            Background::Light => Self::light(),
        }
    }
}

/// A theme made for one terminal. See the [module documentation](self).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    depth: ColorDepth,
    background: Background,
    background_rgb: Rgb,
    styles: [Style; Token::ALL.len()],
}

impl Theme {
    /// The default theme for a terminal of `depth` whose background is
    /// `background`, or [`ASSUMED_BACKGROUND`] when it did not say.
    #[must_use]
    pub fn new(depth: ColorDepth, background: Option<Rgb>) -> Self {
        let rgb = background.unwrap_or(ASSUMED_BACKGROUND);
        Self::with_palette(
            &Palette::for_background(Background::of(rgb)),
            depth,
            background,
        )
    }

    /// The default theme for the terminal `caps` describes.
    #[must_use]
    pub fn for_terminal(caps: &Capabilities) -> Self {
        Self::new(caps.color, caps.background)
    }

    /// A theme of the given colours, for a terminal of `depth` whose
    /// background is `background` (or [`ASSUMED_BACKGROUND`]).
    #[must_use]
    pub fn with_palette(palette: &Palette, depth: ColorDepth, background: Option<Rgb>) -> Self {
        let rgb = background.unwrap_or(ASSUMED_BACKGROUND);
        let kind = Background::of(rgb);
        let mut styles = [Style::PLAIN; Token::ALL.len()];
        for token in Token::ALL {
            styles[token as usize] =
                resolve(token, palette, depth, kind, rgb, background.is_some());
        }
        Self {
            depth,
            background: kind,
            background_rgb: rgb,
            styles,
        }
    }

    /// The style of `token`.
    #[must_use]
    pub fn style(&self, token: Token) -> Style {
        self.styles[token as usize]
    }

    /// The depth the theme was made for.
    #[must_use]
    pub fn depth(&self) -> ColorDepth {
        self.depth
    }

    /// Whether the background is dark or light.
    #[must_use]
    pub fn background(&self) -> Background {
        self.background
    }

    /// The background colour the theme was made against: the terminal's, or
    /// [`ASSUMED_BACKGROUND`].
    #[must_use]
    pub fn background_rgb(&self) -> Rgb {
        self.background_rgb
    }
}

/// The style of one token. See [`Token`] for what each is.
fn resolve(
    token: Token,
    palette: &Palette,
    depth: ColorDepth,
    background: Background,
    background_rgb: Rgb,
    reported: bool,
) -> Style {
    let plain = Style::PLAIN;
    let mono = depth == ColorDepth::Mono;
    let fg = |color: Color| plain.fg(color.for_depth(depth));
    let muted = if mono { plain.dim() } else { fg(palette.muted) };
    let accent = match depth {
        ColorDepth::Mono => Color::Default,
        ColorDepth::Ansi16 => palette.accent_16,
        _ => palette.accent.for_depth(depth),
    };
    let accent_text = match (depth, background) {
        (ColorDepth::Mono, _) | (ColorDepth::Ansi16, Background::Dark) => plain.bold(),
        (ColorDepth::Ansi16, Background::Light) => plain.fg(palette.accent_16),
        _ => plain.fg(accent_text_color(
            palette,
            depth,
            background,
            background_rgb,
        )),
    };
    // The tints exist in true colour only, and only against a background the
    // terminal reported: mixed into one it did not, they could be dark bars
    // under dark text. Elsewhere a changed line is drawn in its colour.
    let tint = |base: Rgb| {
        (depth == ColorDepth::TrueColor && reported).then(|| {
            let (r, g, b) = mix(
                background_rgb,
                base,
                f64::from(palette.diff_tint_percent) / 100.0,
            );
            Color::Rgb(r, g, b)
        })
    };
    let on = |style: Style, tint: Option<Color>| match tint {
        Some(color) => style.bg(color),
        None => style,
    };
    let add_tint = tint(palette.diff_add_tint);
    let del_tint = tint(palette.diff_del_tint);
    let colour_or = |color: Color, without: Style| if mono { without } else { fg(color) };

    match token {
        Token::Text => plain,
        Token::DiffAddBg => on(plain, add_tint),
        Token::DiffDelBg => on(plain, del_tint),
        Token::Muted | Token::Border | Token::Hint | Token::DiffContext => muted,
        Token::Accent => plain.fg(accent),
        Token::AccentText => accent_text,
        Token::User | Token::BorderSelected if mono => plain.bold(),
        Token::User => plain.fg(accent).bold(),
        Token::BorderSelected => plain.fg(accent),
        Token::Title | Token::Heading | Token::DiffEmph => plain.bold(),
        Token::Code if mono => plain.bold(),
        Token::Code if palette.code_bold => fg(palette.code).bold(),
        Token::Code => fg(palette.code),
        Token::Success => colour_or(palette.success, plain),
        Token::Warning => colour_or(palette.warning, plain),
        Token::Error => colour_or(palette.error, plain.bold()),
        Token::DiffAdd => colour_or(palette.diff_add, plain),
        Token::DiffDel => colour_or(palette.diff_del, plain.dim()),
        Token::Answer if mono => plain.bold(),
        Token::Answer => plain,
        Token::Reasoning => muted.italic(),
        Token::Link => plain.underline(),
        Token::Key | Token::Selected => accent_text.bold(),
        Token::DiffAddLine => match add_tint {
            Some(color) => plain.bg(color),
            None => colour_or(palette.diff_add, plain),
        },
        Token::DiffDelLine => match del_tint {
            Some(color) => plain.bg(color),
            None => colour_or(palette.diff_del, plain.dim()),
        },
        Token::DiffAddSign => match add_tint {
            Some(color) => fg(palette.diff_add).bg(color),
            None if mono => plain.bold(),
            None => fg(palette.diff_add).bold(),
        },
        Token::DiffDelSign => match del_tint {
            Some(color) => fg(palette.diff_del).bg(color),
            None if mono => plain.bold(),
            None => fg(palette.diff_del).bold(),
        },
        Token::DiffAddNumber => on(muted, add_tint),
        Token::DiffDelNumber => on(muted, del_tint),
        Token::SyntaxKeyword => colour_or(palette.syntax_keyword, plain),
        Token::SyntaxType => colour_or(palette.syntax_type, plain),
        Token::SyntaxString => colour_or(palette.syntax_string, plain),
        Token::SyntaxNumber => colour_or(palette.syntax_number, plain),
        Token::SyntaxComment => colour_or(palette.syntax_comment, plain.dim()),
    }
}

/// The colour of text in the accent, in true colour or 256 colours.
fn accent_text_color(
    palette: &Palette,
    depth: ColorDepth,
    background: Background,
    background_rgb: Rgb,
) -> Color {
    if let Some(color) = palette.accent_text {
        return color.for_depth(depth);
    }
    let Color::Rgb(r, g, b) = palette.accent else {
        // An accent from the terminal's palette cannot be tinted.
        return palette.accent.for_depth(depth);
    };
    let toward = match background {
        Background::Dark => (255, 255, 255),
        Background::Light => (0, 0, 0),
    };
    let tint = tint_for((r, g, b), background_rgb, TEXT_CONTRAST, toward);
    match depth {
        ColorDepth::Indexed256 => nearest_indexed_reaching(tint, background_rgb, TEXT_CONTRAST),
        _ => Color::Rgb(tint.0, tint.1, tint.2),
    }
}

/// The least mix of `base` toward `toward`, in steps of 1%, that reaches
/// `target` against `background`. If none does, `toward` itself.
fn tint_for(base: Rgb, background: Rgb, target: f64, toward: Rgb) -> Rgb {
    for step in 0..=100u8 {
        let color = mix(base, toward, f64::from(step) / 100.0);
        if contrast(color, background) >= target {
            return color;
        }
    }
    toward
}

/// The colour of the 256-colour palette nearest to `rgb` among those that
/// reach `target` against `background` and keep its hue: within 15 degrees,
/// and not grey. Taking the nearest colour first and then darkening or
/// lightening it would not keep the hue: the purple's darker neighbours in
/// the palette's cube are blue. If no colour keeps the hue, the nearest that
/// reaches the target.
fn nearest_indexed_reaching(rgb: Rgb, background: Rgb, target: f64) -> Color {
    let distance = |a: Rgb, b: Rgb| {
        let d = |x: u8, y: u8| u32::from(x.abs_diff(y)).pow(2);
        d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)
    };
    let (hue, _) = hue_and_saturation(rgb);
    let mut keeps_hue: Option<(u32, u8)> = None;
    let mut any: Option<(u32, u8)> = None;
    for index in 16..=255u8 {
        let Some(shown) = shown_rgb(Color::Indexed(index)) else {
            continue;
        };
        if contrast(shown, background) < target {
            continue;
        }
        let gap = distance(shown, rgb);
        let (other, saturation) = hue_and_saturation(shown);
        let turn = (other - hue).abs();
        if saturation >= 0.4
            && turn.min(360.0 - turn) <= 15.0
            && keeps_hue.is_none_or(|(least, _)| gap < least)
        {
            keeps_hue = Some((gap, index));
        }
        if any.is_none_or(|(least, _)| gap < least) {
            any = Some((gap, index));
        }
    }
    keeps_hue.or(any).map_or(
        Color::Rgb(rgb.0, rgb.1, rgb.2).for_depth(ColorDepth::Indexed256),
        |(_, index)| Color::Indexed(index),
    )
}

/// A colour's hue, in degrees, and its saturation, from 0 to 1 (as HSV
/// defines them).
fn hue_and_saturation((r, g, b): Rgb) -> (f64, f64) {
    let (r, g, b) = (f64::from(r), f64::from(g), f64::from(b));
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max - min;
    if chroma == 0.0 {
        return (0.0, 0.0);
    }
    let sector = if max == r {
        ((g - b) / chroma).rem_euclid(6.0)
    } else if max == g {
        (b - r) / chroma + 2.0
    } else {
        (r - g) / chroma + 4.0
    };
    (sector * 60.0, chroma / max)
}

/// The red, green and blue a colour is shown as, where that is known: an
/// exact colour, or one of the 256-colour palette's cube and grey ramp (whose
/// colours are standard). The first sixteen are the terminal's own.
fn shown_rgb(color: Color) -> Option<Rgb> {
    const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Indexed(index @ 16..=231) => {
            let cube = usize::from(index - 16);
            Some((CUBE[cube / 36], CUBE[cube / 6 % 6], CUBE[cube % 6]))
        }
        Color::Indexed(index @ 232..=255) => {
            let level = 8 + (index - 232) * 10;
            Some((level, level, level))
        }
        _ => None,
    }
}

/// `a` mixed with `b`: `amount` of `b`, from 0 to 1. Each channel is rounded
/// to the nearest, a half to the even one.
#[must_use]
pub fn mix(a: Rgb, b: Rgb, amount: f64) -> Rgb {
    let channel = |x: u8, y: u8| {
        let (x, y) = (f64::from(x), f64::from(y));
        // In 0..=255 after the clamp, so the cast is exact.
        (x + (y - x) * amount).round_ties_even().clamp(0.0, 255.0) as u8
    };
    (channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}

/// The relative luminance of a colour, as WCAG 2 defines it: 0 for black, 1
/// for white.
#[must_use]
pub fn luminance(rgb: Rgb) -> f64 {
    let linear = |channel: u8| {
        let c = f64::from(channel) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgb.0) + 0.7152 * linear(rgb.1) + 0.0722 * linear(rgb.2)
}

/// The contrast between two colours, as WCAG 2 defines it: from 1 (the
/// same) to 21 (black and white).
#[must_use]
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    let (high, low) = if la > lb { (la, lb) } else { (lb, la) };
    (high + 0.05) / (low + 0.05)
}

#[cfg(test)]
mod tests;
