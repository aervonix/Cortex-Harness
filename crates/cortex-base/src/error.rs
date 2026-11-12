//! The kind every error has (`docs/architecture/02-runtime.md`, "Error
//! handling").
//!
//! Each library crate defines its own error enum. What they share is a
//! [`ErrorKind`]: a closed set that decides whether the harness retries, which
//! exit code a headless run ends with, and which `kind` a client sees. A
//! crate's error says which kind it is by implementing [`Classified`].
//!
//! The protocol has an enumeration of the same name
//! ([`cortex_protocol::ErrorKind`]). That one is *open*: it keeps a kind a
//! newer build sent. This one is *closed*: code in the harness matches on it
//! exhaustively, so adding a kind is a compile error everywhere a decision
//! depends on it.

use cortex_protocol::ErrorData;
use cortex_protocol::ErrorKind as WireKind;

/// What sort of failure an error is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// Bad flags, arguments or request parameters.
    Usage,
    /// Invalid configuration.
    Config,
    /// Missing or rejected credentials.
    Auth,
    /// The provider refused the request for a reason a retry will not change:
    /// an invalid request, a missing model, billing.
    Provider,
    /// A network failure, a timeout, an overload or a retryable rate limit.
    Transient,
    /// The model stopped in a way that is not success: a refusal, a content
    /// filter, the output limit.
    ModelStop,
    /// A required approval was denied or could not be asked.
    Permission,
    /// A configured budget was reached: requests, cost, tokens or time.
    Limit,
    /// The context cannot be made to fit.
    ContextOverflow,
    /// A tool failed. Reported to the model, which may recover.
    Tool,
    /// Cancelled by the user or by a signal.
    Interrupted,
    /// A bug in the harness.
    Internal,
}

/// Whether the harness tries again by itself after an error of some kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Retry {
    /// Never. The error is reported.
    Never,
    /// Yes, with backoff, until the retries are used up.
    Automatic,
    /// Once, after the context has been compacted.
    AfterCompaction,
}

impl ErrorKind {
    /// Every kind, in the order of the table in `02-runtime.md`.
    pub const ALL: [Self; 12] = [
        Self::Usage,
        Self::Config,
        Self::Auth,
        Self::Provider,
        Self::Transient,
        Self::ModelStop,
        Self::Permission,
        Self::Limit,
        Self::ContextOverflow,
        Self::Tool,
        Self::Interrupted,
        Self::Internal,
    ];

    /// The kind as the protocol writes it, in `snake_case`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Usage => "usage",
            Self::Config => "config",
            Self::Auth => "auth",
            Self::Provider => "provider",
            Self::Transient => "transient",
            Self::ModelStop => "model_stop",
            Self::Permission => "permission",
            Self::Limit => "limit",
            Self::ContextOverflow => "context_overflow",
            Self::Tool => "tool",
            Self::Interrupted => "interrupted",
            Self::Internal => "internal",
        }
    }

    /// Reads a kind from its protocol name. `None` for a name this build does
    /// not know.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == name)
    }

    /// Whether the harness retries after an error of this kind.
    #[must_use]
    pub const fn retry(self) -> Retry {
        match self {
            Self::Transient => Retry::Automatic,
            Self::ContextOverflow => Retry::AfterCompaction,
            Self::Usage
            | Self::Config
            | Self::Auth
            | Self::Provider
            | Self::ModelStop
            | Self::Permission
            | Self::Limit
            | Self::Tool
            | Self::Interrupted
            | Self::Internal => Retry::Never,
        }
    }

    /// The exit code of a headless run that ends with an error of this kind
    /// (`docs/architecture/10-headless.md`, "Exit codes").
    ///
    /// `None` for [`ErrorKind::Tool`]: a tool failure is reported to the model
    /// and never ends a run by itself.
    #[must_use]
    pub const fn exit_code(self) -> Option<u8> {
        match self {
            Self::Internal => Some(1),
            Self::Usage | Self::Config => Some(2),
            Self::Auth => Some(3),
            Self::Provider | Self::Transient => Some(4),
            Self::ModelStop => Some(5),
            Self::Permission => Some(6),
            Self::Limit => Some(7),
            Self::ContextOverflow => Some(8),
            Self::Interrupted => Some(130),
            Self::Tool => None,
        }
    }

    /// The protocol's form of this kind.
    #[must_use]
    pub fn to_wire(self) -> WireKind {
        match self {
            Self::Usage => WireKind::Usage,
            Self::Config => WireKind::Config,
            Self::Auth => WireKind::Auth,
            Self::Provider => WireKind::Provider,
            Self::Transient => WireKind::Transient,
            Self::ModelStop => WireKind::ModelStop,
            Self::Permission => WireKind::Permission,
            Self::Limit => WireKind::Limit,
            Self::ContextOverflow => WireKind::ContextOverflow,
            Self::Tool => WireKind::Tool,
            Self::Interrupted => WireKind::Interrupted,
            Self::Internal => WireKind::Internal,
        }
    }

    /// Reads the protocol's form. `None` for a kind this build does not know,
    /// which a caller treats as it treats any failure it cannot classify.
    #[must_use]
    pub fn from_wire(kind: &WireKind) -> Option<Self> {
        Self::from_name(kind.as_str())
    }
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<ErrorKind> for WireKind {
    fn from(kind: ErrorKind) -> Self {
        kind.to_wire()
    }
}

/// An error that knows its [`ErrorKind`].
///
/// Every error enum in a library crate implements this, so that the code that
/// reports an error (the loop, the RPC server, the headless printer) never
/// has to guess from a message what sort of failure it has.
pub trait Classified: std::error::Error {
    /// What sort of failure this is.
    fn kind(&self) -> ErrorKind;

    /// A stable, machine-readable name for the exact failure, in
    /// `snake_case`. Clients may branch on it. The default is the kind's own
    /// name, for an error that has nothing more exact to say.
    fn code(&self) -> &str {
        self.kind().as_str()
    }

    /// The error as every front end sees it: its kind, its code, and its
    /// `Display` text as the message.
    fn to_error_data(&self) -> ErrorData {
        ErrorData::new(self.kind().to_wire(), self.code(), self.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_has_the_protocols_name_and_reads_back() {
        let names: Vec<&str> = ErrorKind::ALL.iter().map(|kind| kind.as_str()).collect();
        assert_eq!(names, WireKind::KNOWN, "same names, same order");
        for kind in ErrorKind::ALL {
            let wire = kind.to_wire();
            assert!(wire.is_known(), "{kind} is listed by the protocol");
            assert_eq!(wire.as_str(), kind.as_str());
            assert_eq!(ErrorKind::from_wire(&wire), Some(kind));
            assert_eq!(ErrorKind::from_name(kind.as_str()), Some(kind));
            assert_eq!(kind.to_string(), kind.as_str());
        }
    }

    #[test]
    fn a_kind_this_build_does_not_know_is_not_guessed() {
        let newer = WireKind::from_wire("quota_exhausted");
        assert_eq!(ErrorKind::from_wire(&newer), None);
        assert_eq!(ErrorKind::from_name("Usage"), None, "names are exact");
    }

    #[test]
    fn exit_codes_follow_the_headless_table() {
        let expected = [
            (ErrorKind::Usage, Some(2)),
            (ErrorKind::Config, Some(2)),
            (ErrorKind::Auth, Some(3)),
            (ErrorKind::Provider, Some(4)),
            (ErrorKind::Transient, Some(4)),
            (ErrorKind::ModelStop, Some(5)),
            (ErrorKind::Permission, Some(6)),
            (ErrorKind::Limit, Some(7)),
            (ErrorKind::ContextOverflow, Some(8)),
            (ErrorKind::Tool, None),
            (ErrorKind::Interrupted, Some(130)),
            (ErrorKind::Internal, Some(1)),
        ];
        assert_eq!(expected.len(), ErrorKind::ALL.len());
        for (kind, code) in expected {
            assert_eq!(kind.exit_code(), code, "{kind}");
        }
        for kind in ErrorKind::ALL {
            assert_ne!(kind.exit_code(), Some(0), "{kind}: zero means success");
        }
    }

    #[test]
    fn only_transient_and_overflow_are_retried() {
        for kind in ErrorKind::ALL {
            let expected = match kind {
                ErrorKind::Transient => Retry::Automatic,
                ErrorKind::ContextOverflow => Retry::AfterCompaction,
                _ => Retry::Never,
            };
            assert_eq!(kind.retry(), expected, "{kind}");
        }
    }

    #[derive(Debug, thiserror::Error)]
    enum Example {
        #[error("the file `{0}` is not valid UTF-8")]
        NotText(String),
        #[error("stopped")]
        Stopped,
    }

    impl Classified for Example {
        fn kind(&self) -> ErrorKind {
            match self {
                Self::NotText(_) => ErrorKind::Tool,
                Self::Stopped => ErrorKind::Interrupted,
            }
        }

        fn code(&self) -> &str {
            match self {
                Self::NotText(_) => "not_text",
                Self::Stopped => self.kind().as_str(),
            }
        }
    }

    #[test]
    fn a_classified_error_becomes_the_protocols_error_object() {
        let data = Example::NotText("a.bin".to_owned()).to_error_data();
        assert_eq!(data.kind, WireKind::Tool);
        assert_eq!(data.code, "not_text");
        assert_eq!(data.message, "the file `a.bin` is not valid UTF-8");
        assert_eq!(data.request_id, None);

        let data = Example::Stopped.to_error_data();
        assert_eq!(data.kind, WireKind::Interrupted);
        assert_eq!(data.code, "interrupted", "the default code is the kind");
    }
}
