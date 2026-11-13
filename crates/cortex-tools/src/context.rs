//! What a tool is given to plan and to run with.
//!
//! [`PlanCx`] is small on purpose: planning has no side effects, so all it
//! can reach is the path resolver. [`RunCx`] is what one call runs with: the
//! cancellation token, the resolver, the session's content store and table
//! of seen files, the output limits, and the few things a session may or
//! may not have (an outline source, a model that takes images).
//!
//! Everything in a [`RunCx`] is shared by handle, so that a tool can move
//! what it needs onto the blocking pool.

use std::sync::Arc;

use cortex_base::{AbsPath, CancelToken, PathResolver};

use crate::files::FileTable;
use crate::output::OutputLimits;
use crate::store::ContentStore;

/// What [`Tool::plan`](crate::Tool::plan) may use.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PlanCx {
    /// The session's path resolver. Every path in a tool's arguments goes
    /// through it, and a path it refuses is refused by the tool with the
    /// resolver's reason.
    pub paths: Arc<PathResolver>,
}

impl PlanCx {
    /// A planning context for the workspace `paths` resolves in.
    #[must_use]
    pub fn new(paths: Arc<PathResolver>) -> Self {
        Self { paths }
    }
}

/// Decides, entry by entry, what a directory walk may read.
///
/// A plan that walks a tree declares one effect for the whole tree
/// ([`Effect::ReadTree`](crate::Effect::ReadTree)), and the permission
/// engine decides it before the walk starts. But a rule can protect one
/// file inside an allowed tree, and nobody can list the tree in advance. So
/// the walk asks this gate about each entry as it meets it, and steps past
/// what it may not use, saying so in its result.
///
/// The permission engine supplies the gate. [`OpenGate`] lets everything
/// through.
pub trait ReadGate: Send + Sync {
    /// Whether the file or directory at `path`, a real location inside a
    /// tree whose walk was allowed, may be read.
    fn may_read(&self, path: &AbsPath) -> bool;
}

/// A [`ReadGate`] that lets every entry through.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenGate;

impl ReadGate for OpenGate {
    fn may_read(&self, _path: &AbsPath) -> bool {
        true
    }
}

/// Where the outline of a file comes from: its definitions with their line
/// numbers (`docs/architecture/15-efficiency.md`, "Repository map").
///
/// `read` puts the outline at the start of its result for a file that is
/// too large to show whole. The repository index implements this. It
/// blocks, and is called on the blocking pool.
pub trait Outliner: Send + Sync {
    /// The outline of the file at `path`, one definition per line, each
    /// with its line number. `None` when there is none: an unknown
    /// language, a file not indexed yet.
    fn outline(&self, path: &AbsPath) -> Option<String>;
}

/// What images the session's model accepts in a tool result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageSupport {
    /// The media types it takes, for example `image/png`.
    pub media_types: Vec<String>,
    /// The largest image, in bytes, it takes.
    pub max_bytes: u64,
}

impl ImageSupport {
    /// Whether an image of this type and size can be sent.
    #[must_use]
    pub fn accepts(&self, media_type: &str, bytes: u64) -> bool {
        bytes <= self.max_bytes && self.media_types.iter().any(|known| known == media_type)
    }
}

/// What one tool call runs with.
#[derive(Clone)]
#[non_exhaustive]
pub struct RunCx {
    /// Fires when the call must stop. A child of the turn's token.
    pub cancel: CancelToken,
    /// Fires when the call must stop **now**: a second interrupt. A tool
    /// that stops in stages (the shell tool, which asks a command politely
    /// and waits before it kills) skips what is left of them. A tool that
    /// stops at once on `cancel` need not look at it. It is fired together
    /// with `cancel` or after it, never in its place, so watching `cancel`
    /// alone still stops a call.
    pub kill: CancelToken,
    /// The session's path resolver.
    pub paths: Arc<PathResolver>,
    /// Where artifacts and file versions are kept.
    pub store: Arc<dyn ContentStore>,
    /// The session's table of seen files.
    pub files: Arc<FileTable>,
    /// The limits on what a result may hold.
    pub limits: OutputLimits,
    /// What a directory walk may read.
    pub gate: Arc<dyn ReadGate>,
    /// Where outlines come from, once the repository index exists.
    pub outliner: Option<Arc<dyn Outliner>>,
    /// The images the model accepts. `None` for a model that takes none.
    pub images: Option<ImageSupport>,
    /// The number of the turn the call belongs to, counted from 1, so that
    /// a result can say when something was last seen. 0 when unknown.
    pub turn: u32,
    /// The number of the tool round the call belongs to: the calls of one
    /// response of the model. 0 when the caller does not say.
    ///
    /// Two calls with the same number were written by the model in one
    /// response, so neither was written with the other's result in view. A
    /// call with a higher number was written after the results of every
    /// round with a lower one had been sent to the model: the loop moves
    /// the number on only when a round's results are recorded. A request
    /// that is sent again keeps its number. `plan` uses it to tell whether
    /// the model has been shown what an earlier call was told
    /// (`docs/architecture/05-tools.md`, "`plan`").
    pub round: u64,
}

impl RunCx {
    /// A context with the default limits, an open gate, no outline source,
    /// no image support, an unknown turn and round, and tokens of its own.
    #[must_use]
    pub fn new(
        paths: Arc<PathResolver>,
        store: Arc<dyn ContentStore>,
        files: Arc<FileTable>,
    ) -> Self {
        Self {
            cancel: CancelToken::new(),
            kill: CancelToken::new(),
            paths,
            store,
            files,
            limits: OutputLimits::default(),
            gate: Arc::new(OpenGate),
            outliner: None,
            images: None,
            turn: 0,
            round: 0,
        }
    }

    /// Sets the cancellation token.
    #[must_use]
    pub fn with_cancel(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// Sets the token of a second interrupt, "kill now".
    #[must_use]
    pub fn with_kill(mut self, kill: CancelToken) -> Self {
        self.kill = kill;
        self
    }

    /// Sets the output limits.
    #[must_use]
    pub fn with_limits(mut self, limits: OutputLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets the gate a directory walk asks.
    #[must_use]
    pub fn with_gate(mut self, gate: Arc<dyn ReadGate>) -> Self {
        self.gate = gate;
        self
    }

    /// Sets where outlines come from.
    #[must_use]
    pub fn with_outliner(mut self, outliner: Arc<dyn Outliner>) -> Self {
        self.outliner = Some(outliner);
        self
    }

    /// Sets which images the model accepts.
    #[must_use]
    pub fn with_images(mut self, images: ImageSupport) -> Self {
        self.images = Some(images);
        self
    }

    /// Sets the turn number.
    #[must_use]
    pub fn with_turn(mut self, turn: u32) -> Self {
        self.turn = turn;
        self
    }

    /// Sets the number of the tool round.
    #[must_use]
    pub fn with_round(mut self, round: u64) -> Self {
        self.round = round;
        self
    }

    /// The planning context for the same workspace.
    #[must_use]
    pub fn plan_cx(&self) -> PlanCx {
        PlanCx::new(Arc::clone(&self.paths))
    }
}

impl std::fmt::Debug for RunCx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunCx")
            .field("root", self.paths.root())
            .field("limits", &self.limits)
            .field("turn", &self.turn)
            .field("round", &self.round)
            .field("cancelled", &self.cancel.is_cancelled())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_support_checks_the_type_and_the_size() {
        let images = ImageSupport {
            media_types: vec!["image/png".to_owned(), "image/jpeg".to_owned()],
            max_bytes: 1000,
        };
        assert!(images.accepts("image/png", 1000));
        assert!(!images.accepts("image/png", 1001));
        assert!(!images.accepts("image/tiff", 10));
    }
}
