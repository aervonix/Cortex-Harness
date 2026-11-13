//! The session index: what listing a project's sessions needs, cached in
//! one file (`docs/architecture/07-sessions-and-context.md`, "The index").
//!
//! It is a cache, never the source of truth. It is rewritten atomically and
//! privately, under a lock, when a session changes (at the end of a turn and
//! when the session is closed), without a flush to the disk, because it can
//! be rebuilt from the logs. A listing reads it and the project's directory
//! (one directory read, no file is opened): a session the index does not
//! have is read from its log and added, and one whose directory is gone is
//! dropped. A missing or unreadable index is rebuilt.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io;

use cortex_base::{AtomicWriteOptions, SystemClock, UtcTime, private, write_atomic};
use cortex_protocol::ids::{ModelId, SessionId, Timestamp};
use cortex_protocol::session::SessionSummary;
use serde::{Deserialize, Serialize};

use crate::error::{Result, SessionError};
use crate::layout::{ProjectSessions, is_plain_name};
use crate::lock;
use crate::log::Log;

/// The version of the index file. Another version is rebuilt.
const INDEX_VERSION: u64 = 1;

/// What the index keeps about one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexRecord {
    /// The session.
    pub id: SessionId,
    /// The workspace root.
    pub workspace: String,
    /// Its title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Its first prompt, on one line, cut to 200 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    /// When it was created.
    pub created: Timestamp,
    /// When it was last written to.
    pub updated: Timestamp,
    /// How many entries its log has.
    pub entries: u64,
    /// How many turns it has had.
    pub turns: u64,
    /// The session it was forked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    /// The main model of its latest context epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelId>,
    /// The size of its log.
    pub bytes: u64,
}

impl From<&IndexRecord> for SessionSummary {
    fn from(record: &IndexRecord) -> Self {
        Self {
            session_id: record.id.clone(),
            workspace: record.workspace.clone(),
            title: record.title.clone(),
            created_at: record.created.clone(),
            updated_at: record.updated.clone(),
            parent: record.parent.clone(),
            model: record.model.clone(),
            turns: record.turns,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct IndexFile {
    v: u64,
    sessions: Vec<IndexRecord>,
}

/// The index of one project's sessions.
#[derive(Debug, Clone)]
pub struct SessionIndex {
    project: ProjectSessions,
}

impl SessionIndex {
    pub(crate) fn new(project: ProjectSessions) -> Self {
        Self { project }
    }

    /// The project's sessions, newest first (by when they were last written
    /// to). Reads the index and the project's directory; reads a log only
    /// for a session the index does not have yet.
    ///
    /// # Errors
    ///
    /// When the project's directory cannot be read, or a corrected index
    /// cannot be written.
    pub fn list(&self) -> Result<Vec<IndexRecord>> {
        let on_disk = self.session_dirs()?;
        let Some(mut records) = self.read()? else {
            return self.rebuild();
        };
        let known: BTreeSet<SessionId> = records.keys().cloned().collect();
        let stale = known != on_disk;
        if stale {
            records.retain(|id, _| on_disk.contains(id));
            let mut added = Vec::new();
            for id in on_disk.difference(&known) {
                if let Some(record) = self.record_from_log(id) {
                    added.push(record);
                }
            }
            if added.is_empty() && records.len() == known.len() {
                return Ok(newest_first(records.into_values().collect()));
            }
            // Written under the lock, merged with what another process may
            // have written meanwhile.
            let fixed = self.change(|current| {
                current.retain(|id, _| on_disk.contains(id));
                for record in &added {
                    current.insert(record.id.clone(), record.clone());
                }
            })?;
            return Ok(newest_first(fixed.into_values().collect()));
        }
        Ok(newest_first(records.into_values().collect()))
    }

    /// The session written to most recently, for `--continue`.
    ///
    /// # Errors
    ///
    /// As for [`list`](Self::list).
    pub fn latest(&self) -> Result<Option<IndexRecord>> {
        Ok(self.list()?.into_iter().next())
    }

    /// The session an id or a unique prefix of one names, for `--resume`.
    ///
    /// # Errors
    ///
    /// [`SessionError::NotFound`] when none matches, and
    /// [`SessionError::AmbiguousPrefix`] when several do.
    pub fn find(&self, prefix: &str) -> Result<SessionId> {
        let ids = self.session_dirs()?;
        let exact = SessionId::new(prefix);
        if ids.contains(&exact) {
            return Ok(exact);
        }
        let matching: Vec<&SessionId> = ids
            .iter()
            .filter(|id| !prefix.is_empty() && id.as_str().starts_with(prefix))
            .collect();
        match matching.as_slice() {
            [] => Err(SessionError::NotFound {
                session: prefix.to_owned(),
            }),
            [one] => Ok((*one).clone()),
            many => Err(SessionError::AmbiguousPrefix {
                prefix: prefix.to_owned(),
                candidates: many.iter().map(ToString::to_string).collect(),
            }),
        }
    }

    /// Reads every session's log and writes the index again.
    ///
    /// # Errors
    ///
    /// When the project's directory cannot be read or the index cannot be
    /// written.
    pub fn rebuild(&self) -> Result<Vec<IndexRecord>> {
        let records: BTreeMap<SessionId, IndexRecord> = self
            .session_dirs()?
            .iter()
            .filter_map(|id| self.record_from_log(id))
            .map(|record| (record.id.clone(), record))
            .collect();
        let written = self.change(|current| *current = records.clone())?;
        Ok(newest_first(written.into_values().collect()))
    }

    /// Puts one session's record in the index.
    ///
    /// # Errors
    ///
    /// When the index cannot be written.
    pub fn update(&self, record: IndexRecord) -> Result<()> {
        self.change(|current| {
            current.insert(record.id.clone(), record.clone());
        })
        .map(|_| ())
    }

    fn record_from_log(&self, id: &SessionId) -> Option<IndexRecord> {
        let dir = self.project.session(id)?;
        let (log, _) = Log::open(&dir).ok()?;
        log.index_record().ok()
    }

    /// The sessions that have a directory.
    fn session_dirs(&self) -> Result<BTreeSet<SessionId>> {
        let path = self.project.path();
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
            Err(error) => {
                return Err(SessionError::io("list the sessions", path.to_path_buf())(
                    error,
                ));
            }
        };
        Ok(entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| is_plain_name(name))
            .map(SessionId::new)
            .collect())
    }

    /// The index as it is on disk. `None` when it is missing, unreadable or
    /// of another version.
    fn read(&self) -> Result<Option<BTreeMap<SessionId, IndexRecord>>> {
        let path = self.project.index_file();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(SessionError::io("read the session index", path)(error)),
        };
        Ok(serde_json::from_slice::<IndexFile>(&bytes)
            .ok()
            .filter(|file| file.v == INDEX_VERSION)
            .map(|file| {
                file.sessions
                    .into_iter()
                    .map(|record| (record.id.clone(), record))
                    .collect()
            }))
    }

    /// Reads, changes and writes the index with the lock held throughout.
    fn change(
        &self,
        change: impl FnOnce(&mut BTreeMap<SessionId, IndexRecord>),
    ) -> Result<BTreeMap<SessionId, IndexRecord>> {
        let directory = self.project.path();
        private::create_dir_all(directory).map_err(SessionError::io(
            "create the project's directory",
            directory.to_path_buf(),
        ))?;
        let _lock = self.lock()?;
        let mut records = self.read()?.unwrap_or_default();
        change(&mut records);
        let file = IndexFile {
            v: INDEX_VERSION,
            sessions: records.values().cloned().collect(),
        };
        let bytes = serde_json::to_vec(&file)?;
        let options = AtomicWriteOptions {
            owner_only: true,
            // A cache: rebuilt if a crash loses it.
            sync: false,
            ..AtomicWriteOptions::default()
        };
        let path = self.project.index_file();
        write_atomic(&path, &bytes, &options, &SystemClock::new()).map_err(|error| {
            SessionError::Io {
                action: "write the session index",
                path: path.clone(),
                source: io::Error::other(error.to_string()),
            }
        })?;
        Ok(records)
    }

    /// The lock that keeps two writers of the index apart. Released when
    /// the file is dropped, or by the OS if the process dies.
    fn lock(&self) -> Result<File> {
        // Held for one read and one write of a small file. On a file system
        // that cannot lock, as for a session's lock, there is none to hold.
        lock::take_waiting(&self.project.index_lock())
    }
}

fn newest_first(mut records: Vec<IndexRecord>) -> Vec<IndexRecord> {
    // Times are compared as times: `…02.250Z` is after `…02Z`, which a
    // comparison of the text gets wrong.
    let time = |stamp: &Timestamp| UtcTime::parse(stamp.as_str()).unwrap_or(UtcTime::UNIX_EPOCH);
    records.sort_by(|a, b| {
        time(&b.updated)
            .cmp(&time(&a.updated))
            .then_with(|| b.id.cmp(&a.id))
    });
    records
}
