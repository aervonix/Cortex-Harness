//! What the terminal can do: detected from the environment at start, asked
//! of the terminal with the first frame, and overridden by settings
//! (`docs/architecture/09-tui.md`, "Terminal layer").
//!
//! Start-up never waits for an answer. [`Capabilities::detect`] decides from
//! the environment alone, conservatively; the queries in
//! [`Capabilities::QUERIES`] go out with the first frame; and each reply,
//! when it comes, goes to [`Capabilities::apply`], which upgrades what later
//! frames do. An override always wins over both.

use crate::input::Reply;
use crate::style::ColorDepth;

/// What a setting says about one capability: on or off whatever the
/// terminal says. `None` leaves it to detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Overrides {
    /// Synchronized updates.
    pub sync: Option<bool>,
    /// The enhanced keyboard protocol.
    pub keyboard: Option<bool>,
    /// How many colours.
    pub color: Option<ColorDepth>,
    /// Links in text.
    pub hyperlinks: Option<bool>,
    /// Unicode glyphs, or ASCII ones.
    pub unicode: Option<bool>,
}

/// The system the terminal runs on, which changes what the environment
/// means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// Linux, and any other Unix.
    Unix,
    /// Windows.
    Windows,
}

impl Platform {
    /// The system this program was built for.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Unix
        }
    }
}

/// What the terminal can do, as far as is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent capabilities of a terminal, each present or not"
)]
pub struct Capabilities {
    /// Frames are wrapped in synchronized-update brackets (mode 2026), so a
    /// terminal that has them shows each frame whole.
    pub sync: bool,
    /// The terminal has the enhanced keyboard protocol, which tells apart
    /// keys that are otherwise the same bytes (Shift+Enter and Enter). Known
    /// only from its reply; until then, `false`.
    pub keyboard: bool,
    /// The terminal has Windows Terminal's win32 input mode (mode 9001),
    /// which reports every key with all its modifiers. Known only from its
    /// reply; until then, `false`. The `keyboard` override covers it too.
    pub win32_input: bool,
    /// How many colours it shows.
    pub color: ColorDepth,
    /// It shows links in text (OSC 8).
    pub hyperlinks: bool,
    /// It shows Unicode glyphs; otherwise the interface uses ASCII ones.
    pub unicode: bool,
    /// Its background colour, from its reply. Used to tint the accent.
    pub background: Option<(u8, u8, u8)>,
    /// Escape sequences are of no use at all (`TERM=dumb`): the interactive
    /// interface should not start.
    pub dumb: bool,
    overrides: Overrides,
}

/// Terminals known to show links, by the environment variable each sets
/// (and, for `TERM_PROGRAM`, its value).
const LINK_TERMINALS: &[&str] = &["iTerm.app", "WezTerm", "vscode", "ghostty", "Hyper"];

/// Terminals on the deny-list for synchronized updates, by `TERM`. A
/// terminal that does not know the mode ignores it, which is why the list is
/// short: these are the ones known to show the sequence or to misbehave.
const NO_SYNC_TERMS: &[&str] = &["dumb", "linux"];

impl Capabilities {
    /// The queries sent with the first frame: whether synchronized updates
    /// are supported (DECRQM 2026), whether the win32 input mode is (DECRQM
    /// 9001), the enhanced keyboard protocol's flags,
    /// the background colour, and last the device attributes, which every
    /// terminal answers.
    pub const QUERIES: &'static [u8] = b"\x1b[?2026$p\x1b[?9001$p\x1b[?u\x1b]11;?\x1b\\\x1b[c";

    /// Decides from the environment, then applies `overrides`. `env` reads
    /// one environment variable.
    pub fn detect(
        env: impl Fn(&str) -> Option<String>,
        platform: Platform,
        overrides: Overrides,
    ) -> Self {
        let var = |name: &str| env(name).filter(|value| !value.is_empty());
        let term = var("TERM").unwrap_or_default();
        let program = var("TERM_PROGRAM").unwrap_or_default();
        let windows_terminal = var("WT_SESSION").is_some();
        let dumb = term == "dumb";
        // On Windows a terminal that sets none of these is taken to be the
        // old console host, which has no synchronized updates and draws few
        // glyphs (`09-tui.md`, "Windows").
        let legacy_console = platform == Platform::Windows
            && !windows_terminal
            && program.is_empty()
            && term.is_empty();

        let color = if var("NO_COLOR").is_some() {
            ColorDepth::Mono
        } else if matches!(var("COLORTERM").as_deref(), Some("truecolor" | "24bit"))
            || windows_terminal
            || LINK_TERMINALS.contains(&program.as_str())
            || term == "xterm-kitty"
            || term.contains("direct")
        {
            ColorDepth::TrueColor
        } else if term.contains("256") {
            ColorDepth::Indexed256
        } else {
            ColorDepth::Ansi16
        };

        let hyperlinks = !dumb
            && var("TMUX").is_none()
            && (windows_terminal
                || LINK_TERMINALS.contains(&program.as_str())
                || term == "xterm-kitty"
                || var("KONSOLE_VERSION").is_some()
                || var("VTE_VERSION")
                    .and_then(|version| version.parse::<u32>().ok())
                    .is_some_and(|version| version >= 5000));

        let unicode = match platform {
            Platform::Windows => !legacy_console,
            Platform::Unix => ["LC_ALL", "LC_CTYPE", "LANG"]
                .iter()
                .find_map(|name| var(name))
                .is_some_and(|locale| {
                    let locale = locale.to_ascii_lowercase();
                    locale.contains("utf-8") || locale.contains("utf8")
                }),
        };

        let mut caps = Self {
            sync: !dumb && !legacy_console && !NO_SYNC_TERMS.contains(&term.as_str()),
            keyboard: false,
            win32_input: false,
            color,
            hyperlinks,
            unicode,
            background: None,
            dumb,
            overrides,
        };
        caps.enforce();
        caps
    }

    /// Takes in a reply to one of [`QUERIES`](Self::QUERIES). Returns
    /// whether anything changed.
    pub fn apply(&mut self, reply: &Reply) -> bool {
        let before = *self;
        match *reply {
            Reply::Mode { mode: 2026, state } => match state {
                1..=3 => self.sync = true,
                4 => self.sync = false,
                // 0: the terminal does not know the mode. It ignores it, so
                // the brackets stay: they cost nothing.
                _ => {}
            },
            Reply::Mode { mode: 9001, state } => self.win32_input = matches!(state, 1..=3),
            Reply::Keyboard(_) => self.keyboard = true,
            Reply::Background(r, g, b) => self.background = Some((r, g, b)),
            Reply::Mode { .. } | Reply::DeviceAttributes => {}
        }
        self.enforce();
        *self != before
    }

    /// The overrides, as given to [`detect`](Self::detect).
    #[must_use]
    pub fn overrides(&self) -> Overrides {
        self.overrides
    }

    fn enforce(&mut self) {
        let o = self.overrides;
        if let Some(sync) = o.sync {
            self.sync = sync;
        }
        if let Some(keyboard) = o.keyboard {
            self.keyboard = keyboard;
            self.win32_input &= keyboard;
        }
        if let Some(color) = o.color {
            self.color = color;
        }
        if let Some(hyperlinks) = o.hyperlinks {
            self.hyperlinks = hyperlinks;
        }
        if let Some(unicode) = o.unicode {
            self.unicode = unicode;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(vars: &[(&str, &str)], platform: Platform) -> Capabilities {
        detect_with(vars, platform, Overrides::default())
    }

    fn detect_with(
        vars: &[(&str, &str)],
        platform: Platform,
        overrides: Overrides,
    ) -> Capabilities {
        let vars: Vec<(String, String)> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Capabilities::detect(
            move |name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()),
            platform,
            overrides,
        )
    }

    #[test]
    fn a_plain_linux_terminal_gets_the_conservative_set() {
        let caps = detect(&[("TERM", "xterm")], Platform::Unix);
        assert!(caps.sync);
        assert!(!caps.keyboard);
        assert_eq!(caps.color, ColorDepth::Ansi16);
        assert!(!caps.hyperlinks);
        assert!(!caps.unicode);
        assert!(!caps.dumb);
    }

    #[test]
    fn the_environment_upgrades_it() {
        let caps = detect(
            &[
                ("TERM", "xterm-256color"),
                ("COLORTERM", "truecolor"),
                ("LANG", "en_GB.UTF-8"),
                ("VTE_VERSION", "7600"),
            ],
            Platform::Unix,
        );
        assert_eq!(caps.color, ColorDepth::TrueColor);
        assert!(caps.unicode && caps.hyperlinks);
        assert_eq!(
            detect(&[("TERM", "screen-256color")], Platform::Unix).color,
            ColorDepth::Indexed256
        );
        // Inside tmux a link may not reach the outer terminal.
        assert!(
            !detect(
                &[("TERM_PROGRAM", "WezTerm"), ("TMUX", "x")],
                Platform::Unix
            )
            .hyperlinks
        );
        assert_eq!(
            detect(
                &[("TERM", "xterm-256color"), ("NO_COLOR", "1")],
                Platform::Unix
            )
            .color,
            ColorDepth::Mono
        );
        let dumb = detect(&[("TERM", "dumb")], Platform::Unix);
        assert!(dumb.dumb && !dumb.sync);
        assert!(!detect(&[("TERM", "linux")], Platform::Unix).sync);
    }

    #[test]
    fn windows_terminal_and_the_old_console_host() {
        let wt = detect(&[("WT_SESSION", "x")], Platform::Windows);
        assert!(wt.sync && wt.unicode && wt.hyperlinks);
        assert_eq!(wt.color, ColorDepth::TrueColor);
        let conhost = detect(&[], Platform::Windows);
        assert!(!conhost.sync && !conhost.unicode && !conhost.hyperlinks);
        assert_eq!(conhost.color, ColorDepth::Ansi16);
    }

    #[test]
    fn replies_upgrade_and_overrides_win() {
        let mut caps = detect(&[("TERM", "xterm")], Platform::Unix);
        assert!(caps.apply(&Reply::Keyboard(0)));
        assert!(caps.keyboard);
        assert!(caps.apply(&Reply::Background(1, 2, 3)));
        assert_eq!(caps.background, Some((1, 2, 3)));
        assert!(caps.apply(&Reply::Mode {
            mode: 2026,
            state: 4
        }));
        assert!(!caps.sync);
        assert!(!caps.apply(&Reply::DeviceAttributes));
        assert!(!caps.win32_input);
        assert!(caps.apply(&Reply::Mode {
            mode: 9001,
            state: 2
        }));
        assert!(caps.win32_input);
        assert!(!detect(&[], Platform::Windows).win32_input);

        let overrides = Overrides {
            sync: Some(true),
            keyboard: Some(false),
            color: Some(ColorDepth::Indexed256),
            hyperlinks: Some(true),
            unicode: Some(true),
        };
        let mut caps = detect_with(&[("TERM", "dumb")], Platform::Unix, overrides);
        assert!(caps.sync && caps.hyperlinks && caps.unicode);
        assert_eq!(caps.color, ColorDepth::Indexed256);
        caps.apply(&Reply::Keyboard(1));
        caps.apply(&Reply::Mode {
            mode: 2026,
            state: 4,
        });
        caps.apply(&Reply::Mode {
            mode: 9001,
            state: 1,
        });
        assert!(!caps.keyboard && !caps.win32_input && caps.sync);
        assert_eq!(caps.overrides(), overrides);
    }
}
