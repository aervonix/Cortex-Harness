//! The content store: where tools put what does not fit in a result, and
//! what they need to keep.
//!
//! Two things go through it.
//!
//! - **Artifacts.** The full output of a call whose result was cut
//!   (`docs/architecture/05-tools.md`, "One output policy"), and images. An
//!   artifact has an id, and `read` and `search` take that id in their
//!   `artifact` argument.
//! - **File versions.** The content of a file as the model last saw it, kept
//!   under its [`ContentHash`], so that a later read can send only what
//!   changed (`docs/architecture/15-efficiency.md`, "Changed-only re-reads").
//!
//! The session owns the real store (`cortex-session`: `artifacts/` and
//! `checkpoints/` in the session's directory). This crate sees it only as
//! the [`ContentStore`] trait, and has [`MemoryStore`] for a session with no
//! disk.
//!
//! Every method blocks, and is called on the blocking pool.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cortex_base::{Classified, ContentHash, ErrorKind, IdCounter};
use cortex_protocol::ids::ArtifactId;
use cortex_protocol::item::ArtifactRef;

/// Why the content store could not do what was asked.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// There is no artifact with that id in this session.
    #[error("there is no artifact `{0}` in this session")]
    NoSuchArtifact(String),
    /// The store could not read or write.
    #[error("the content store failed: {0}")]
    Io(#[from] io::Error),
}

impl Classified for StoreError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::NoSuchArtifact(_) => ErrorKind::Tool,
            Self::Io(_) => ErrorKind::Internal,
        }
    }

    fn code(&self) -> &str {
        match self {
            Self::NoSuchArtifact(_) => "no_such_artifact",
            Self::Io(_) => "store_failed",
        }
    }
}

/// Where a session keeps artifacts and file versions.
pub trait ContentStore: Send + Sync {
    /// Starts an artifact. `label` says what it holds, in a few words, and
    /// `media_type` what kind of content it is (`text/plain`, `image/png`).
    /// The content is written to what this returns, in pieces, so output
    /// that is still arriving never has to be held in memory.
    fn begin_artifact(
        &self,
        label: &str,
        media_type: &str,
    ) -> Result<Box<dyn ArtifactWriter>, StoreError>;

    /// Opens an artifact for reading from its start.
    fn open_artifact(&self, id: &ArtifactId) -> Result<Box<dyn Read + Send>, StoreError>;

    /// Keeps one version of a file's content, and returns the hash it is
    /// kept under. Keeping the same content twice keeps it once.
    fn put_version(&self, content: &[u8]) -> Result<ContentHash, StoreError>;

    /// The content kept under a hash, or `None` if it was never kept or has
    /// been cleared away.
    fn get_version(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>, StoreError>;

    /// Stores an artifact whose whole content is at hand.
    fn put_artifact(
        &self,
        label: &str,
        media_type: &str,
        content: &[u8],
    ) -> Result<ArtifactRef, StoreError> {
        let mut writer = self.begin_artifact(label, media_type)?;
        writer.write_all(content)?;
        writer.finish()
    }
}

/// An artifact being written. Dropping it without [`finish`](Self::finish)
/// abandons the artifact.
pub trait ArtifactWriter: Write + Send {
    /// Completes the artifact and returns the reference a result carries.
    fn finish(self: Box<Self>) -> Result<ArtifactRef, StoreError>;
}

/// A store that refuses everything, with one reason. It stands in when the
/// real store has failed in a way that cannot be asked again, so that a
/// result can still be cut and say why nothing was kept.
#[derive(Debug)]
pub(crate) struct Unavailable(pub(crate) String);

impl Unavailable {
    fn error(&self) -> StoreError {
        StoreError::Io(io::Error::other(self.0.clone()))
    }
}

impl ContentStore for Unavailable {
    fn begin_artifact(
        &self,
        _label: &str,
        _media_type: &str,
    ) -> Result<Box<dyn ArtifactWriter>, StoreError> {
        Err(self.error())
    }

    fn open_artifact(&self, _id: &ArtifactId) -> Result<Box<dyn Read + Send>, StoreError> {
        Err(self.error())
    }

    fn put_version(&self, _content: &[u8]) -> Result<ContentHash, StoreError> {
        Err(self.error())
    }

    fn get_version(&self, _hash: &ContentHash) -> Result<Option<Vec<u8>>, StoreError> {
        Err(self.error())
    }
}

/// A content store that keeps everything in memory, for a session that has
/// no directory: tests of the loop and of tools, and one-off runs that store
/// nothing.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Debug, Default)]
struct Inner {
    ids: Option<IdCounter>,
    artifacts: HashMap<ArtifactId, Stored>,
    versions: HashMap<ContentHash, Arc<[u8]>>,
}

#[derive(Debug, Clone)]
struct Stored {
    label: String,
    media_type: String,
    content: Arc<[u8]>,
}

impl MemoryStore {
    /// An empty store. Its artifact ids count up from `a_0001`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while the lock was held leaves whole entries behind, never
        // half of one: every change is a single insert.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The content of an artifact, or `None` if there is none by that id.
    #[must_use]
    pub fn artifact(&self, id: &ArtifactId) -> Option<Vec<u8>> {
        self.lock()
            .artifacts
            .get(id)
            .map(|stored| stored.content.to_vec())
    }

    /// The label and media type an artifact was stored with.
    #[must_use]
    pub fn artifact_info(&self, id: &ArtifactId) -> Option<(String, String)> {
        self.lock()
            .artifacts
            .get(id)
            .map(|stored| (stored.label.clone(), stored.media_type.clone()))
    }

    /// How many artifacts the store holds.
    #[must_use]
    pub fn artifact_count(&self) -> usize {
        self.lock().artifacts.len()
    }

    /// How many file versions the store holds.
    #[must_use]
    pub fn version_count(&self) -> usize {
        self.lock().versions.len()
    }
}

impl ContentStore for MemoryStore {
    fn begin_artifact(
        &self,
        label: &str,
        media_type: &str,
    ) -> Result<Box<dyn ArtifactWriter>, StoreError> {
        Ok(Box::new(MemoryWriter {
            store: self.clone(),
            label: label.to_owned(),
            media_type: media_type.to_owned(),
            content: Vec::new(),
        }))
    }

    fn open_artifact(&self, id: &ArtifactId) -> Result<Box<dyn Read + Send>, StoreError> {
        let content = self
            .lock()
            .artifacts
            .get(id)
            .map(|stored| Arc::clone(&stored.content))
            .ok_or_else(|| StoreError::NoSuchArtifact(id.to_string()))?;
        Ok(Box::new(io::Cursor::new(content)))
    }

    fn put_version(&self, content: &[u8]) -> Result<ContentHash, StoreError> {
        let hash = ContentHash::of(content);
        self.lock()
            .versions
            .entry(hash)
            .or_insert_with(|| Arc::from(content));
        Ok(hash)
    }

    fn get_version(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self
            .lock()
            .versions
            .get(hash)
            .map(|content| content.to_vec()))
    }
}

struct MemoryWriter {
    store: MemoryStore,
    label: String,
    media_type: String,
    content: Vec<u8>,
}

impl Write for MemoryWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.content.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl ArtifactWriter for MemoryWriter {
    fn finish(self: Box<Self>) -> Result<ArtifactRef, StoreError> {
        let bytes = self.content.len() as u64;
        let mut inner = self.store.lock();
        let id: ArtifactId = inner
            .ids
            .get_or_insert_with(|| IdCounter::new("a_").padded(4))
            .next_id();
        inner.artifacts.insert(
            id.clone(),
            Stored {
                label: self.label.clone(),
                media_type: self.media_type,
                content: Arc::from(self.content),
            },
        );
        Ok(ArtifactRef {
            id,
            label: self.label,
            bytes: Some(bytes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_artifact_written_in_pieces_reads_back_whole() {
        let store = MemoryStore::new();
        let mut writer = store
            .begin_artifact("output of a command", "text/plain")
            .expect("the store accepts an artifact");
        writer.write_all(b"first ").expect("write");
        writer.write_all(b"second").expect("write");
        let reference = writer.finish().expect("finish");
        assert_eq!(reference.id.as_str(), "a_0001");
        assert_eq!(reference.label, "output of a command");
        assert_eq!(reference.bytes, Some(12));

        let mut content = String::new();
        store
            .open_artifact(&reference.id)
            .expect("it is there")
            .read_to_string(&mut content)
            .expect("read");
        assert_eq!(content, "first second");
        assert_eq!(
            store.artifact_info(&reference.id),
            Some(("output of a command".to_owned(), "text/plain".to_owned()))
        );

        let next = store.put_artifact("more", "text/plain", b"x").expect("put");
        assert_eq!(next.id.as_str(), "a_0002");
        assert_eq!(store.artifact_count(), 2);
    }

    #[test]
    fn an_abandoned_artifact_is_not_stored() {
        let store = MemoryStore::new();
        let mut writer = store.begin_artifact("half", "text/plain").expect("begin");
        writer.write_all(b"half").expect("write");
        drop(writer);
        assert_eq!(store.artifact_count(), 0);
    }

    #[test]
    fn a_missing_artifact_is_an_error_for_the_model() {
        let store = MemoryStore::new();
        let error = store
            .open_artifact(&ArtifactId::new("a_9999"))
            .err()
            .expect("there is no such artifact");
        assert_eq!(error.code(), "no_such_artifact");
        assert_eq!(error.kind(), ErrorKind::Tool);
        assert_eq!(
            error.to_string(),
            "there is no artifact `a_9999` in this session"
        );
    }

    #[test]
    fn a_version_is_kept_once_under_its_hash() {
        let store = MemoryStore::new();
        let first = store.put_version(b"fn main() {}\n").expect("put");
        let again = store.put_version(b"fn main() {}\n").expect("put");
        assert_eq!(first, again);
        assert_eq!(first, ContentHash::of(b"fn main() {}\n"));
        assert_eq!(store.version_count(), 1);
        assert_eq!(
            store.get_version(&first).expect("get"),
            Some(b"fn main() {}\n".to_vec())
        );
        assert_eq!(
            store.get_version(&ContentHash::of(b"other")).expect("get"),
            None
        );
    }
}
