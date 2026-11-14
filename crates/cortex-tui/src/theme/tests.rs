use super::*;

/// Windows Terminal's default dark scheme, Campbell: foreground, background
/// and the sixteen colours.
const CAMPBELL: Scheme = Scheme {
    fg: (0xcc, 0xcc, 0xcc),
    bg: (0x0c, 0x0c, 0x0c),
    ansi: [
        (0x0c, 0x0c, 0x0c),
        (0xc5, 0x0f, 0x1f),
        (0x13, 0xa1, 0x0e),
        (0xc1, 0x9c, 0x00),
        (0x00, 0x37, 0xda),
        (0x88, 0x17, 0x98),
        (0x3a, 0x96, 0xdd),
        (0xcc, 0xcc, 0xcc),
        (0x76, 0x76, 0x76),
        (0xe7, 0x48, 0x56),
        (0x16, 0xc6, 0x0c),
        (0xf9, 0xf1, 0xa5),
        (0x3b, 0x78, 0xff),
        (0xb4, 0x00, 0x9e),
        (0x61, 0xd6, 0xd6),
        (0xf2, 0xf2, 0xf2),
    ],
};

/// One Half Light, a light scheme that ships with Windows Terminal.
const ONE_HALF_LIGHT: Scheme = Scheme {
    fg: (0x38, 0x3a, 0x42),
    bg: (0xfa, 0xfa, 0xfa),
    ansi: [
        (0x38, 0x3a, 0x42),
        (0xe4, 0x56, 0x49),
        (0x50, 0xa1, 0x4f),
        (0xc1, 0x83, 0x01),
        (0x01, 0x84, 0xbc),
        (0xa6, 0x26, 0xa4),
        (0x09, 0x97, 0xb3),
        (0xfa, 0xfa, 0xfa),
        (0x4f, 0x52, 0x5d),
        (0xdf, 0x6c, 0x75),
        (0x98, 0xc3, 0x79),
        (0xe4, 0xc0, 0x7a),
        (0x61, 0xaf, 0xef),
        (0xc5, 0x77, 0xdd),
        (0x56, 0xb5, 0xc1),
        (0xff, 0xff, 0xff),
    ],
};

struct Scheme {
    fg: Rgb,
    bg: Rgb,
    ansi: [Rgb; 16],
}

/// Backgrounds the accent's text tint is checked against: the two schemes
/// above, and the default backgrounds of other common terminals and
/// schemes.
const BACKGROUNDS: [(&str, Rgb); 12] = [
    ("Campbell", (0x0c, 0x0c, 0x0c)),
    ("One Half Light", (0xfa, 0xfa, 0xfa)),
    ("One Half Dark", (0x28, 0x2c, 0x34)),
    ("Solarized Dark", (0x00, 0x2b, 0x36)),
    ("Solarized Light", (0xfd, 0xf6, 0xe3)),
    ("Tango Dark and xterm", (0x00, 0x00, 0x00)),
    ("Tango Light and xterm reversed", (0xff, 0xff, 0xff)),
    ("Ubuntu", (0x30, 0x0a, 0x24)),
    ("GNOME Terminal dark", (0x17, 0x14, 0x21)),
    ("Visual Studio Code dark", (0x1e, 0x1e, 0x1e)),
    ("Dracula", (0x28, 0x2a, 0x36)),
    ("Gruvbox light", (0xfb, 0xf1, 0xc7)),
];

fn rounded(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

#[test]
fn the_tokens_are_listed_in_their_order() {
    for (index, token) in Token::ALL.iter().enumerate() {
        assert_eq!(*token as usize, index, "{token:?}");
    }
}

#[test]
fn the_accent_at_16_colours_is_the_one_the_theme_names_not_the_reduction() {
    // The automatic reduction would make the purple bright blue.
    assert_eq!(
        Color::Rgb(ACCENT.0, ACCENT.1, ACCENT.2).for_depth(ColorDepth::Ansi16),
        Color::Ansi(12)
    );
    let dark = Theme::new(ColorDepth::Ansi16, Some(CAMPBELL.bg));
    let light = Theme::new(ColorDepth::Ansi16, Some(ONE_HALF_LIGHT.bg));
    for token in [Token::Accent, Token::User, Token::BorderSelected] {
        assert_eq!(dark.style(token).fg, Color::Ansi(13), "{token:?}");
        assert_eq!(light.style(token).fg, Color::Ansi(5), "{token:?}");
    }
    // 256 colours: the nearest, index 99. True colour: the purple.
    let indexed = Theme::new(ColorDepth::Indexed256, None);
    assert_eq!(indexed.style(Token::Accent).fg, Color::Indexed(99));
    let exact = Theme::new(ColorDepth::TrueColor, None);
    assert_eq!(exact.style(Token::Accent).fg, Color::Rgb(0x7a, 0x3c, 0xfb));
    assert_eq!(
        exact.style(Token::User),
        Style::PLAIN.fg(Color::Rgb(0x7a, 0x3c, 0xfb)).bold()
    );
}

#[test]
fn on_a_dark_background_at_16_colours_accent_text_is_bold_in_the_default_colour() {
    let dark = Theme::new(ColorDepth::Ansi16, Some(CAMPBELL.bg));
    for token in [Token::AccentText, Token::Key, Token::Selected] {
        let style = dark.style(token);
        assert_eq!(style.fg, Color::Default, "{token:?}");
        assert!(style.bold, "{token:?}");
    }
    // No magenta reaches the target there: 2.4 and 3.2 to 1 on Campbell.
    assert_eq!(rounded(contrast(CAMPBELL.ansi[5], CAMPBELL.bg)), 2.4);
    assert_eq!(rounded(contrast(CAMPBELL.ansi[13], CAMPBELL.bg)), 3.2);
    // On a light background magenta does (5.9 to 1), and is used as it is.
    let light = Theme::new(ColorDepth::Ansi16, Some(ONE_HALF_LIGHT.bg));
    assert_eq!(
        light.style(Token::AccentText),
        Style::PLAIN.fg(Color::Ansi(5))
    );
    assert_eq!(
        light.style(Token::Key),
        Style::PLAIN.fg(Color::Ansi(5)).bold()
    );
    assert_eq!(
        rounded(contrast(ONE_HALF_LIGHT.ansi[5], ONE_HALF_LIGHT.bg)),
        5.9
    );
}

/// The colour an accent-text style is shown as.
fn shown(theme: &Theme) -> Rgb {
    let color = theme.style(Token::AccentText).fg;
    shown_rgb(color).unwrap_or_else(|| panic!("{color:?} has no known colour"))
}

#[test]
fn the_accents_text_tint_reaches_its_target_on_every_sample_background() {
    for (name, background) in BACKGROUNDS {
        for depth in [ColorDepth::TrueColor, ColorDepth::Indexed256] {
            let theme = Theme::new(depth, Some(background));
            let ratio = contrast(shown(&theme), background);
            assert!(
                ratio >= TEXT_CONTRAST,
                "{name} at {depth:?}: the accent's text is {ratio:.2} to 1"
            );
        }
    }
    // The figures 09-tui gives. On Campbell the purple is 3.6 to 1, and its
    // tint `#8D57FC` 4.6 to 1.
    assert_eq!(rounded(contrast(ACCENT, CAMPBELL.bg)), 3.6);
    let dark = Theme::new(ColorDepth::TrueColor, Some(CAMPBELL.bg));
    assert_eq!(
        dark.style(Token::AccentText).fg,
        Color::Rgb(0x8d, 0x57, 0xfc)
    );
    assert_eq!(rounded(contrast((0x8d, 0x57, 0xfc), CAMPBELL.bg)), 4.6);
    // On One Half Light the purple itself is 5.2 to 1, and is used.
    assert_eq!(rounded(contrast(ACCENT, ONE_HALF_LIGHT.bg)), 5.2);
    let light = Theme::new(ColorDepth::TrueColor, Some(ONE_HALF_LIGHT.bg));
    assert_eq!(
        light.style(Token::AccentText).fg,
        Color::Rgb(0x7a, 0x3c, 0xfb)
    );
    // When the background is not known, the dark tint is used.
    let unknown = Theme::new(ColorDepth::TrueColor, None);
    assert_eq!(
        unknown.style(Token::AccentText).fg,
        Color::Rgb(0x8d, 0x57, 0xfc)
    );
    assert_eq!(unknown.background(), Background::Dark);
    // At 256 colours on a light background the purple's nearest, index 99,
    // is under the target, so a darker one is chosen.
    assert!(contrast((0x87, 0x5f, 0xff), ONE_HALF_LIGHT.bg) < TEXT_CONTRAST);
    let light_256 = Theme::new(ColorDepth::Indexed256, Some(ONE_HALF_LIGHT.bg));
    assert_ne!(light_256.style(Token::AccentText).fg, Color::Indexed(99));
}

#[test]
fn a_tint_is_the_least_mix_that_reaches_the_target() {
    let dark = Theme::new(ColorDepth::TrueColor, Some(CAMPBELL.bg));
    let Color::Rgb(r, g, b) = dark.style(Token::AccentText).fg else {
        panic!("not an exact colour");
    };
    // One step less does not reach it.
    let less = mix(ACCENT, (255, 255, 255), 0.13);
    assert!(contrast(less, CAMPBELL.bg) < TEXT_CONTRAST);
    assert_eq!((r, g, b), mix(ACCENT, (255, 255, 255), 0.14));
    // A palette that names its text colour is used as given.
    let palette = Palette {
        accent_text: Some(Color::Rgb(1, 2, 3)),
        ..Palette::dark()
    };
    let given = Theme::with_palette(&palette, ColorDepth::TrueColor, None);
    assert_eq!(given.style(Token::AccentText).fg, Color::Rgb(1, 2, 3));
}

#[test]
fn the_figures_for_code_blue_and_the_diff_tints_hold() {
    // Inline code: bright cyan is 11.3 to 1 on Campbell; cyan is 3.3 to 1 on
    // One Half Light, so it is bold there.
    assert_eq!(rounded(contrast(CAMPBELL.ansi[14], CAMPBELL.bg)), 11.3);
    assert_eq!(
        rounded(contrast(ONE_HALF_LIGHT.ansi[6], ONE_HALF_LIGHT.bg)),
        3.3
    );
    let dark = Theme::new(ColorDepth::TrueColor, Some(CAMPBELL.bg));
    let light = Theme::new(ColorDepth::TrueColor, Some(ONE_HALF_LIGHT.bg));
    assert_eq!(dark.style(Token::Code), Style::PLAIN.fg(Color::Ansi(14)));
    assert_eq!(
        light.style(Token::Code),
        Style::PLAIN.fg(Color::Ansi(6)).bold()
    );
    // Blue on a dark background: 2.4 to 1, and bright blue 5.0.
    assert_eq!(rounded(contrast(CAMPBELL.ansi[4], CAMPBELL.bg)), 2.4);
    assert_eq!(rounded(contrast(CAMPBELL.ansi[12], CAMPBELL.bg)), 5.0);
    // Normal red is under the target on Campbell; the dark theme's errors
    // are bright red.
    assert_eq!(rounded(contrast(CAMPBELL.ansi[1], CAMPBELL.bg)), 3.2);
    assert_eq!(rounded(contrast(CAMPBELL.ansi[9], CAMPBELL.bg)), 5.1);
    assert_eq!(dark.style(Token::Error).fg, Color::Ansi(9));
    assert_eq!(light.style(Token::Error).fg, Color::Ansi(1));
    // The tints: 20% of green or red into a dark background, 13% into a
    // light one. Text on them keeps the default foreground, 9.1 to 10.2 to 1.
    let expected = [
        (&dark, (19, 42, 23), (54, 22, 22), CAMPBELL.fg),
        (&light, (223, 238, 226), (247, 226, 226), ONE_HALF_LIGHT.fg),
    ];
    for (theme, add, del, fg) in expected {
        assert_eq!(
            theme.style(Token::DiffAddLine),
            Style::PLAIN.bg(Color::Rgb(add.0, add.1, add.2))
        );
        assert_eq!(
            theme.style(Token::DiffDelLine),
            Style::PLAIN.bg(Color::Rgb(del.0, del.1, del.2))
        );
        for tint in [add, del] {
            let ratio = rounded(contrast(fg, tint));
            assert!((9.1..=10.2).contains(&ratio), "{ratio}");
        }
    }
    // Without true colour there is no tint: the line takes its colour.
    for depth in [ColorDepth::Indexed256, ColorDepth::Ansi16] {
        let theme = Theme::new(depth, Some(CAMPBELL.bg));
        assert_eq!(
            theme.style(Token::DiffAddLine),
            Style::PLAIN.fg(Color::Ansi(10))
        );
        assert_eq!(
            theme.style(Token::DiffDelSign),
            Style::PLAIN.fg(Color::Ansi(9)).bold()
        );
        assert_eq!(theme.style(Token::DiffAddBg), Style::PLAIN);
    }
}

#[test]
fn without_colour_nothing_is_coloured_and_meaning_keeps_an_attribute() {
    for background in [CAMPBELL.bg, ONE_HALF_LIGHT.bg] {
        let mono = Theme::new(ColorDepth::Mono, Some(background));
        for token in Token::ALL {
            let style = mono.style(token);
            assert_eq!(
                (style.fg, style.bg),
                (Color::Default, Color::Default),
                "{token:?}"
            );
        }
        let plain = Style::PLAIN;
        for (token, style) in [
            (Token::Muted, plain.dim()),
            (Token::Border, plain.dim()),
            (Token::Hint, plain.dim()),
            (Token::Reasoning, plain.dim().italic()),
            (Token::Error, plain.bold()),
            (Token::User, plain.bold()),
            (Token::Answer, plain.bold()),
            (Token::AccentText, plain.bold()),
            (Token::Key, plain.bold()),
            (Token::Selected, plain.bold()),
            (Token::Code, plain.bold()),
            (Token::DiffDelLine, plain.dim()),
            (Token::DiffAddLine, plain),
            (Token::DiffAddSign, plain.bold()),
            (Token::DiffDelSign, plain.bold()),
            (Token::DiffContext, plain.dim()),
        ] {
            assert_eq!(mono.style(token), style, "{token:?}");
        }
    }
}

#[test]
fn the_background_decides_the_palette() {
    for (name, background) in BACKGROUNDS {
        let expected =
            if name.contains("Light") || name.contains("light") || name.contains("reversed") {
                Background::Light
            } else {
                Background::Dark
            };
        assert_eq!(Background::of(background), expected, "{name}");
        assert_eq!(
            Theme::new(ColorDepth::TrueColor, Some(background)).background(),
            expected
        );
    }
    assert_eq!(Background::of((0x77, 0x77, 0x77)), Background::Light);
    let theme = Theme::for_terminal(&cortex_term::Capabilities::detect(
        |name| (name == "NO_COLOR").then(|| "1".to_owned()),
        cortex_term::Platform::Unix,
        cortex_term::Overrides::default(),
    ));
    assert_eq!(theme.depth(), ColorDepth::Mono);
    assert_eq!(theme.background_rgb(), ASSUMED_BACKGROUND);
}

#[test]
fn mixing_rounds_half_to_even() {
    assert_eq!(mix((0, 0, 0), (1, 3, 5), 0.5), (0, 2, 2));
    assert_eq!(mix((10, 20, 30), (10, 20, 30), 0.7), (10, 20, 30));
    assert_eq!(mix((0, 0, 0), (255, 255, 255), 1.0), (255, 255, 255));
    assert_eq!(rounded(contrast((0, 0, 0), (255, 255, 255))), 21.0);
    assert_eq!(contrast((5, 5, 5), (5, 5, 5)), 1.0);
}
