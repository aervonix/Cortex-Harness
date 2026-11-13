//! The file store: `credentials.toml`, readable by its owner only.
//!
//! ```toml
//! # Cortex-Harness credentials. Keep this file private.
//! version = 1
//!
//! [providers.anthropic]
//! key = "…"
//! ```
//!
//! - **Owner-only, always.** Every write goes through
//!   [`write_atomic`] with `owner_only`, so the file is mode `0600` on
//!   Linux and has an access list with the user alone on Windows, whether
//!   this store created it or the user did by hand. A file that is found
//!   open to others is still read (the harm is done, and refusing would
//!   only stop the user working); [`FileStore::is_private`] reports it, and
//!   the next write closes it.
//! - **Writers do not interleave.** A change is read, modify, write. A lock
//!   file beside the credentials file is held across the three, so two
//!   processes that store keys at the same moment both keep theirs. The lock
//!   is the operating system's, taken on an open file: it goes when its
//!   holder exits, however it exits, so there is no stale lock to clear.
//! - **Readers need no lock.** The file is replaced in one step, so a
//!   reader sees the old content or the new, never part of each.
//! - **What is not understood is not overwritten.** A file with a newer
//!   `version`, or one that is not TOML, is an error and is left as it is.
//!   Entries and keys this version does not know are kept when the file is
//!   rewritten.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "blocking file calls; a credential store is used on the blocking pool"
)]

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use cortex_base::{AtomicWriteOptions, Secret, SharedClock, private, write_atomic};
use toml::Value;

use super::{CredentialError, CredentialStore, StoreKind, check_provider};
use crate::document;
use crate::env::Env;

/// The version of the file's format that this build writes.
const VERSION: i64 = 1;

/// What the file starts with.
const HEADER: &str = "# Cortex-Harness credentials. Keep this file private.\n\
                      # Written by `cortex auth login`; removed by `cortex auth logout`.\n";

/// A credential store in one owner-only file.
#[derive(Debug, Clone)]
pub struct FileStore {
    path: PathBuf,
    clock: SharedClock,
}

impl FileStore {
    /// A store in the file at `path`. Nothing is created until a
    /// credential is stored.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, clock: SharedClock) -> Self {
        Self {
            path: path.into(),
            clock,
        }
    }

    /// The credentials file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether only the user can open the credentials file. `None` when
    /// there is no file yet.
    ///
    /// # Errors
    ///
    /// When the file cannot be looked at.
    pub fn is_private(&self) -> io::Result<Option<bool>> {
        match private::is_private(&self.path) {
            Ok(answer) => Ok(Some(answer)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn io(&self, action: &'static str) -> impl Fn(io::Error) -> CredentialError + '_ {
        move |source| CredentialError::Io {
            action,
            path: self.path.clone(),
            source,
        }
    }

    fn malformed(&self, reason: impl Into<String>) -> CredentialError {
        CredentialError::Malformed {
            path: self.path.clone(),
            reason: reason.into(),
        }
    }

    /// Refuses a link at one of the store's two paths: the file and its
    /// lock are files of their own, or nothing.
    ///
    /// The check is made just before each use, and the use opens the path
    /// again: a link put there in between, by a process of the same user,
    /// is followed. Closing that needs an open that does not follow links,
    /// which `cortex-base` gains in step 10.
    fn refuse_link(path: &Path) -> Result<(), CredentialError> {
        match std::fs::symlink_metadata(path) {
            Ok(found) if found.file_type().is_symlink() => Err(CredentialError::Link {
                path: path.to_path_buf(),
            }),
            _ => Ok(()),
        }
    }

    /// The lock file: the credentials file's name with `.lock` added.
    fn lock_path(&self) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(".lock");
        PathBuf::from(name)
    }

    /// Reads the whole file. `None` when there is none.
    fn read(&self) -> Result<Option<toml::Table>, CredentialError> {
        Self::refuse_link(&self.path)?;
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                return Err(self.malformed("it is not UTF-8 text"));
            }
            Err(error) => return Err(self.io("read the credentials file")(error)),
        };
        // The parser's own description and a line number. Never the text of
        // the file: a line of it may be a key.
        let table = document::parse(&text)
            .map_err(|failure| {
                self.malformed(match failure.line {
                    Some(line) => format!("line {line} is not valid TOML"),
                    None => "it is not valid TOML".to_owned(),
                })
            })?
            .table;
        match table.get("version") {
            Some(Value::Integer(version)) if *version == VERSION => {}
            Some(Value::Integer(version)) if *version > VERSION => {
                return Err(self.malformed(format!(
                    "it was written by a newer version of Cortex-Harness (format {version}; this \
                     version understands {VERSION})"
                )));
            }
            // A file made by hand may leave the version out.
            None => {}
            Some(_) => return Err(self.malformed("its `version` is not one this version knows")),
        }
        match table.get("providers") {
            None | Some(Value::Table(_)) => Ok(Some(table)),
            Some(_) => Err(self.malformed("`providers` is not a table")),
        }
    }

    /// Reads, changes and writes the file, with the lock held throughout.
    /// `change` returns whether there is anything to write.
    fn update(
        &self,
        change: impl FnOnce(&mut toml::Table) -> bool,
    ) -> Result<bool, CredentialError> {
        // Before anything is created: a refused store leaves no lock behind.
        Self::refuse_link(&self.path)?;
        let directory = match self.path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        // A missing directory is created for the user alone.
        // One that exists is left as it is: the file is private by itself.
        private::create_dir_all(directory).map_err(self.io("create the directory of"))?;
        let _lock = self.lock()?;

        let mut table = self.read()?.unwrap_or_default();
        if !change(&mut table) {
            return Ok(false);
        }
        table.insert("version".to_owned(), Value::Integer(VERSION));
        let text = format!("{HEADER}{table}");
        let options = AtomicWriteOptions {
            owner_only: true,
            ..AtomicWriteOptions::default()
        };
        // An atomic write follows a link to the file it points to, which is
        // what an editor wants and this store must not do.
        Self::refuse_link(&self.path)?;
        write_atomic(&self.path, text.as_bytes(), &options, &*self.clock).map_err(|error| {
            CredentialError::Io {
                action: "write the credentials file",
                path: self.path.clone(),
                source: io::Error::other(error.to_string()),
            }
        })?;
        Ok(true)
    }

    /// Takes the lock that keeps two writers apart. It is released when the
    /// returned file is dropped, and by the operating system if this
    /// process dies first.
    fn lock(&self) -> Result<File, CredentialError> {
        let path = self.lock_path();
        Self::refuse_link(&path)?;
        let open = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
        };
        let file = match open() {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match private::create_new(&path) {
                    Ok(_) => {}
                    // Another process made it first.
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(self.io("create the lock file of")(error)),
                }
                open().map_err(self.io("open the lock file of"))?
            }
            Err(error) => return Err(self.io("open the lock file of")(error)),
        };
        // Blocks while another writer holds it, which is for the length of
        // one read and one write of a small file.
        file.lock().map_err(self.io("lock"))?;
        Ok(file)
    }
}

impl CredentialStore for FileStore {
    fn kind(&self) -> StoreKind {
        StoreKind::File
    }

    fn describe(&self, env: &Env) -> String {
        format!("the file {}", env.display_path(&self.path))
    }

    fn warning(&self, env: &Env) -> Option<String> {
        if let Err(error) =
            Self::refuse_link(&self.path).and_then(|()| Self::refuse_link(&self.lock_path()))
        {
            return Some(format!("the credential store cannot be used: {error}"));
        }
        match self.is_private() {
            Ok(Some(false)) => Some(format!(
                "the credentials file {} can be opened by other users of this machine. It is \
                 made private the next time a key is stored; until then, restrict it yourself",
                env.display_path(&self.path)
            )),
            Ok(Some(true) | None) => None,
            Err(error) => Some(format!(
                "could not check who can open the credentials file {}: {error}",
                env.display_path(&self.path)
            )),
        }
    }

    fn get(&self, provider: &str) -> Result<Option<Secret>, CredentialError> {
        check_provider(provider)?;
        let Some(table) = self.read()? else {
            return Ok(None);
        };
        let entry = table
            .get("providers")
            .and_then(|providers| providers.get(provider));
        match entry.map(|entry| entry.get("key")) {
            None | Some(None) => Ok(None),
            Some(Some(Value::String(key))) if !key.is_empty() => Ok(Some(Secret::new(key.clone()))),
            Some(Some(Value::String(_))) => Ok(None),
            Some(Some(_)) => Err(self.malformed(format!("the key of `{provider}` is not text"))),
        }
    }

    fn set(&self, provider: &str, credential: &Secret) -> Result<(), CredentialError> {
        check_provider(provider)?;
        self.update(|table| {
            let providers = table
                .entry("providers")
                .or_insert_with(|| Value::Table(toml::Table::new()));
            if let Value::Table(providers) = providers {
                let entry = providers
                    .entry(provider)
                    .or_insert_with(|| Value::Table(toml::Table::new()));
                if !entry.is_table() {
                    *entry = Value::Table(toml::Table::new());
                }
                if let Value::Table(entry) = entry {
                    entry.insert(
                        "key".to_owned(),
                        Value::String(credential.expose().to_owned()),
                    );
                }
            }
            true
        })
        .map(drop)
    }

    fn remove(&self, provider: &str) -> Result<bool, CredentialError> {
        check_provider(provider)?;
        // Nothing to remove from a file that is not there, and no reason to
        // create a directory and a lock file to find that out.
        if self.read()?.is_none() {
            return Ok(false);
        }
        self.update(|table| match table.get_mut("providers") {
            Some(Value::Table(providers)) => providers.remove(provider).is_some(),
            _ => false,
        })
    }
}
