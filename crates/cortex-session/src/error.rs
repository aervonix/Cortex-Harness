//! What can go wrong in the session store.

use std::io;
use std::path::PathBuf;

use cortex_base::{Classified, ErrorKind};

/// Why the session store could not do what was asked.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Another process has the session open for writing.
    #[error("the session `{session}` is open for writing in another process")]
    Busy {
        /// The session.
        session: String,
    },
    /// The session was written by a newer version of Cortex-Harness, or
    /// was opened read-only, so it cannot be written to.
    #[error("the session `{session}` is read-only: {reason}")]
    ReadOnly {
        /// The session.
        session: String,
        /// Why.
        reason: String,
    },
    /// There is no such session.
    #[error("there is no session `{session}`")]
    NotFound {
        /// The session, or the prefix that was asked for.
        session: String,
    },
    /// A prefix names more than one session.
    #[error("`{prefix}` names more than one session: {}", candidates.join(", "))]
    AmbiguousPrefix {
        /// What was asked for.
        prefix: String,
        /// The sessions it could mean.
        candidates: Vec<String>,
    },
    /// There is no entry with that id in the session.
    #[error("there is no entry `{entry}` in this session")]
    NoSuchEntry {
        /// The entry.
        entry: String,
    },
    /// The entry exists but cannot be used for what was asked.
    #[error("{0}")]
    Invalid(String),
    /// A write failed and the log could not be put back as it was, so the
    /// session refuses further writes until it is opened again.
    #[error(
        "a write to the session log failed and the log could not be restored; open the \
         session again to repair it"
    )]
    Broken,
    /// The file is not a session log this build can read.
    #[error("`{path}` is not a session log: {reason}")]
    NotALog {
        /// The file.
        path: PathBuf,
        /// What was wrong with it.
        reason: String,
    },
    /// Only a regular file can be kept: this is a link, a directory, a
    /// device or a FIFO, which are never followed or read.
    #[error("`{}` is not a regular file, so its content is not kept", path.display())]
    NotARegularFile {
        /// The path.
        path: PathBuf,
    },
    /// An entry is longer, as one line of the log, than a log line may be.
    /// Large content belongs in an artifact.
    #[error(
        "the entry would be a line of {bytes} bytes, over the limit of {limit}; large content \
         belongs in an artifact"
    )]
    EntryTooLarge {
        /// How long its line would be, without the newline.
        bytes: u64,
        /// The longest line a log may have.
        limit: u64,
    },
    /// Stored content does not match the hash it is filed under.
    #[error("the stored content `{hash}` is damaged: its content no longer matches its hash")]
    Corrupt {
        /// The hash it is filed under.
        hash: String,
    },
    /// The file system refused.
    #[error("could not {action} `{}`: {source}", path.display())]
    Io {
        /// What was being done, as a phrase: "write the session log".
        action: &'static str,
        /// The file or directory.
        path: PathBuf,
        /// What the operating system reported.
        #[source]
        source: io::Error,
    },
    /// An entry could not be written as JSON.
    #[error("could not encode an entry: {0}")]
    Encode(#[from] serde_json::Error),
}

impl SessionError {
    /// A closure that wraps an I/O error with what was being done to which
    /// path, for `map_err`.
    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
    ) -> impl FnOnce(io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io {
            action,
            path,
            source,
        }
    }
}

impl Classified for SessionError {
    fn kind(&self) -> ErrorKind {
        match self {
            // Each is about what was asked for, and asking differently (another
            // session, a longer prefix, read-only) is the way out.
            Self::Busy { .. }
            | Self::ReadOnly { .. }
            | Self::NotFound { .. }
            | Self::NoSuchEntry { .. }
            | Self::AmbiguousPrefix { .. }
            | Self::Invalid(_)
            | Self::NotARegularFile { .. } => ErrorKind::Usage,
            Self::Broken
            | Self::EntryTooLarge { .. }
            | Self::NotALog { .. }
            | Self::Corrupt { .. }
            | Self::Io { .. }
            | Self::Encode(_) => ErrorKind::Internal,
        }
    }

    fn code(&self) -> &str {
        match self {
            Self::Busy { .. } => "session_busy",
            Self::ReadOnly { .. } => "session_read_only",
            Self::NotFound { .. } => "unknown_session",
            Self::AmbiguousPrefix { .. } => "ambiguous_session",
            Self::NoSuchEntry { .. } => "no_such_entry",
            Self::Invalid(_) => "invalid_entry",
            Self::Broken => "session_log_broken",
            Self::NotALog { .. } => "not_a_session_log",
            Self::Corrupt { .. } => "stored_content_damaged",
            Self::NotARegularFile { .. } => "not_a_regular_file",
            Self::EntryTooLarge { .. } => "session_entry_too_large",
            Self::Io { .. } => "session_io",
            Self::Encode(_) => "session_encode",
        }
    }
}

/// What the session store returns.
pub type Result<T, E = SessionError> = std::result::Result<T, E>;
