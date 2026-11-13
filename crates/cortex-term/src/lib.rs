//! The terminal layer: raw mode and putting the terminal back, input
//! decoding, capability detection, the styled-line model, the line renderer,
//! the inline presenter, the frame scheduler and sanitising
//! (`docs/architecture/09-tui.md`, "Rendering approach" and "Terminal layer").
//! It knows nothing about agents.
//!
//! | Module | What it gives |
//! |---|---|
//! | [`style`] | [`Color`], [`Style`], and colours brought down to what a terminal shows. |
//! | [`line`] | [`Line`] and [`Span`]: what every component renders to. |
//! | [`sanitize`] | [`sanitize()`]: content made safe to show. No control character from content reaches the terminal. |
//! | [`input`] | [`Decoder`]: bytes to [`Event`]s, however a read splits them. |
//! | [`caps`] | [`Capabilities`]: what the terminal can do, detected, asked and overridden. |
//! | [`inline`] | [`Inline`]: the inline presenter. A frame is one write, inside a synchronized update. |
//! | [`schedule`] | [`Scheduler`]: when to draw. |
//! | [`terminal`] | [`Terminal`]: raw mode on the way in, put back on every way out, events on the way through. |
//! | `pty` | Pseudo-terminals, for tests. Behind the `pty` feature, which only the test kit turns on. |
//!
//! # No look
//!
//! Nothing here names a colour, a glyph or a layout. Those are the
//! interface's, in `cortex-tui`, and the theme's.
//!
//! # `unsafe`
//!
//! Only in `sys`, one module for each system, each block with its reason.

pub mod caps;
pub mod inline;
pub mod input;
pub mod line;
#[cfg(feature = "pty")]
pub mod pty;
mod render;
pub mod sanitize;
pub mod schedule;
pub mod style;
mod sys;
pub mod terminal;

pub use caps::{Capabilities, Overrides, Platform};
pub use inline::{Cursor, Drawn, Frame, Inline, Sink};
pub use input::{Decoder, Event, Key, KeyCode, Mods, Reply};
pub use line::{Line, Span};
pub use sanitize::sanitize;
pub use schedule::{Scheduler, Wake};
pub use style::{Color, ColorDepth, Style};
pub use sys::Ready;
pub use terminal::{Device, System, Terminal, Waker, is_terminal};
