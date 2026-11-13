//! The session's store, as the tools and the provider layer reach it
//! (`docs/architecture/05-tools.md`, "The content store";
//! `docs/architecture/04-providers.md`, "Images").
//!
//! `cortex-tools` keeps artifacts and file versions through its own
//! [`ContentStore`] trait, and `cortex-llm` loads a stored image through
//! its [`ImageSource`]. Neither can see `cortex-session`, whose
//! [`DiskStore`] is where both really are. The two adapters here join
//! them: an artifact a tool writes is a file of the session's directory,
//! redacted and owner-only as the store writes it, a file version is a
//! blob of the checkpoint store, and an image a `read` stored as an
//! artifact is what a request sends.

#![allow(
    clippy::disallowed_types,
    reason = "the content store's calls block, and are made on the blocking pool"
)]

use std::io::{self, Read, Write};
use std::sync::Arc;

use cortex_base::ContentHash;
use cortex_llm::{BoxFuture, ImageError, ImageSource};
use cortex_protocol::ArtifactId;
use cortex_protocol::item::ArtifactRef;
use cortex_session::{ArtifactFile, DiskStore, SessionError};
use cortex_tools::{ArtifactWriter, ContentStore, StoreError};

/// The largest image a request is given: what no provider takes more than.
/// A larger artifact is not read into memory to find that out.
pub(crate) const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

fn store_error(error: SessionError) -> StoreError {
    match error {
        SessionError::NoSuchEntry { entry } => StoreError::NoSuchArtifact(entry),
        other => StoreError::Io(io::Error::other(other.to_string())),
    }
}

/// A session's [`DiskStore`] as the tools' content store.
#[derive(Debug, Clone)]
pub(crate) struct DiskContent(pub(crate) Arc<DiskStore>);

struct Writer(ArtifactFile);

impl Write for Writer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl ArtifactWriter for Writer {
    fn finish(self: Box<Self>) -> Result<ArtifactRef, StoreError> {
        self.0.finish().map_err(store_error)
    }
}

impl ContentStore for DiskContent {
    fn begin_artifact(
        &self,
        label: &str,
        media_type: &str,
    ) -> Result<Box<dyn ArtifactWriter>, StoreError> {
        let file = self
            .0
            .begin_artifact(label, media_type)
            .map_err(store_error)?;
        Ok(Box::new(Writer(file)))
    }

    fn open_artifact(&self, id: &ArtifactId) -> Result<Box<dyn Read + Send>, StoreError> {
        let file = self.0.open_artifact(id).map_err(store_error)?;
        Ok(Box::new(file))
    }

    fn put_version(&self, content: &[u8]) -> Result<ContentHash, StoreError> {
        self.0.put_blob(content).map_err(store_error)
    }

    fn get_version(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>, StoreError> {
        self.0.get_blob(hash).map_err(store_error)
    }
}

/// A session's content store as the place a request's images come from.
#[derive(Clone)]
pub(crate) struct StoredImages(pub(crate) Arc<dyn ContentStore>);

impl std::fmt::Debug for StoredImages {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredImages").finish_non_exhaustive()
    }
}

impl ImageSource for StoredImages {
    fn load<'a>(&'a self, artifact: &'a ArtifactId) -> BoxFuture<'a, Result<Vec<u8>, ImageError>> {
        let store = Arc::clone(&self.0);
        let id = artifact.clone();
        Box::pin(async move {
            let read = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
                let file = store
                    .open_artifact(&id)
                    .map_err(|error| format!("the stored image `{id}` cannot be read: {error}"))?;
                let mut bytes = Vec::new();
                file.take(MAX_IMAGE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| format!("the stored image `{id}` cannot be read: {error}"))?;
                if bytes.len() as u64 > MAX_IMAGE_BYTES {
                    return Err(format!(
                        "the stored image `{id}` is larger than {} MiB",
                        MAX_IMAGE_BYTES / (1024 * 1024)
                    ));
                }
                Ok(bytes)
            })
            .await;
            match read {
                Ok(Ok(bytes)) => Ok(bytes),
                Ok(Err(message)) => Err(ImageError::new(message)),
                Err(_) => Err(ImageError::new(
                    "the stored image could not be read: the read was abandoned",
                )),
            }
        })
    }
}
