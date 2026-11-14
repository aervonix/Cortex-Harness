//! View functions: state, a width and a [`Look`] in, styled lines out
//! (`docs/architecture/09-tui.md`, "Structure of `cortex-tui`").
//!
//! A view is pure. It reads its state and draws it, and keeps nothing: the
//! same state, width and look give the same lines, which is what the
//! snapshot tests hold it to and what lets the full-screen presenter draw the
//! same views as the inline one.
//!
//! **The width a view is given is the columns a line may use,** which is one
//! less than the window's (`cortex_term::Inline::width`): no row reaches the
//! terminal's last column. A view never returns a line wider than that.
//!
//! **Content passes through `cortex_term::sanitize`** before it is laid out:
//! text from a model, a tool or a file never reaches a line with a control
//! character in it.

use crate::glyphs::Glyphs;
use crate::theme::Theme;

// Step 21's views, the input box and the menus, are `view::input` and
// `view::menu`.

/// How a view draws, beside its state: the theme, the glyph set and the
/// `tui` settings that shape the transcript. Step 23 fills it from the
/// configuration; until then, [`Look::new`] gives the defaults.
#[derive(Debug, Clone)]
pub struct Look {
    /// The colours, for the terminal being drawn to.
    pub theme: Theme,
    /// The glyph set.
    pub glyphs: &'static Glyphs,
    /// Blank lines between blocks, or only between turns (`tui.density`).
    pub density: Density,
    /// The widest prose may be, in columns, its two-column indent included
    /// (`tui.prose_width`, 80).
    pub prose_width: usize,
    /// The most diff lines a box opens on by itself, and the most rows of
    /// detail an open box in the live region shows (`tui.tool_detail`, 12).
    pub tool_detail: usize,
    /// Which tool detail starts open (`tui.tool_output`).
    pub tool_output: ToolOutput,
    /// What finished reasoning leaves in the transcript (`tui.reasoning`).
    pub reasoning: Reasoning,
    /// A dim summary line after each turn (`tui.turn_summary`, off).
    pub turn_summary: bool,
    /// A still mark in place of the spinner (`tui.reduced_motion`, off).
    pub reduced_motion: bool,
}

impl Look {
    /// The defaults of every `tui` setting, with `theme` and `glyphs`.
    #[must_use]
    pub fn new(theme: Theme, glyphs: &'static Glyphs) -> Self {
        Self {
            theme,
            glyphs,
            density: Density::Comfortable,
            prose_width: 80,
            tool_detail: 12,
            tool_output: ToolOutput::Auto,
            reasoning: Reasoning::Summary,
            turn_summary: false,
            reduced_motion: false,
        }
    }

    /// The spinner's frame for a tick, or its still mark under reduced
    /// motion.
    #[must_use]
    pub fn spinner(&self, tick: usize) -> &'static str {
        if self.reduced_motion {
            self.glyphs.still_spinner()
        } else {
            self.glyphs.spinner_frame(tick)
        }
    }
}

/// `tui.density`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Density {
    /// A blank line between blocks (the default).
    Comfortable,
    /// A blank line only between turns, and reasoning as its first line
    /// alone.
    Compact,
}

/// `tui.tool_output`: which tool detail starts open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolOutput {
    /// A failure's output and a diff of up to `tui.tool_detail` lines start
    /// open; the rest starts closed (the default).
    Auto,
    /// Every detail starts open.
    Always,
    /// Every box stays closed.
    Never,
}

/// `tui.reasoning`: what finished reasoning leaves in the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reasoning {
    /// `∴ Reasoned for 6s` and the provider's summary (the default).
    Summary,
    /// `∴ Reasoned for 6s` alone; the summary is in the viewer.
    Duration,
}
