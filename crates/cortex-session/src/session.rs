//! A session open for writing: one writer, holding the session's lock.
//!
//! **Memory never holds an entry that is not on disk.** An entry is added
//! to the tree only after its line is written (and flushed, with the
//! default durability). A write that fails leaves the file as it was, and
//! the session as it was: the caller treats the error as fatal for the
//! turn.
//!
//! **Nothing is on disk until the first user message.** A new session holds
//! its first entries (the header, a lease, an epoch) until a `user` entry
//! is appended, and then creates the log with all of them in one atomic
//! write, so the log never exists without its first line, and a session
//! that never got a prompt leaves nothing behind.
//!
//! **Resume repairs.** It sets aside a last line a crash cut off, adds a
//! newline a crash left out, and gives every tool call at the end of the
//! active branch that has no result an "interrupted" one. Tools are never
//! run again by themselves.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::ops::Deref;
use std::path::PathBuf;
use std::sync::Arc;

use cortex_base::{
    AbsPath, AtomicWriteOptions, IdCounter, PathStyle, SharedClock, private, write_atomic,
};
use cortex_protocol::ids::{ItemId, SessionId, TurnId};
use cortex_protocol::item::{Item, ToolOutcome, ToolResultItem};
use cortex_protocol::message::Part;

use crate::entry::{
    self, Body, Checkpoint, Entry, EntryId, ForkOrigin, Header, Kind, Outcome, Reverted, Rewind,
    RewindReason,
};
use crate::error::{Result, SessionError};
use crate::file::{DiskFile, Durability, LogFile, Writer};
use crate::layout::{ProjectSessions, SessionDir};
use crate::lock;
use crate::log::{Log, OpenReport};
use crate::redact::{NoRedaction, Redactor};
use crate::scan::MAX_LINE;
use crate::snapshot::{
    self, FileRestore, FileStatus, Repaired, Restore, RewindDone, RewindPlan, SwitchDone,
};
use crate::store::DiskStore;

/// How a session is written.
#[derive(Debug, Clone)]
pub struct SessionOptions {
    /// When the log is flushed to the disk. From the setting
    /// `session.durability`.
    pub durability: Durability,
    /// Where entries' times come from.
    pub clock: SharedClock,
    /// What masks secrets before anything is written.
    pub redactor: Arc<dyn Redactor>,
    /// The version of Cortex-Harness, recorded when a session is created.
    pub harness: String,
}

impl SessionOptions {
    /// Flushed after every entry, nothing redacted.
    #[must_use]
    pub fn new(clock: SharedClock, harness: impl Into<String>) -> Self {
        Self {
            durability: Durability::Item,
            clock,
            redactor: Arc::new(NoRedaction),
            harness: harness.into(),
        }
    }
}

/// What the result of a tool call says when the call never finished.
const INTERRUPTED: &str = "This tool call did not finish: the session stopped while it was \
                           running, or was rewound to before its result. It was not run again.";

/// A session open for writing. It reads like a [`Log`], which it derefs to.
#[derive(Debug)]
pub struct Session {
    log: Log,
    state: State,
    options: SessionOptions,
    entry_ids: IdCounter,
    item_ids: IdCounter,
    turn_ids: IdCounter,
    store: Arc<DiskStore>,
    warnings: Vec<PathBuf>,
    /// The longest line written: the longest the reader takes as an entry,
    /// [`MAX_LINE`]. Only a test sets another.
    max_line: usize,
}

#[derive(Debug)]
enum State {
    /// Nothing on disk yet: the entries waiting for the first user message.
    Unborn { pending: Vec<(Entry, Vec<u8>)> },
    /// The log exists, and this process holds its lock.
    Live {
        writer: Writer,
        #[expect(
            dead_code,
            reason = "held for its lock, which is released when it drops"
        )]
        lock: File,
    },
}

impl Deref for Session {
    type Target = Log;

    fn deref(&self) -> &Log {
        &self.log
    }
}

impl Drop for Session {
    /// A session that ends before its first prompt leaves nothing behind:
    /// what its store kept meanwhile (the base state) is removed, once no
    /// write to it is in progress, and nothing more is stored.
    fn drop(&mut self) {
        if matches!(self.state, State::Unborn { .. }) {
            self.store.abandon();
        }
    }
}

impl Session {
    /// Starts a new session in `project`. Nothing is written until the
    /// first user message is appended.
    ///
    /// # Errors
    ///
    /// When the id cannot be a directory name, or a session with this id
    /// already exists.
    pub fn create(
        project: &ProjectSessions,
        id: SessionId,
        workspace: &AbsPath,
        options: SessionOptions,
    ) -> Result<Self> {
        let dir = project
            .session(&id)
            .ok_or_else(|| SessionError::Invalid(format!("`{id}` cannot be a session id")))?;
        if fs::symlink_metadata(dir.log()).is_ok() {
            return Err(SessionError::Invalid(format!(
                "the session `{id}` exists already"
            )));
        }
        Ok(Self::unborn(
            dir,
            Header {
                session: id,
                workspace: workspace.as_str().to_owned(),
                harness: options.harness.clone(),
                forked_from: None,
            },
            options,
        ))
    }

    fn unborn(dir: SessionDir, header: Header, options: SessionOptions) -> Self {
        let now = options.clock.now().to_timestamp();
        let first = Entry {
            id: EntryId::header(),
            parent: None,
            ts: now.clone(),
            turn: None,
            redacted: false,
            body: Body::Session(header.clone()),
        };
        let store = Arc::new(DiskStore::unborn(
            dir.clone(),
            Arc::clone(&options.redactor),
            options.durability != Durability::Never,
        ));
        let mut session = Self {
            log: Log::unborn(dir, header, now),
            state: State::Unborn {
                pending: Vec::new(),
            },
            entry_ids: IdCounter::new("e_").padded(4),
            item_ids: IdCounter::new("i_"),
            turn_ids: IdCounter::new("t_"),
            store,
            warnings: Vec::new(),
            max_line: MAX_LINE,
            options,
        };
        // The header is written by the same code as every other entry.
        if let Ok(line) = entry::encode(&first, &*session.options.redactor)
            && let State::Unborn { pending } = &mut session.state
        {
            pending.push((first, line));
        }
        session
    }

    /// Opens a stored session for writing: takes its lock, repairs its
    /// tail, and gives dangling tool calls an "interrupted" result.
    ///
    /// # Errors
    ///
    /// [`SessionError::Busy`] when another process is writing it,
    /// [`SessionError::ReadOnly`] when it was written in a newer format
    /// (open it with [`Log::open`] to read it), and otherwise when it
    /// cannot be read or repaired.
    pub fn resume(
        project: &ProjectSessions,
        id: &SessionId,
        options: SessionOptions,
    ) -> Result<(Self, OpenReport)> {
        let dir = project.session(id).ok_or_else(|| SessionError::NotFound {
            session: id.to_string(),
        })?;
        if fs::symlink_metadata(dir.log()).is_err() {
            return Err(SessionError::NotFound {
                session: id.to_string(),
            });
        }
        let lock = take_lock(&dir)?;
        let (log, mut report) = Log::open(&dir)?;
        log.writable()?;
        let path = dir.log();
        let mut file = OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(SessionError::io("open the session log", path.clone()))?;

        // The tail: a cut line is set aside and cut off, a missing newline
        // is added. Both are flushed before anything else is written.
        let mut end = log.scan.end;
        let flush = options.durability != Durability::Never;
        if let Some(cut) = &log.scan.cut {
            report.cut_saved_to = Some(set_aside(&dir, cut.offset, flush)?);
            file.set_len(cut.offset)
                .and_then(|()| if flush { file.sync_data() } else { Ok(()) })
                .map_err(SessionError::io("repair the session log", path.clone()))?;
            end = cut.offset;
        }
        if log.scan.missing_newline {
            let mut disk = DiskFile(file);
            disk.write_at(end, b"\n")
                .and_then(|()| if flush { disk.sync() } else { Ok(()) })
                .map_err(SessionError::io("repair the session log", path.clone()))?;
            end += 1;
            file = disk.0;
        }

        let store = Arc::new(DiskStore::new(
            dir.clone(),
            Arc::clone(&options.redactor),
            options.durability != Durability::Never,
        ));
        let mut session = Self {
            entry_ids: IdCounter::new("e_").padded(4).after(log.scan.max_entry),
            item_ids: IdCounter::new("i_").after(log.scan.max_item),
            turn_ids: IdCounter::new("t_").after(log.scan.max_turn),
            log,
            state: State::Live {
                writer: Writer::new(Box::new(DiskFile(file)), path, end, options.durability),
                lock,
            },
            store,
            warnings: Vec::new(),
            max_line: MAX_LINE,
            options,
        };
        session.log.scan.end = end;
        session.log.scan.missing_newline = false;
        session.log.scan.cut = None;
        report.repaired = session.repair()?;
        session.check_privacy();
        report.not_private.clone_from(&session.warnings);
        Ok((session, report))
    }

    /// The session's artifacts and stored contents.
    #[must_use]
    pub fn store(&self) -> Arc<DiskStore> {
        Arc::clone(&self.store)
    }

    /// The log, for reading.
    #[must_use]
    pub fn log(&self) -> &Log {
        &self.log
    }

    /// Whether the log exists on disk yet.
    #[must_use]
    pub fn is_born(&self) -> bool {
        matches!(self.state, State::Live { .. })
    }

    /// Files and directories of the session that other users can open.
    /// Empty when everything is private.
    #[must_use]
    pub fn not_private(&self) -> &[PathBuf] {
        &self.warnings
    }

    /// The id for the next item, after every item in the log and every id
    /// this session made. For an owner that does not make its own ids
    /// ([`Log::last_ids`] says who does).
    pub fn next_item_id(&mut self) -> ItemId {
        self.item_ids = self.item_ids.clone().after(
            self.item_ids
                .peek()
                .saturating_sub(1)
                .max(self.log.scan.max_item),
        );
        self.item_ids.next_id()
    }

    /// The id for the next turn, after every turn in the log and every id
    /// this session made.
    pub fn next_turn_id(&mut self) -> TurnId {
        self.turn_ids = self.turn_ids.clone().after(
            self.turn_ids
                .peek()
                .saturating_sub(1)
                .max(self.log.scan.max_turn),
        );
        self.turn_ids.next_id()
    }

    /// Appends an entry, made in `turn` if it was made in one, and returns
    /// its id. A session record goes outside the tree; anything else
    /// continues the active branch.
    ///
    /// The entry is in the session only if it is on disk: when this fails,
    /// nothing changed, in memory or in the file.
    ///
    /// # Errors
    ///
    /// When the entry is one a caller may not append (the first line, a
    /// `rewind`, something read and not understood), or the write fails.
    pub fn append(&mut self, turn: Option<&TurnId>, body: Body) -> Result<EntryId> {
        self.append_marked(turn, body, false)
    }

    /// As [`append`](Self::append), for an entry the caller altered before
    /// storing it: it is written with the mark that the redactor's own
    /// changes get (`"redacted": true`), so that a resume knows the log
    /// does not hold what the model was sent or wrote
    /// ([`Entry::redacted`]).
    ///
    /// # Errors
    ///
    /// As for [`append`](Self::append).
    pub fn append_altered(&mut self, turn: Option<&TurnId>, body: Body) -> Result<EntryId> {
        self.append_marked(turn, body, true)
    }

    fn append_marked(
        &mut self,
        turn: Option<&TurnId>,
        body: Body,
        altered: bool,
    ) -> Result<EntryId> {
        match &body {
            Body::Session(_)
            | Body::Rewind(_)
            | Body::Unknown { .. }
            | Body::Unreadable { .. }
            | Body::LeaseRefused(_) => {
                return Err(SessionError::Invalid(format!(
                    "a `{}` entry cannot be appended",
                    body.kind().as_str()
                )));
            }
            _ => {}
        }
        let parent = if body.is_record() {
            None
        } else {
            self.position()
        };
        self.push(turn, body, parent, altered)
    }

    /// Appends a protocol item.
    ///
    /// # Errors
    ///
    /// As for [`append`](Self::append), and for a kind of item this build
    /// does not know.
    pub fn append_item(&mut self, turn: Option<&TurnId>, item: Item) -> Result<EntryId> {
        let body = Body::from_item(item)
            .ok_or_else(|| SessionError::Invalid("an item of an unknown kind".to_owned()))?;
        self.append(turn, body)
    }

    /// The end of a turn: flushes the log for [`Durability::Turn`], and
    /// brings the session's index record up to date.
    ///
    /// # Errors
    ///
    /// When the flush fails. The entries stay: they were written, and
    /// whether they reach the disk is the operating system's now.
    pub fn end_turn(&mut self) -> Result<()> {
        if let State::Live { writer, .. } = &mut self.state {
            writer.flush()?;
            self.update_index();
        }
        Ok(())
    }

    /// Closes the session: flushes, updates the index, and lets go of the
    /// lock.
    ///
    /// # Errors
    ///
    /// When the flush fails.
    pub fn close(mut self) -> Result<()> {
        self.end_turn()
    }

    /// Where the next tree entry goes: the active position.
    fn position(&self) -> Option<EntryId> {
        match &self.state {
            State::Live { .. } => self.log.active().cloned(),
            State::Unborn { pending } => pending
                .iter()
                .rev()
                .find(|(entry, _)| !entry.body.is_record() || entry.id == EntryId::header())
                .map(|(entry, _)| entry.id.clone()),
        }
    }

    fn push(
        &mut self,
        turn: Option<&TurnId>,
        body: Body,
        parent: Option<EntryId>,
        altered: bool,
    ) -> Result<EntryId> {
        if let State::Live { writer, .. } = &self.state
            && writer.is_broken()
        {
            return Err(SessionError::Broken);
        }
        let mut ids = self.entry_ids.clone();
        let entry = Entry {
            id: ids.next_id(),
            parent,
            ts: self.options.clock.now().to_timestamp(),
            turn: turn.cloned(),
            redacted: altered,
            body,
        };
        let line = entry::encode(&entry, &*self.options.redactor)?;
        // A line the scanner would skip as damage must not be written: it
        // would be in memory now and gone, with the branch after it, when
        // the log is read again.
        let length = line.len() - 1;
        if length > self.max_line {
            return Err(SessionError::EntryTooLarge {
                bytes: length as u64,
                limit: self.max_line as u64,
            });
        }
        let id = entry.id.clone();
        match &mut self.state {
            State::Live { writer, .. } => {
                // A lease is flushed whatever the durability: a crash must
                // not bring back a lease that had ended or been narrowed.
                let offset = if is_lease(&entry.body) {
                    writer.append_flushed(&line)?
                } else {
                    writer.append(&line)?
                };
                self.entry_ids = ids;
                self.log.add(&entry, offset, length);
            }
            State::Unborn { pending } => {
                let is_user = matches!(entry.body, Body::User(_));
                pending.push((entry, line));
                self.entry_ids = ids;
                if is_user && let Err(error) = self.birth() {
                    if let State::Unborn { pending } = &mut self.state {
                        pending.pop();
                    }
                    return Err(error);
                }
            }
        }
        Ok(id)
    }

    /// Creates the log, with every entry held so far, in one atomic write.
    fn birth(&mut self) -> Result<()> {
        let State::Unborn { pending } = &self.state else {
            return Ok(());
        };
        let dir = self.log.dir.clone();
        let created_dir = fs::symlink_metadata(dir.path()).is_err();
        private::create_dir_all(dir.path()).map_err(SessionError::io(
            "create the session's directory",
            dir.path().to_path_buf(),
        ))?;
        let lock = take_lock(&dir)?;
        let content: Vec<u8> = pending.iter().flat_map(|(_, line)| line.clone()).collect();
        let path = dir.log();
        let options = AtomicWriteOptions {
            owner_only: true,
            sync: self.options.durability != Durability::Never
                || pending.iter().any(|(entry, _)| is_lease(&entry.body)),
            ..AtomicWriteOptions::default()
        };
        let written = write_atomic(&path, &content, &options, &*self.options.clock)
            .map_err(|error| SessionError::Io {
                action: "create the session log",
                path: path.clone(),
                source: io::Error::other(error.to_string()),
            })
            .and_then(|()| {
                let writer = OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .map_err(SessionError::io("open the session log", path.clone()))?;
                let reader = File::open(&path)
                    .map_err(SessionError::io("open the session log", path.clone()))?;
                Ok((writer, reader))
            });
        let (writer, reader) = match written {
            Ok(files) => files,
            Err(error) => {
                drop(lock);
                if created_dir {
                    let _ = fs::remove_file(dir.lock());
                    let _ = fs::remove_dir(dir.path());
                }
                return Err(error);
            }
        };
        let State::Unborn { pending } = std::mem::replace(
            &mut self.state,
            State::Live {
                writer: Writer::new(
                    Box::new(DiskFile(writer)),
                    path,
                    content.len() as u64,
                    self.options.durability,
                ),
                lock,
            },
        ) else {
            return Ok(());
        };
        self.log.reader = Some(reader);
        self.store.born();
        let mut offset = 0u64;
        for (entry, line) in pending {
            self.log.add(&entry, offset, line.len() - 1);
            offset += line.len() as u64;
        }
        self.check_privacy();
        self.update_index();
        Ok(())
    }

    /// Gives the tool calls at the end of the active branch that have no
    /// result an "interrupted" one, and says which, with the ids it made.
    fn repair(&mut self) -> Result<Vec<Repaired>> {
        let Some((turn, calls)) = self.log.dangling_calls()? else {
            return Ok(Vec::new());
        };
        let mut repaired = Vec::new();
        for (call_id, tool) in calls {
            let item = self.next_item_id();
            let result = ToolResultItem {
                id: item.clone(),
                call_id: call_id.clone(),
                tool,
                outcome: ToolOutcome::Interrupted,
                content: vec![Part::text(INTERRUPTED)],
                payload: None,
                artifacts: Vec::new(),
                duration_ms: None,
                created_at: Some(self.options.clock.now().to_timestamp()),
            };
            let entry = self.append(turn.as_ref(), Body::ToolResult(result))?;
            repaired.push(Repaired {
                call: call_id,
                item,
                entry,
            });
        }
        Ok(repaired)
    }

    fn check_privacy(&mut self) {
        let dir = &self.log.dir;
        self.warnings = [dir.path().to_path_buf(), dir.log(), dir.lock()]
            .into_iter()
            .filter(|path| fs::symlink_metadata(path).is_ok())
            .filter(|path| !private::is_private(path).unwrap_or(false))
            .collect();
    }

    fn update_index(&self) {
        // The index is a cache: a failure here costs a slower listing, and
        // the next listing repairs it.
        if let Ok(record) = self.log.index_record() {
            let _ = self.log.dir.project().index().update(record);
        }
    }

    // ------------------------------------------------------------ rewinding

    /// Works out a rewind to `to`, an entry of the active branch: the turns
    /// it undoes, and what it would do to each file they changed.
    ///
    /// # Errors
    ///
    /// When `to` is not on the active branch, or the files cannot be read.
    pub fn plan_rewind(&self, to: &EntryId) -> Result<RewindPlan> {
        self.plan(to, RewindReason::Rewind)
    }

    /// Works out an undo of the last turn: a rewind to the entry before it
    /// began. `None` when the active branch has no turn.
    ///
    /// # Errors
    ///
    /// When the files cannot be read.
    pub fn plan_undo(&self) -> Result<Option<RewindPlan>> {
        let branch = self.log.branch();
        let Some(last_turn) = branch
            .iter()
            .rev()
            .find_map(|id| self.log.turn_of(id).cloned())
        else {
            return Ok(None);
        };
        let Some(first) = branch
            .iter()
            .find(|id| self.log.turn_of(id) == Some(&last_turn))
        else {
            return Ok(None);
        };
        let Some(before) = self.log.parent_of(first).cloned() else {
            return Ok(None);
        };
        self.plan(&before, RewindReason::Undo).map(Some)
    }

    fn plan(&self, to: &EntryId, reason: RewindReason) -> Result<RewindPlan> {
        let undone = self.undone(to)?;
        let root = self.workspace_root()?;
        let blocked = !undone.unreadable.is_empty();
        let files = undone
            .changes
            .into_iter()
            .map(|change| snapshot::file_status(&self.store, &root, change, blocked))
            .collect::<Result<Vec<FileRestore>>>()?;
        Ok(RewindPlan {
            to: to.clone(),
            reason,
            from: self.log.active().cloned(),
            turns: undone.turns,
            files,
            unreadable: undone.unreadable,
            session: self.log.id().clone(),
        })
    }

    /// What a rewind to `to` undoes, read from the log: the turns, and for
    /// each file the undone rounds changed, the earliest `before` (what to
    /// go back to) and the latest `after` (what the harness left). And the
    /// checkpoints among them that cannot be read: damaged, or written in
    /// a newer format, so that what they changed is not known.
    fn undone(&self, to: &EntryId) -> Result<Undone> {
        let branch = self.log.branch();
        let Some(position) = branch.iter().position(|id| id == to) else {
            return Err(SessionError::Invalid(format!(
                "`{to}` is not on the active branch"
            )));
        };
        let root = self.workspace_root()?;
        let mut turns: Vec<TurnId> = Vec::new();
        let mut changes: Vec<snapshot::Change> = Vec::new();
        let mut unreadable: Vec<EntryId> = Vec::new();
        // By the file's comparison form, so two spellings of one file in
        // different rounds are one file.
        let mut by_key: HashMap<String, usize> = HashMap::new();
        for id in &branch[position + 1..] {
            if let Some(turn) = self.log.turn_of(id)
                && !turns.contains(turn)
            {
                turns.push(turn.clone());
            }
            let is_checkpoint = match self.log.kind_of(id) {
                Some(Kind::Checkpoint) => true,
                // A newer format's line is read as a kind this build does
                // not know, whatever it is called.
                Some(Kind::Unknown(name)) => **name == *Kind::Checkpoint.as_str(),
                _ => false,
            };
            if !is_checkpoint {
                continue;
            }
            let Body::Checkpoint(Checkpoint { files }) = self.log.read(id)?.body else {
                unreadable.push(id.clone());
                continue;
            };
            for change in files {
                let key = snapshot::locate(&root, &change.path)?.key().to_owned();
                match by_key.get(&key) {
                    Some(&index) => changes[index].2 = change.after,
                    None => {
                        by_key.insert(key, changes.len());
                        changes.push((change.path, change.before, change.after));
                    }
                }
            }
        }
        Ok(Undone {
            turns,
            changes,
            unreadable,
        })
    }

    fn workspace_root(&self) -> Result<AbsPath> {
        AbsPath::parse(&self.log.header.workspace, PathStyle::NATIVE).map_err(|error| {
            SessionError::Invalid(format!("the session's workspace cannot be read: {error}"))
        })
    }

    /// Carries out a rewind: puts files back as `restore` says, moves the
    /// active position, records each undone turn's outcome, and gives any
    /// tool call left without a result at the new position an
    /// "interrupted" one.
    ///
    /// The plan is checked against the log first: it must have been made
    /// for this session, at its active position, and what it would do is
    /// worked out again from the log's own checkpoints, so a plan that was
    /// changed, or made for another session, is refused rather than
    /// followed. Each file's state is looked at again too: one changed
    /// since the plan was made is treated as it is now.
    ///
    /// # Errors
    ///
    /// When the plan is not one for this session at its active position, or
    /// the log cannot be written. A file that cannot be put back is
    /// reported in [`RewindDone::not_restored`], not as an error.
    pub fn rewind(&mut self, plan: &RewindPlan, restore: &Restore) -> Result<RewindDone> {
        if plan.session != *self.log.id() {
            return Err(SessionError::Invalid(format!(
                "the rewind was planned for the session `{}`, not `{}`",
                plan.session,
                self.log.id()
            )));
        }
        if plan.from.as_ref() != self.log.active() {
            return Err(SessionError::Invalid(
                "the session has moved on since the rewind was planned".to_owned(),
            ));
        }
        // From the log, not from the plan: `to` must be on the active
        // branch, and the plan must say what the log says.
        let Undone {
            turns,
            changes,
            unreadable,
        } = self.undone(&plan.to)?;
        if turns != plan.turns || changes != plan.changes() || unreadable != plan.unreadable {
            return Err(SessionError::Invalid(
                "the rewind plan does not match the session's checkpoints; plan it again"
                    .to_owned(),
            ));
        }
        let root = self.workspace_root()?;
        let mut restored = Vec::new();
        let mut not_restored = Vec::new();
        if restore.files {
            let overwrite: BTreeSet<&str> = restore
                .overwrite_edited
                .iter()
                .map(String::as_str)
                .collect();
            let blocked = !unreadable.is_empty();
            for change in changes {
                let file = snapshot::file_status(&self.store, &root, change, blocked)?;
                let why_not = match file.status {
                    FileStatus::AlreadyRestored => None,
                    FileStatus::CannotRestore => file.why_not.map(str::to_owned),
                    FileStatus::EditedSince if !overwrite.contains(file.path.as_str()) => {
                        Some("it was changed after the harness left it".to_owned())
                    }
                    FileStatus::AsLeft | FileStatus::EditedSince => {
                        match snapshot::put_back(&self.store, &root, &file, &*self.options.clock) {
                            Ok(()) => None,
                            Err(error) => Some(error.to_string()),
                        }
                    }
                };
                match why_not {
                    None => restored.push(file.path),
                    Some(reason) => not_restored.push((file.path, reason)),
                }
            }
        }
        let entry = self.move_to(&plan.to, plan.reason.clone())?;
        let reverted = if plan.reason == RewindReason::Undo {
            Reverted::Undone
        } else {
            Reverted::Rewound
        };
        for turn in &turns {
            self.append(
                Some(turn),
                Body::Outcome(Outcome {
                    reverted: Some(reverted.clone()),
                    ..Outcome::default()
                }),
            )?;
        }
        let repaired = self.repair()?;
        Ok(RewindDone {
            entry,
            restored,
            not_restored,
            repaired,
        })
    }

    /// Makes another branch active: the one that ends at `to`. Files are
    /// not touched. Tool calls at `to` that have no result are given an
    /// "interrupted" one, with item ids the store makes, and reported.
    ///
    /// # Errors
    ///
    /// When `to` is not an entry of the tree, or the log cannot be written.
    pub fn switch(&mut self, to: &EntryId) -> Result<SwitchDone> {
        let entry = self.move_to(to, RewindReason::Switch)?;
        let repaired = self.repair()?;
        Ok(SwitchDone { entry, repaired })
    }

    /// Appends the `rewind` entry that makes `to` the active position.
    fn move_to(&mut self, to: &EntryId, reason: RewindReason) -> Result<EntryId> {
        if !self.log.contains(to) || self.log.is_record(to) {
            return Err(SessionError::Invalid(format!(
                "`{to}` is not an entry of the conversation"
            )));
        }
        let from = self.log.active().cloned();
        self.push(
            None,
            Body::Rewind(Rewind { reason, from }),
            Some(to.clone()),
            false,
        )
    }
}

impl Log {
    /// Forks the session: a new session in the same project whose log
    /// begins with a copy of the branch that ends at `at` (the active one
    /// when `at` is `None`) and the session's base state. Leases, outcomes
    /// and metadata are not copied: the new session records its own lease.
    /// The original is not changed. The artifacts and stored contents are
    /// shared by hard links where the file system allows, copied otherwise.
    ///
    /// # Errors
    ///
    /// When the session was written in a newer format, `at` is not an
    /// entry of the tree, or the new session cannot be written.
    pub fn fork(
        &self,
        at: Option<&EntryId>,
        id: SessionId,
        options: SessionOptions,
    ) -> Result<(Session, OpenReport)> {
        self.writable()?;
        let end = match at {
            Some(at) if self.contains(at) && !self.is_record(at) => at.clone(),
            Some(at) => {
                return Err(SessionError::Invalid(format!(
                    "`{at}` is not an entry of the conversation"
                )));
            }
            None => self
                .active()
                .cloned()
                .ok_or_else(|| SessionError::Invalid("the session is empty".to_owned()))?,
        };
        let project = self.dir.project();
        let target = project
            .session(&id)
            .ok_or_else(|| SessionError::Invalid(format!("`{id}` cannot be a session id")))?;
        if fs::symlink_metadata(target.path()).is_ok() {
            return Err(SessionError::Invalid(format!(
                "the session `{id}` exists already"
            )));
        }

        // What to copy, in the order it was written.
        let mut copied: Vec<u32> = self
            .branch_of(&end)
            .iter()
            .filter_map(|id| self.index_of(id))
            .filter(|&index| index != 0)
            .collect();
        copied.extend(self.record_indices(&Kind::Base));
        copied.sort_unstable();

        let header = Entry {
            id: EntryId::header(),
            parent: None,
            ts: options.clock.now().to_timestamp(),
            turn: None,
            redacted: false,
            body: Body::Session(Header {
                session: id.clone(),
                workspace: self.header.workspace.clone(),
                harness: options.harness.clone(),
                forked_from: Some(ForkOrigin {
                    session: self.header.session.clone(),
                    entry: end,
                }),
            }),
        };
        let mut content = entry::encode(&header, &*options.redactor)?;
        for index in copied {
            content.extend(self.raw_line(self.node_at(index))?);
            content.push(b'\n');
        }

        private::create_dir_all(target.path()).map_err(SessionError::io(
            "create the session's directory",
            target.path().to_path_buf(),
        ))?;
        share_dir(&self.dir.artifacts(), &target.artifacts())?;
        share_dir(&self.dir.checkpoints(), &target.checkpoints())?;
        let path = target.log();
        let write = AtomicWriteOptions {
            owner_only: true,
            sync: options.durability != Durability::Never,
            ..AtomicWriteOptions::default()
        };
        write_atomic(&path, &content, &write, &*options.clock).map_err(|error| {
            SessionError::Io {
                action: "create the session log",
                path: path.clone(),
                source: io::Error::other(error.to_string()),
            }
        })?;
        let (session, report) = Session::resume(&project, &id, options)?;
        session.update_index();
        Ok((session, report))
    }
}

/// What a rewind undoes, read from the log.
struct Undone {
    turns: Vec<TurnId>,
    changes: Vec<snapshot::Change>,
    unreadable: Vec<EntryId>,
}

/// Whether an entry is a lease record, which is flushed whatever the
/// durability.
fn is_lease(body: &Body) -> bool {
    matches!(body, Body::Lease(_))
}

/// Takes the session's lock, creating the lock file if it is missing.
fn take_lock(dir: &SessionDir) -> Result<File> {
    lock::try_take(&dir.lock())?.ok_or_else(|| SessionError::Busy {
        session: dir.id().to_string(),
    })
}

/// Keeps the bytes of a cut last line in a private file of their own,
/// before the log is cut back.
fn set_aside(dir: &SessionDir, offset: u64, flush: bool) -> Result<PathBuf> {
    let path = dir.log();
    let source =
        File::open(&path).map_err(SessionError::io("read the session log", path.clone()))?;
    let length = source
        .metadata()
        .map_err(SessionError::io("read the session log", path.clone()))?
        .len();
    let bytes = crate::file::read_at(
        &source,
        offset,
        usize::try_from(length - offset).unwrap_or(0),
    )
    .map_err(SessionError::io("read the session log", path.clone()))?;
    let mut attempt = 0u32;
    loop {
        let aside = if attempt == 0 {
            dir.cut(offset)
        } else {
            dir.path().join(format!("log.cut-{offset}-{attempt}"))
        };
        match private::create_new(&aside) {
            Ok(mut file) => {
                file.write_all(&bytes)
                    .and_then(|()| if flush { file.sync_all() } else { Ok(()) })
                    .map_err(SessionError::io("set aside the cut line", aside.clone()))?;
                return Ok(aside);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt < 100 => {
                attempt += 1;
            }
            Err(error) => return Err(SessionError::io("set aside the cut line", aside)(error)),
        }
    }
}

/// Makes every file of `from` available in `to`: a hard link, or a private
/// copy where links are not possible. Temporary files are left out.
fn share_dir(from: &std::path::Path, to: &std::path::Path) -> Result<()> {
    let entries = match fs::read_dir(from) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(SessionError::io("list", from.to_path_buf())(error)),
    };
    private::create_dir_all(to).map_err(SessionError::io("create", to.to_path_buf()))?;
    for found in entries.flatten() {
        let name = found.file_name();
        let Some(text) = name.to_str() else { continue };
        if text.starts_with('.') || text.ends_with(".part") {
            continue;
        }
        if !found.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let source = found.path();
        let target = to.join(&name);
        if fs::hard_link(&source, &target).is_ok() {
            continue;
        }
        let mut input = File::open(&source).map_err(SessionError::io("read", source.clone()))?;
        let mut output =
            private::create_new(&target).map_err(SessionError::io("copy to", target.clone()))?;
        io::copy(&mut input, &mut output)
            .and_then(|_| output.sync_all())
            .map_err(SessionError::io("copy to", target))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use cortex_base::FakeClock;
    use cortex_protocol::ids::CallId;
    use cortex_protocol::item::{AssistantMessageItem, NoticeItem, NoticeSource, UserMessageItem};
    use cortex_protocol::message::ToolCallPart;
    use cortex_testkit::cases::{self, ensure};
    use cortex_testkit::{Rng, SeededRng};
    use serde_json::json;

    use super::*;
    use crate::entry::Meta;
    use crate::file::fake::{FakeDisk, Faults};
    use crate::scan::Scan;

    /// A directory of its own for one test, removed with it.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "cortex-session-unit-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn options(durability: Durability) -> SessionOptions {
        SessionOptions {
            durability,
            ..SessionOptions::new(Arc::new(FakeClock::default()), "0.0.0")
        }
    }

    fn user(text: &str) -> Body {
        Body::User(UserMessageItem {
            id: ItemId::new(format!("i_{text}")),
            parts: vec![Part::text(text)],
            delivery: None,
            created_at: None,
        })
    }

    fn body(rng: &mut SeededRng, n: u64) -> Body {
        match rng.below(4) {
            0 => user(&format!("prompt {n} é")),
            1 => Body::Assistant {
                item: AssistantMessageItem {
                    id: ItemId::new(format!("i_{n}")),
                    parts: vec![
                        Part::text("x".repeat(cases::small(rng, 40))),
                        Part::ToolCall(ToolCallPart {
                            id: CallId::new(format!("c_{n}")),
                            name: "read".into(),
                            arguments: json!({"path": "a"}),
                            raw: None,
                        }),
                    ],
                    stop_reason: None,
                    raw_stop: None,
                    stop_detail: None,
                    truncated: false,
                    origin: None,
                    usage: None,
                    error: None,
                    created_at: None,
                },
                request_id: None,
            },
            2 => Body::Notice(NoticeItem {
                id: ItemId::new(format!("i_{n}")),
                source: NoticeSource::Harness,
                text: "note".into(),
                created_at: None,
            }),
            _ => Body::Meta(Meta {
                title: Some(format!("title {n}")),
                ..Meta::default()
            }),
        }
    }

    /// A born session whose writes go to a fake disk that starts with what
    /// the real file holds.
    fn on_fake_disk(scratch: &Scratch, durability: Durability) -> (Session, FakeDisk) {
        let project = ProjectSessions::at(scratch.0.join("project"));
        let root = AbsPath::parse(&scratch.0.to_string_lossy(), PathStyle::NATIVE).unwrap();
        let mut session =
            Session::create(&project, SessionId::new("s_1"), &root, options(durability)).unwrap();
        // Born on the fake disk: the held entries are written there, as
        // the real birth writes them to a new file, and nothing touches the
        // real disk but the stand-in for the lock.
        let State::Unborn { pending } = std::mem::replace(
            &mut session.state,
            State::Unborn {
                pending: Vec::new(),
            },
        ) else {
            panic!("a new session is unborn")
        };
        let disk = FakeDisk::default();
        let mut writer = Writer::new(disk.file(), session.log.dir.log(), 0, durability);
        for (entry, line) in pending {
            let offset = writer.append(&line).unwrap();
            session.log.add(&entry, offset, line.len() - 1);
        }
        let stand_in = scratch.0.join("lock");
        if !stand_in.exists() {
            fs::write(&stand_in, b"").unwrap();
        }
        session.state = State::Live {
            writer,
            lock: File::open(&stand_in).unwrap(),
        };
        session.append(None, user("first")).unwrap();
        (session, disk)
    }

    /// The tree in memory, as comparable values.
    type Shape = (String, Option<u32>, String, Option<String>, u64, u32, bool);

    fn shape(nodes: &[crate::scan::Node]) -> Vec<Shape> {
        nodes
            .iter()
            .map(|node| {
                (
                    node.id.to_string(),
                    node.parent,
                    node.kind.as_str().to_owned(),
                    node.turn.as_ref().map(ToString::to_string),
                    node.offset,
                    node.len,
                    node.record,
                )
            })
            .collect()
    }

    fn agree(session: &Session, disk: &FakeDisk) -> cases::Outcome {
        let bytes = disk.contents();
        let scan = Scan::read(bytes.as_slice()).unwrap().unwrap();
        ensure!(
            scan.cut.is_none(),
            "the disk has a cut line: {:?}",
            scan.cut
        );
        ensure!(scan.damaged.is_empty(), "the disk has damage");
        ensure!(
            shape(&scan.nodes) == shape(&session.log.scan.nodes),
            "memory and disk disagree"
        );
        ensure!(scan.end == session.log.scan.end, "the ends differ");
        Ok(())
    }

    #[test]
    fn a_write_that_fails_at_any_byte_changes_neither_memory_nor_disk() {
        let scratch = Scratch::new();
        for durability in [Durability::Item, Durability::Turn, Durability::Never] {
            let (mut session, disk) = on_fake_disk(&scratch, durability);
            let line_len = {
                let probe = entry::encode(
                    &Entry {
                        id: EntryId::new("e_0002"),
                        parent: session.active().cloned(),
                        ts: session.options.clock.now().to_timestamp(),
                        turn: None,
                        redacted: false,
                        body: user("second"),
                    },
                    &NoRedaction,
                )
                .unwrap();
                probe.len()
            };
            for after in 0..=line_len {
                disk.fail(Faults {
                    write_fails_after: Some(after),
                    ..Faults::default()
                });
                let before = disk.contents();
                assert!(session.append(None, user("second")).is_err());
                assert_eq!(disk.contents(), before, "{durability:?}, {after} bytes");
                agree(&session, &disk).unwrap();
            }
            // A flush that fails takes the line back out.
            if durability == Durability::Item {
                disk.fail(Faults {
                    sync_fails: true,
                    ..Faults::default()
                });
                assert!(session.append(None, user("second")).is_err());
                agree(&session, &disk).unwrap();
            }
            let id = session.append(None, user("second")).unwrap();
            assert_eq!(session.active(), Some(&id));
            agree(&session, &disk).unwrap();
        }
    }

    #[test]
    fn memory_and_disk_agree_after_any_sequence_of_failures() {
        let scratch = Scratch::new();
        cases::check(
            "failed_writes_agree",
            150,
            |rng| {
                let steps: Vec<(u64, u64, u64)> = (0..cases::small(rng, 24))
                    .map(|_| (rng.next_u64(), rng.below(12), rng.below(400)))
                    .collect();
                steps
            },
            |steps| {
                let (mut session, disk) = on_fake_disk(&scratch, Durability::Item);
                for (n, &(seed, fault, at)) in steps.iter().enumerate() {
                    let mut rng = SeededRng::new(seed);
                    let body = body(&mut rng, n as u64);
                    let faults = match fault {
                        0 | 1 => Faults {
                            write_fails_after: Some(usize::try_from(at).unwrap()),
                            ..Faults::default()
                        },
                        2 => Faults {
                            sync_fails: true,
                            ..Faults::default()
                        },
                        3 => Faults {
                            write_fails_after: Some(usize::try_from(at).unwrap()),
                            truncate_fails: true,
                            ..Faults::default()
                        },
                        _ => Faults::default(),
                    };
                    disk.fail(faults);
                    let before_nodes = shape(&session.log.scan.nodes);
                    let result = session.append(None, body);
                    let broken =
                        matches!(&session.state, State::Live { writer, .. } if writer.is_broken());
                    if result.is_err() {
                        ensure!(
                            shape(&session.log.scan.nodes) == before_nodes,
                            "a failed append changed memory"
                        );
                    }
                    if broken {
                        // The file is no longer vouched for: nothing more is
                        // written, and opening it again repairs it.
                        ensure!(result.is_err(), "a broken session accepted a write");
                        let scan = Scan::read(disk.contents().as_slice()).unwrap().unwrap();
                        ensure!(
                            shape(&scan.nodes).starts_with(&before_nodes),
                            "the disk lost entries memory has"
                        );
                        return Ok(());
                    }
                    agree(&session, &disk)?;
                }
                Ok(())
            },
        );
    }

    fn lease() -> Body {
        Body::Lease(crate::entry::LeaseRecord {
            event: crate::entry::LeaseEvent::Ended,
            lease: cortex_protocol::lease::Lease::from_wire(json!({
                "id": "l_1",
                "grant": {"level": "supervised", "sandbox": {"read": {"all": true}, "write": {"roots": ["/w"]}, "network": "off"}},
                "expires": {"kind": "end_of_session"},
                "granted_by": {"kind": "user"}
            }))
            .unwrap(),
        })
    }

    #[test]
    fn a_lease_is_flushed_whatever_the_durability() {
        let scratch = Scratch::new();
        for durability in [Durability::Item, Durability::Turn, Durability::Never] {
            let (mut session, disk) = on_fake_disk(&scratch, durability);
            // The next flush fails. Only a write that flushes sees it.
            disk.fail(Faults {
                sync_fails: true,
                ..Faults::default()
            });
            let before = disk.contents();
            let flushed = session.append(None, lease()).is_err();
            assert!(flushed, "{durability:?}: the lease was not flushed");
            assert_eq!(disk.contents(), before, "{durability:?}");
            agree(&session, &disk).unwrap();
            session.append(None, lease()).unwrap();
            agree(&session, &disk).unwrap();
        }
    }

    /// Gives its bytes one at a time, so that the scanner looks at the
    /// length of every line before its newline arrives, as it does for a
    /// line that spans its reads of a real file.
    struct Trickle<'a>(&'a [u8]);

    impl io::Read for Trickle<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = self.0.len().min(buffer.len()).min(1);
            buffer[..count].copy_from_slice(&self.0[..count]);
            self.0 = &self.0[count..];
            Ok(count)
        }
    }

    #[test]
    fn the_longest_line_a_session_writes_is_the_longest_the_reader_takes() {
        let scratch = Scratch::new();
        let (mut session, disk) = on_fake_disk(&scratch, Durability::Never);
        let meta = |length: usize| {
            Body::Meta(Meta {
                title: Some("x".repeat(length)),
                ..Meta::default()
            })
        };
        // A limit longer than every line already written (the first holds
        // the workspace's path, which is long on Windows): the line of a
        // title longer than all of them.
        let longest = disk
            .contents()
            .split(|&byte| byte == b'\n')
            .map(<[u8]>::len)
            .max()
            .unwrap_or(0);
        let title = longest + 100;
        let probe = Entry {
            id: EntryId::new("e_0002"),
            parent: None,
            ts: session.options.clock.now().to_timestamp(),
            turn: None,
            redacted: false,
            body: meta(title),
        };
        let limit = entry::encode(&probe, &NoRedaction).unwrap().len() - 1;
        session.max_line = limit;

        let before = disk.contents();
        let refused = session.append(None, meta(title + 1));
        assert!(
            matches!(refused, Err(SessionError::EntryTooLarge { bytes, limit: most })
                if bytes == limit as u64 + 1 && most == limit as u64),
            "{refused:?}"
        );
        assert_eq!(disk.contents(), before, "nothing was written");
        let kept = session.append(None, meta(title)).unwrap();
        agree(&session, &disk).unwrap();
        let scan = Scan::read_limited(Trickle(&disk.contents()), limit)
            .unwrap()
            .unwrap();
        assert!(scan.damaged.is_empty(), "{:?}", scan.damaged);
        assert!(scan.by_id.contains_key(&kept));

        // One byte longer is a line the reader skips as damage.
        let mut longer = disk.contents();
        longer.extend(
            entry::encode(
                &Entry {
                    id: EntryId::new("e_0003"),
                    body: meta(title + 1),
                    ..probe
                },
                &NoRedaction,
            )
            .unwrap(),
        );
        let scan = Scan::read_limited(Trickle(&longer), limit)
            .unwrap()
            .unwrap();
        assert_eq!(scan.damaged.len(), 1);
    }

    #[test]
    fn a_broken_session_does_not_report_its_turn_as_flushed() {
        let scratch = Scratch::new();
        for durability in [Durability::Item, Durability::Turn, Durability::Never] {
            let (mut session, disk) = on_fake_disk(&scratch, durability);
            session.end_turn().unwrap();
            disk.fail(Faults {
                write_fails_after: Some(3),
                truncate_fails: true,
                ..Faults::default()
            });
            assert!(session.append(None, user("lost")).is_err());
            assert!(
                matches!(session.end_turn(), Err(SessionError::Broken)),
                "{durability:?}"
            );
        }
    }

    #[test]
    fn nothing_is_on_disk_until_the_first_user_message() {
        let scratch = Scratch::new();
        let project = ProjectSessions::at(scratch.0.join("project"));
        let root = AbsPath::parse(&scratch.0.to_string_lossy(), PathStyle::NATIVE).unwrap();
        let mut session = Session::create(
            &project,
            SessionId::new("s_1"),
            &root,
            options(Durability::Item),
        )
        .unwrap();
        session
            .append(
                None,
                Body::Meta(Meta {
                    title: Some("t".into()),
                    ..Meta::default()
                }),
            )
            .unwrap();
        assert!(!session.is_born());
        assert!(
            fs::symlink_metadata(project.path()).is_err(),
            "nothing was created"
        );
        assert!(session.is_empty());
        let id = session.append(None, user("hello")).unwrap();
        assert!(session.is_born());
        assert_eq!(session.len(), 3);
        assert_eq!(session.active(), Some(&id));
        assert_eq!(session.parent_of(&id), Some(&EntryId::header()));
        assert_eq!(session.title().unwrap().as_deref(), Some("t"));
    }
}
