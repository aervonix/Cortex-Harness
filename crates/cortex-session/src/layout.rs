//! Where a session's files are (`docs/architecture/07-sessions-and-context.md`,
//! "On-disk layout").
//!
//! ```text
//! <data dir>/sessions/                 SessionsRoot
//!     <project key>/                   ProjectSessions
//!         index.json                   the listing cache (SessionIndex)
//!         index.lock
//!         <session id>/                SessionDir
//!             log.jsonl                the session: append-only, the source of truth
//!             lock                     held by the one process that writes the log
//!             log.cut-<offset>         bytes of a line a crash cut off, kept aside
//!             artifacts/<id>.<ext>     full tool outputs and other large content
//!             checkpoints/<hash>       file contents, filed under their hash
//! ```
//!
//! The data directory comes from configuration, so every type here takes
//! its path as an input. Nothing here touches the file system.

use std::path::{Path, PathBuf};

use cortex_protocol::{ProjectKey, SessionId};

use crate::index::SessionIndex;

/// The directory that holds every project's sessions: `<data dir>/sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionsRoot {
    path: PathBuf,
}

impl SessionsRoot {
    /// The sessions directory at `path`, usually `<data dir>/sessions`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The sessions directory of the data directory at `data_dir`.
    #[must_use]
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self::new(data_dir.join("sessions"))
    }

    /// The directory itself.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The sessions of one project. `None` when the key could not be one
    /// directory name: a key is never a path.
    #[must_use]
    pub fn project(&self, key: &ProjectKey) -> Option<ProjectSessions> {
        is_plain_name(key.as_str()).then(|| ProjectSessions {
            path: self.path.join(key.as_str()),
        })
    }
}

/// The sessions of one project: `<sessions>/<project key>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSessions {
    path: PathBuf,
}

impl ProjectSessions {
    /// The project's directory, for a caller that already has one.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The directory itself.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The directory of one session. `None` when the id could not be one
    /// directory name.
    #[must_use]
    pub fn session(&self, id: &SessionId) -> Option<SessionDir> {
        is_plain_name(id.as_str()).then(|| SessionDir {
            path: self.path.join(id.as_str()),
            id: id.clone(),
        })
    }

    /// The project's session index.
    #[must_use]
    pub fn index(&self) -> SessionIndex {
        SessionIndex::new(self.clone())
    }

    /// `index.json`.
    pub(crate) fn index_file(&self) -> PathBuf {
        self.path.join("index.json")
    }

    /// `index.lock`.
    pub(crate) fn index_lock(&self) -> PathBuf {
        self.path.join("index.lock")
    }
}

/// The directory of one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDir {
    path: PathBuf,
    id: SessionId,
}

impl SessionDir {
    /// The directory itself.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The session it holds.
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.id
    }

    /// `log.jsonl`: the session, one entry a line.
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.path.join("log.jsonl")
    }

    /// `lock`: held by the process that writes the log.
    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.path.join("lock")
    }

    /// `log.cut-<offset>`: the bytes of a last line that a crash cut off.
    #[must_use]
    pub fn cut(&self, offset: u64) -> PathBuf {
        self.path.join(format!("log.cut-{offset}"))
    }

    /// `artifacts/`.
    #[must_use]
    pub fn artifacts(&self) -> PathBuf {
        self.path.join("artifacts")
    }

    /// `checkpoints/`.
    #[must_use]
    pub fn checkpoints(&self) -> PathBuf {
        self.path.join("checkpoints")
    }

    /// The project directory this session is in.
    #[must_use]
    pub fn project(&self) -> ProjectSessions {
        ProjectSessions {
            path: self
                .path
                .parent()
                .map_or_else(PathBuf::new, Path::to_path_buf),
        }
    }
}

/// Whether `name` can be a directory name on every OS, and nothing more:
/// ASCII letters, digits, `-` and `_`, at most 128 of them.
pub(crate) fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_layout_matches_the_page() {
        let root = SessionsRoot::in_data_dir(Path::new("/data"));
        let project = root.project(&ProjectKey::new("0123abcd")).unwrap();
        let session = project.session(&SessionId::new("s_01")).unwrap();
        assert_eq!(
            session.log(),
            Path::new("/data/sessions/0123abcd/s_01/log.jsonl")
        );
        assert_eq!(
            session.artifacts(),
            Path::new("/data/sessions/0123abcd/s_01/artifacts")
        );
        assert_eq!(
            session.checkpoints(),
            Path::new("/data/sessions/0123abcd/s_01/checkpoints")
        );
        assert_eq!(
            project.index_file(),
            Path::new("/data/sessions/0123abcd/index.json")
        );
        assert_eq!(session.project(), project);
    }

    #[test]
    fn a_key_or_an_id_that_could_be_a_path_is_refused() {
        let root = SessionsRoot::new("/data/sessions");
        for bad in ["", "..", "a/b", "a\\b", "C:", "a.b", &"x".repeat(129)] {
            assert!(root.project(&ProjectKey::new(bad)).is_none(), "{bad:?}");
        }
        let project = root.project(&ProjectKey::new("k")).unwrap();
        assert!(project.session(&SessionId::new("../s")).is_none());
        assert!(project.session(&SessionId::new("s_1")).is_some());
    }
}
