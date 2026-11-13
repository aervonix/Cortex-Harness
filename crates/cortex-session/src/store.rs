//! Artifacts and stored file contents, in the session's directory.
//!
//! - **Artifacts** (`artifacts/<id>.<ext>`): the full output of a call whose
//!   result was cut, an attached image, the patch of a base state. Written
//!   in pieces, under a temporary name until they are finished, so a
//!   half-written artifact never has an artifact's name. Ids count up per
//!   session: `a_0001`, the form the model is told.
//! - **Stored contents** (`checkpoints/<hash>`): a file's content filed
//!   under its [`ContentHash`]. The checkpoints that make undo possible and
//!   the file versions behind changed-only re-reads are the same thing, so
//!   they share one store, and content kept twice is kept once. A read
//!   checks the content against its hash.
//!
//! Every file is created private ([`cortex_base::private`]), and every
//! method blocks.
//!
//! **Against `cortex-tools`' `ContentStore`.** [`DiskStore::begin_artifact`]
//! is `begin_artifact` (an [`ArtifactFile`] is the writer, and
//! [`ArtifactFile::finish`] its `finish`), [`DiskStore::open_artifact`] is
//! `open_artifact`, [`DiskStore::put_blob`] is `put_version` and
//! [`DiskStore::get_blob`] is `get_version`. The core adapts one to the
//! other.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use cortex_base::rng::{Rng, SystemRng};
use cortex_base::{ContentHash, ContentHasher, private};
use cortex_protocol::ids::ArtifactId;
use cortex_protocol::item::ArtifactRef;

use crate::entry::{self, FileVersion};
use crate::error::{Result, SessionError};
use crate::layout::SessionDir;
use crate::redact::Redactor;

/// The largest file whose content a checkpoint keeps: 16 MiB.
///
/// A file over it is recorded by hash and size only, and an undo says it
/// cannot put it back. The files the editing tools change are source files,
/// far smaller than this. A file over it is generated output, a database or
/// a media file, where a copy per turn costs time and disk for little. And
/// a restore holds the content in memory, which this keeps inside the
/// memory budgets (`docs/performance-budgets.md`).
pub const KEEP_LIMIT: u64 = 16 * 1024 * 1024;

/// A text artifact's line that grows past this without a newline is
/// redacted as it is, so that a stream with no newlines is not held.
const REDACT_PENDING: usize = 64 * 1024;

/// A session's artifacts and stored contents. Shared between threads.
#[derive(Debug)]
pub struct DiskStore {
    dir: SessionDir,
    redactor: Arc<dyn Redactor>,
    /// Flush content to the disk before it takes its name.
    sync: bool,
    /// The number of the last artifact, once the directory has been looked
    /// at.
    last_artifact: Mutex<Option<u64>>,
}

impl DiskStore {
    /// The store of the session in `dir`. Nothing is created until
    /// something is stored.
    #[must_use]
    pub fn new(dir: SessionDir, redactor: Arc<dyn Redactor>, sync: bool) -> Self {
        Self {
            dir,
            redactor,
            sync,
            last_artifact: Mutex::new(None),
        }
    }

    /// Starts an artifact. `label` says what it holds, in a few words, and
    /// `media_type` what kind of content it is. Text (`text/*` and JSON)
    /// passes through the redactor as it is written.
    ///
    /// # Errors
    ///
    /// When the artifacts directory or the file cannot be created.
    pub fn begin_artifact(&self, label: &str, media_type: &str) -> Result<ArtifactFile> {
        let directory = self.dir.artifacts();
        private::create_dir_all(&directory).map_err(SessionError::io(
            "create the artifacts directory",
            directory.clone(),
        ))?;
        let number = self.next_artifact(&directory)?;
        let id = ArtifactId::new(format!("a_{number:04}"));
        let part = directory.join(format!("{id}.part"));
        let finished = directory.join(format!("{id}.{}", extension(media_type)));
        let file = private::create_new(&part)
            .map_err(SessionError::io("create the artifact", part.clone()))?;
        let redact = is_text(media_type).then(|| LineRedactor {
            redactor: Arc::clone(&self.redactor),
            pending: Vec::new(),
        });
        Ok(ArtifactFile {
            file: Some(file),
            part,
            finished,
            id,
            label: label.to_owned(),
            bytes: 0,
            redact,
            sync: self.sync,
        })
    }

    /// Stores an artifact whose whole content is at hand.
    ///
    /// # Errors
    ///
    /// As for [`begin_artifact`](Self::begin_artifact) and writing.
    pub fn put_artifact(
        &self,
        label: &str,
        media_type: &str,
        content: &[u8],
    ) -> Result<ArtifactRef> {
        let mut file = self.begin_artifact(label, media_type)?;
        let path = file.part.clone();
        file.write_all(content)
            .map_err(SessionError::io("write the artifact", path))?;
        file.finish()
    }

    fn next_artifact(&self, directory: &Path) -> Result<u64> {
        let mut last = self
            .last_artifact
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let current = match *last {
            Some(number) => number,
            None => {
                // Continue after the highest number in the directory,
                // finished or not, so an id is never given twice.
                let mut highest = 0;
                let entries = fs::read_dir(directory).map_err(SessionError::io(
                    "list the artifacts",
                    directory.to_path_buf(),
                ))?;
                for found in entries.flatten() {
                    let name = found.file_name();
                    let Some(name) = name.to_str() else { continue };
                    let stem = name.split('.').next().unwrap_or("");
                    if let Some(number) = entry::number_after(stem, "a_") {
                        highest = highest.max(number);
                    }
                }
                highest
            }
        };
        let next = current + 1;
        *last = Some(next);
        Ok(next)
    }

    /// Where a finished artifact is, by its id.
    ///
    /// # Errors
    ///
    /// [`SessionError::NoSuchEntry`] when there is no such artifact, or the
    /// id is not one this store makes.
    pub fn artifact_path(&self, id: &ArtifactId) -> Result<PathBuf> {
        let missing = || SessionError::NoSuchEntry {
            entry: id.to_string(),
        };
        if !entry::is_artifact_id(id) {
            return Err(missing());
        }
        let directory = self.dir.artifacts();
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(missing()),
            Err(error) => return Err(SessionError::io("list the artifacts", directory)(error)),
        };
        let prefix = format!("{id}.");
        for found in entries.flatten() {
            let name = found.file_name();
            if let Some(name) = name.to_str()
                && let Some(extension) = name.strip_prefix(&prefix)
                && extension != "part"
                && !extension.contains('.')
            {
                return Ok(found.path());
            }
        }
        Err(missing())
    }

    /// Opens an artifact for reading from its start.
    ///
    /// # Errors
    ///
    /// When there is no such artifact or it cannot be opened.
    pub fn open_artifact(&self, id: &ArtifactId) -> Result<File> {
        let path = self.artifact_path(id)?;
        File::open(&path).map_err(SessionError::io("open the artifact", path))
    }

    fn blob_path(&self, hash: &ContentHash) -> PathBuf {
        self.dir.checkpoints().join(hash.to_hex())
    }

    /// Whether content with this hash is stored.
    #[must_use]
    pub fn has_blob(&self, hash: &ContentHash) -> bool {
        fs::symlink_metadata(self.blob_path(hash)).is_ok()
    }

    /// Stores content under its hash, and returns the hash. Storing the
    /// same content twice stores it once.
    ///
    /// # Errors
    ///
    /// When the content cannot be written.
    pub fn put_blob(&self, content: &[u8]) -> Result<ContentHash> {
        let hash = ContentHash::of(content);
        if self.has_blob(&hash) {
            return Ok(hash);
        }
        let (temporary, mut file) = self.temporary_blob()?;
        let written = file
            .write_all(content)
            .and_then(|()| if self.sync { file.sync_all() } else { Ok(()) });
        drop(file);
        self.settle_blob(temporary, &hash, written)?;
        Ok(hash)
    }

    /// Stores the content of the file at `path`, streamed, unless it is
    /// larger than `limit`: then only its hash and size are recorded, and
    /// [`FileVersion::kept`] is false. `None` when there is no file.
    ///
    /// # Errors
    ///
    /// When the file exists and cannot be read, or the store cannot be
    /// written.
    pub fn put_file(&self, path: &Path, limit: u64) -> Result<Option<FileVersion>> {
        let mut source = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SessionError::io("read the file", path.to_path_buf())(error)),
        };
        let mut hasher = ContentHasher::new();
        let mut copy = Some(self.temporary_blob()?);
        let mut bytes: u64 = 0;
        let mut buffer = vec![0u8; 64 * 1024];
        let outcome = loop {
            let read = match source.read(&mut buffer) {
                Ok(0) => break Ok(()),
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => break Err(error),
            };
            hasher.update(&buffer[..read]);
            bytes += read as u64;
            if bytes > limit
                && let Some((temporary, file)) = copy.take()
            {
                // Over the limit: keep hashing, stop copying.
                drop(file);
                let _ = fs::remove_file(temporary);
            }
            if let Some((_, file)) = &mut copy
                && let Err(error) = file.write_all(&buffer[..read])
            {
                break Err(error);
            }
        };
        let hash = hasher.finish();
        let Some((temporary, file)) = copy else {
            outcome.map_err(SessionError::io("read the file", path.to_path_buf()))?;
            return Ok(Some(FileVersion {
                hash,
                bytes,
                kept: false,
            }));
        };
        let finished = outcome.and_then(|()| if self.sync { file.sync_all() } else { Ok(()) });
        drop(file);
        if self.has_blob(&hash) {
            let _ = fs::remove_file(&temporary);
            finished.map_err(SessionError::io("read the file", path.to_path_buf()))?;
        } else {
            self.settle_blob(temporary, &hash, finished)?;
        }
        Ok(Some(FileVersion {
            hash,
            bytes,
            kept: true,
        }))
    }

    /// The content stored under a hash, checked against it. `None` when it
    /// was never stored.
    ///
    /// # Errors
    ///
    /// [`SessionError::Corrupt`] when the content no longer matches its
    /// hash; otherwise when it cannot be read.
    pub fn get_blob(&self, hash: &ContentHash) -> Result<Option<Vec<u8>>> {
        let path = self.blob_path(hash);
        let content = match fs::read(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SessionError::io("read stored content", path)(error)),
        };
        if ContentHash::of(&content) != *hash {
            return Err(SessionError::Corrupt {
                hash: hash.to_hex(),
            });
        }
        Ok(Some(content))
    }

    fn temporary_blob(&self) -> Result<(PathBuf, File)> {
        let directory = self.dir.checkpoints();
        private::create_dir_all(&directory).map_err(SessionError::io(
            "create the checkpoints directory",
            directory.clone(),
        ))?;
        let mut rng = SystemRng::new();
        loop {
            let temporary = directory.join(format!(".part-{:016x}", rng.next_u64()));
            match private::create_new(&temporary) {
                Ok(file) => return Ok((temporary, file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(SessionError::io("create stored content", temporary)(error));
                }
            }
        }
    }

    /// Gives a written temporary file its hash's name, or removes it.
    fn settle_blob(
        &self,
        temporary: PathBuf,
        hash: &ContentHash,
        written: io::Result<()>,
    ) -> Result<()> {
        let target = self.blob_path(hash);
        let renamed = written.and_then(|()| fs::rename(&temporary, &target));
        if let Err(error) = renamed {
            let _ = fs::remove_file(&temporary);
            // Another writer stored the same content first.
            if !self.has_blob(hash) {
                return Err(SessionError::io("store content", target)(error));
            }
        }
        Ok(())
    }
}

/// An artifact being written. Dropping it without
/// [`finish`](Self::finish) removes what was written.
#[derive(Debug)]
pub struct ArtifactFile {
    file: Option<File>,
    part: PathBuf,
    finished: PathBuf,
    id: ArtifactId,
    label: String,
    bytes: u64,
    redact: Option<LineRedactor>,
    sync: bool,
}

impl ArtifactFile {
    /// The id the artifact will have.
    #[must_use]
    pub fn id(&self) -> &ArtifactId {
        &self.id
    }

    /// Completes the artifact, gives it its name, and returns the reference
    /// a result carries.
    ///
    /// # Errors
    ///
    /// When the content cannot be written or flushed, or the file cannot
    /// be renamed. The artifact is then removed.
    pub fn finish(mut self) -> Result<ArtifactRef> {
        let rest = self.redact.as_mut().map(LineRedactor::take_all);
        let mut file = self
            .file
            .take()
            .ok_or_else(|| SessionError::Invalid("the artifact was already finished".to_owned()))?;
        let done = (|| {
            if let Some(rest) = rest {
                file.write_all(&rest)?;
            }
            if self.sync {
                file.sync_all()?;
            }
            drop(file);
            fs::rename(&self.part, &self.finished)
        })();
        if let Err(error) = done {
            let _ = fs::remove_file(&self.part);
            return Err(SessionError::io(
                "finish the artifact",
                self.finished.clone(),
            )(error));
        }
        Ok(ArtifactRef {
            id: self.id.clone(),
            label: std::mem::take(&mut self.label),
            bytes: Some(self.bytes),
        })
    }
}

impl Write for ArtifactFile {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("the artifact was finished"))?;
        match &mut self.redact {
            Some(redact) => {
                if let Some(ready) = redact.push(data) {
                    file.write_all(&ready)?;
                }
            }
            None => file.write_all(data)?,
        }
        self.bytes += data.len() as u64;
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.as_mut().map_or(Ok(()), Write::flush)
    }
}

impl Drop for ArtifactFile {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            drop(file);
            let _ = fs::remove_file(&self.part);
        }
    }
}

/// Redacts a text artifact a line at a time, as it arrives.
#[derive(Debug)]
struct LineRedactor {
    redactor: Arc<dyn Redactor>,
    pending: Vec<u8>,
}

impl LineRedactor {
    /// Takes in more content, and gives back whatever is ready to write:
    /// whole lines, redacted.
    fn push(&mut self, data: &[u8]) -> Option<Vec<u8>> {
        self.pending.extend_from_slice(data);
        let cut = match self.pending.iter().rposition(|&byte| byte == b'\n') {
            Some(newline) => newline + 1,
            None if self.pending.len() > REDACT_PENDING => self.pending.len(),
            None => return None,
        };
        let ready: Vec<u8> = self.pending.drain(..cut).collect();
        Some(self.redact(ready))
    }

    fn take_all(&mut self) -> Vec<u8> {
        let rest = std::mem::take(&mut self.pending);
        self.redact(rest)
    }

    fn redact(&self, bytes: Vec<u8>) -> Vec<u8> {
        if self.redactor.is_noop() {
            return bytes;
        }
        match String::from_utf8(bytes) {
            Ok(text) => self.redactor.redact(&text).into_owned().into_bytes(),
            // Not text after all: written as it is.
            Err(error) => error.into_bytes(),
        }
    }
}

/// Whether content of this media type is text the redactor reads.
fn is_text(media_type: &str) -> bool {
    media_type.starts_with("text/") || media_type == "application/json"
}

/// The file extension for a media type.
fn extension(media_type: &str) -> &'static str {
    match media_type.split(';').next().unwrap_or("").trim() {
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/x-diff" | "text/x-patch" => "diff",
        "application/json" => "json",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        other if other.starts_with("text/") => "txt",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types_have_extensions() {
        assert_eq!(extension("text/plain; charset=utf-8"), "txt");
        assert_eq!(extension("text/x-diff"), "diff");
        assert_eq!(extension("image/png"), "png");
        assert_eq!(extension("text/csv"), "txt");
        assert_eq!(extension("application/x-tar"), "bin");
        assert!(is_text("text/plain"));
        assert!(is_text("application/json"));
        assert!(!is_text("image/png"));
    }

    #[test]
    fn lines_are_redacted_whole_wherever_the_pieces_split() {
        let text = "one hunter2\ntwo hun";
        let rest = "ter2 three\nfour";
        let mut redact = LineRedactor {
            redactor: Arc::new(crate::redact::tests::MaskHunter),
            pending: Vec::new(),
        };
        let mut out = redact.push(text.as_bytes()).unwrap();
        out.extend(redact.push(rest.as_bytes()).unwrap());
        out.extend(redact.take_all());
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "one [redacted]\ntwo [redacted] three\nfour"
        );
    }
}
