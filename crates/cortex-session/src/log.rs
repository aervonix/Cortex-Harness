//! A session log, read: the tree, the active branch, and entries read from
//! the file when they are asked for.
//!
//! [`Log::open`] takes no lock and never changes the file, whatever it
//! finds. Another process may be writing the session, and a line it is in
//! the middle of writing looks exactly like a line a crash cut off. Only
//! [`Session::resume`](crate::Session::resume), which holds the lock,
//! repairs a log.

use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io;
use std::path::PathBuf;

use cortex_protocol::error::ErrorData;
use cortex_protocol::ids::{CallId, ItemId, ModelId, SessionId, Timestamp, TurnId};
use cortex_protocol::item::Item;
use cortex_protocol::lease::Lease;
use cortex_protocol::message::Part;

use crate::base::BaseState;
use crate::entry::{self, Body, Entry, EntryId, FORMAT_VERSION, Header, Kind, LeaseEvent};
use crate::error::{Result, SessionError};
use crate::file;
use crate::index::IndexRecord;
use crate::layout::SessionDir;
use crate::scan::{CutTail, Node, Scan};

/// What opening a log found, for the diagnostics to show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenReport {
    /// A last line that a crash (or a writer still at work) cut off. It was
    /// not read.
    pub cut_tail: Option<CutTail>,
    /// When a resume set the cut bytes aside before repairing the log:
    /// where they are.
    pub cut_saved_to: Option<PathBuf>,
    /// Line numbers (from 1) of lines that are not entries. They were
    /// skipped.
    pub damaged_lines: Vec<u64>,
    /// Entries whose parent is not an earlier entry: the branch through
    /// them stops there.
    pub broken_links: Vec<EntryId>,
    /// Kinds this build does not know, with how many of each. Skipped.
    pub unknown_kinds: BTreeMap<String, u32>,
    /// Lines written in a newer format version. Skipped.
    pub newer_entries: u32,
    /// Why the session cannot be written to, when it cannot.
    pub read_only: Option<String>,
    /// Tool calls that had no result, for which a resume appended an
    /// "interrupted" result.
    pub repaired_calls: Vec<CallId>,
    /// Files and directories of the session that other users can open: on
    /// a file system without permissions, or made private by hand and
    /// opened up since.
    pub not_private: Vec<PathBuf>,
}

impl OpenReport {
    /// Whether anything is worth telling the user.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self == &Self::default()
    }
}

/// A session log, opened for reading.
#[derive(Debug)]
pub struct Log {
    pub(crate) dir: SessionDir,
    pub(crate) header: Header,
    pub(crate) created: Timestamp,
    pub(crate) scan: Scan,
    /// The file, for reading entries. `None` before the log exists.
    pub(crate) reader: Option<File>,
    pub(crate) read_only: Option<String>,
}

impl Log {
    /// Opens a session's log for reading. Takes no lock and changes
    /// nothing, on disk or anywhere else.
    ///
    /// # Errors
    ///
    /// When there is no log, it cannot be read, or it is not a session log.
    pub fn open(dir: &SessionDir) -> Result<(Self, OpenReport)> {
        let path = dir.log();
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(SessionError::NotFound {
                    session: dir.id().to_string(),
                });
            }
            Err(error) => return Err(SessionError::io("open the session log", path)(error)),
        };
        let scan = Scan::read(&file)
            .map_err(SessionError::io("read the session log", path.clone()))?
            .map_err(|not_a_log| SessionError::NotALog {
                path: path.clone(),
                reason: not_a_log.0,
            })?;
        let first = scan.nodes.first().ok_or_else(|| SessionError::NotALog {
            path: path.clone(),
            reason: "it is empty".to_owned(),
        })?;
        let line = file::read_at(&file, first.offset, first.len as usize)
            .map_err(SessionError::io("read the session log", path.clone()))?;
        let first = entry::decode(&line).map_err(|error| SessionError::NotALog {
            path: path.clone(),
            reason: error.0,
        })?;
        let header = match first.body {
            Body::Session(header) => header,
            Body::Unknown { fields, .. } if scan.version > FORMAT_VERSION => {
                // A newer format: read what can be read of the first line, to
                // show the session, and write nothing.
                serde_json::from_value(serde_json::Value::Object(fields)).map_err(|error| {
                    SessionError::NotALog {
                        path: path.clone(),
                        reason: error.to_string(),
                    }
                })?
            }
            _ => {
                return Err(SessionError::NotALog {
                    path,
                    reason: "its first line cannot be read".to_owned(),
                });
            }
        };
        if header.session != *dir.id() {
            return Err(SessionError::NotALog {
                path,
                reason: format!(
                    "it belongs to the session `{}`, not `{}`",
                    header.session,
                    dir.id()
                ),
            });
        }
        let read_only = (scan.version > FORMAT_VERSION).then(|| {
            format!(
                "it was written in format version {}, and this version of Cortex-Harness \
                 writes version {FORMAT_VERSION}",
                scan.version
            )
        });
        let report = OpenReport {
            cut_tail: scan.cut.clone(),
            damaged_lines: scan.damaged.clone(),
            broken_links: scan.broken_links.clone(),
            unknown_kinds: scan.unknown_kinds.clone(),
            newer_entries: scan.newer,
            read_only: read_only.clone(),
            ..OpenReport::default()
        };
        let log = Self {
            dir: dir.clone(),
            header,
            created: first.ts,
            scan,
            reader: Some(file),
            read_only,
        };
        Ok((log, report))
    }

    /// A log that does not exist on disk yet.
    pub(crate) fn unborn(dir: SessionDir, header: Header, created: Timestamp) -> Self {
        Self {
            dir,
            header,
            created,
            scan: Scan {
                version: FORMAT_VERSION,
                ..Scan::default()
            },
            reader: None,
            read_only: None,
        }
    }

    /// The session's directory.
    #[must_use]
    pub fn dir(&self) -> &SessionDir {
        &self.dir
    }

    /// The session.
    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.header.session
    }

    /// The first line: the session, its workspace, where it was forked from.
    #[must_use]
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// When the session was created.
    #[must_use]
    pub fn created(&self) -> &Timestamp {
        &self.created
    }

    /// When the session was last written to.
    #[must_use]
    pub fn updated(&self) -> &Timestamp {
        self.scan.last_ts.as_ref().unwrap_or(&self.created)
    }

    /// Whether the session can be written to, and why not.
    ///
    /// # Errors
    ///
    /// [`SessionError::ReadOnly`] for a session written in a newer format.
    pub fn writable(&self) -> Result<()> {
        match &self.read_only {
            None => Ok(()),
            Some(reason) => Err(SessionError::ReadOnly {
                session: self.id().to_string(),
                reason: reason.clone(),
            }),
        }
    }

    /// How many entries the log has, the first line included.
    #[must_use]
    pub fn len(&self) -> usize {
        self.scan.nodes.len()
    }

    /// Whether the log has no entries: true only before it exists on disk.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.scan.nodes.is_empty()
    }

    /// How many bytes of the file are whole entries.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.scan.end
    }

    /// Where the id counters stand: the highest turn number (`t_N`) and the
    /// highest item number (`i_N`) in the log, 0 for none.
    ///
    /// **While a session is open, its owner makes the turn and item ids,**
    /// starting after these. The agent loop hands out a turn id the moment
    /// a prompt is queued, on the caller's thread, and must not wait for the
    /// session behind a flush. The one time the store makes an item id is
    /// when it repairs (on resume, on a rewind or a switch): resume does so
    /// before it returns, so read these after it, and a rewind or a switch
    /// reports the results it added.
    #[must_use]
    pub fn last_ids(&self) -> (u64, u64) {
        (self.scan.max_turn, self.scan.max_item)
    }

    /// Whether the log has an entry with this id.
    #[must_use]
    pub fn contains(&self, id: &EntryId) -> bool {
        self.scan.by_id.contains_key(id)
    }

    /// The kind of an entry.
    #[must_use]
    pub fn kind_of(&self, id: &EntryId) -> Option<&Kind> {
        self.node(id).map(|node| &node.kind)
    }

    /// The turn an entry was made in.
    #[must_use]
    pub fn turn_of(&self, id: &EntryId) -> Option<&TurnId> {
        self.node(id).and_then(|node| node.turn.as_ref())
    }

    /// The parent of an entry.
    #[must_use]
    pub fn parent_of(&self, id: &EntryId) -> Option<&EntryId> {
        let node = self.node(id)?;
        node.parent
            .map(|parent| &self.scan.nodes[parent as usize].id)
    }

    /// Whether an entry is a session record, outside the conversation tree.
    #[must_use]
    pub fn is_record(&self, id: &EntryId) -> bool {
        self.node(id).is_some_and(|node| node.record)
    }

    fn node(&self, id: &EntryId) -> Option<&Node> {
        self.scan
            .by_id
            .get(id)
            .map(|&index| &self.scan.nodes[index as usize])
    }

    /// The index of the active position: the last tree entry in the file.
    pub(crate) fn active_index(&self) -> Option<u32> {
        self.scan
            .nodes
            .iter()
            .rposition(|node| !node.record)
            .and_then(|index| u32::try_from(index).ok())
    }

    /// The active position: the last entry of the active branch. `None`
    /// only before the log exists.
    #[must_use]
    pub fn active(&self) -> Option<&EntryId> {
        self.active_index()
            .map(|index| &self.scan.nodes[index as usize].id)
    }

    pub(crate) fn branch_indices(&self, from: u32) -> Vec<u32> {
        let mut path = Vec::new();
        let mut current = Some(from);
        while let Some(index) = current {
            path.push(index);
            current = self.scan.nodes[index as usize].parent;
        }
        path.reverse();
        path
    }

    /// The active branch: the entries from the first line to the active
    /// position, in order.
    #[must_use]
    pub fn branch(&self) -> Vec<EntryId> {
        self.active_index()
            .map(|active| self.ids(&self.branch_indices(active)))
            .unwrap_or_default()
    }

    /// The branch that ends at `id`, from the first line. Empty for an id
    /// the log does not have, and for a session record.
    #[must_use]
    pub fn branch_of(&self, id: &EntryId) -> Vec<EntryId> {
        match self.scan.by_id.get(id) {
            Some(&index) if !self.scan.nodes[index as usize].record => {
                self.ids(&self.branch_indices(index))
            }
            _ => Vec::new(),
        }
    }

    /// The tree entries no other entry continues: the ends of the branches,
    /// in the order they were written.
    #[must_use]
    pub fn leaves(&self) -> Vec<EntryId> {
        let mut has_child = vec![false; self.scan.nodes.len()];
        for node in &self.scan.nodes {
            if let Some(parent) = node.parent {
                has_child[parent as usize] = true;
            }
        }
        self.scan
            .nodes
            .iter()
            .zip(has_child)
            .filter(|(node, has_child)| !node.record && !has_child)
            .map(|(node, _)| node.id.clone())
            .collect()
    }

    fn ids(&self, indices: &[u32]) -> Vec<EntryId> {
        indices
            .iter()
            .map(|&index| self.scan.nodes[index as usize].id.clone())
            .collect()
    }

    /// The entry that holds an item.
    #[must_use]
    pub fn entry_of_item(&self, item: &ItemId) -> Option<&EntryId> {
        self.scan
            .by_item
            .get(item)
            .map(|&index| &self.scan.nodes[index as usize].id)
    }

    /// Reads one entry from the file.
    ///
    /// # Errors
    ///
    /// When the log has no such entry, or the file cannot be read.
    pub fn read(&self, id: &EntryId) -> Result<Entry> {
        let index = *self
            .scan
            .by_id
            .get(id)
            .ok_or_else(|| SessionError::NoSuchEntry {
                entry: id.to_string(),
            })?;
        self.read_index(index)
    }

    pub(crate) fn read_index(&self, index: u32) -> Result<Entry> {
        let node = &self.scan.nodes[index as usize];
        let line = self.raw_line(node)?;
        entry::decode(&line).map_err(|error| SessionError::NotALog {
            path: self.dir.log(),
            reason: format!("the entry `{}` cannot be read again: {}", node.id, error.0),
        })
    }

    /// The bytes of an entry's line, without its newline.
    pub(crate) fn raw_line(&self, node: &Node) -> Result<Vec<u8>> {
        let reader = self
            .reader
            .as_ref()
            .ok_or_else(|| SessionError::NoSuchEntry {
                entry: node.id.to_string(),
            })?;
        file::read_at(reader, node.offset, node.len as usize)
            .map_err(SessionError::io("read the session log", self.dir.log()))
    }

    pub(crate) fn node_at(&self, index: u32) -> &Node {
        &self.scan.nodes[index as usize]
    }

    /// Reads the entries of the active branch, from the first line to the
    /// active position.
    ///
    /// # Errors
    ///
    /// When the file cannot be read.
    pub fn read_branch(&self) -> Result<Vec<Entry>> {
        match self.active_index() {
            Some(active) => self
                .branch_indices(active)
                .into_iter()
                .map(|index| self.read_index(index))
                .collect(),
            None => Ok(Vec::new()),
        }
    }

    /// The items of the active branch, in order: what a request's history is
    /// made from.
    ///
    /// # Errors
    ///
    /// When the file cannot be read.
    pub fn items(&self) -> Result<Vec<Item>> {
        let Some(active) = self.active_index() else {
            return Ok(Vec::new());
        };
        let mut items = Vec::new();
        for index in self.branch_indices(active) {
            if self.scan.nodes[index as usize].kind.holds_item()
                && let Some(item) = self.read_index(index)?.body.as_item()
            {
                items.push(item);
            }
        }
        Ok(items)
    }

    /// The session records of one kind, in the order they were written.
    ///
    /// # Errors
    ///
    /// When the file cannot be read.
    pub fn records(&self, kind: &Kind) -> Result<Vec<Entry>> {
        self.scan
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.record && &node.kind == kind)
            .map(|(index, _)| self.read_index(u32::try_from(index).unwrap_or(u32::MAX)))
            .collect()
    }

    /// The lease in force: the one granted or extended last that has not
    /// ended. `None` when no lease is recorded.
    ///
    /// # Errors
    ///
    /// The refusal, with the code `lease_not_understood`, when **any** lease
    /// record is one this build does not understand in full. The leases are
    /// then unknown, and a lease is never read in part, so none is given.
    /// Reading the file can fail too, as an internal error.
    pub fn lease_in_force(&self) -> std::result::Result<Option<Lease>, Box<ErrorData>> {
        let records = self
            .records(&Kind::Lease)
            .map_err(|error| Box::new(cortex_base::Classified::to_error_data(&error)))?;
        let mut in_force: Vec<Lease> = Vec::new();
        for record in records {
            match record.body {
                Body::Lease(record) => {
                    in_force.retain(|lease| lease.id != record.lease.id);
                    if record.event != LeaseEvent::Ended {
                        in_force.push(record.lease);
                    }
                }
                Body::LeaseRefused(refusal) => return Err(refusal),
                _ => {}
            }
        }
        Ok(in_force.pop())
    }

    /// The workspace's starting state: the latest `base` record. A later
    /// one completes an earlier one, as when a project is trusted after the
    /// session started.
    ///
    /// # Errors
    ///
    /// When the file cannot be read.
    pub fn base(&self) -> Result<Option<BaseState>> {
        Ok(self
            .records(&Kind::Base)?
            .into_iter()
            .filter_map(|entry| match entry.body {
                Body::Base(base) => Some(base),
                _ => None,
            })
            .next_back())
    }

    /// The session's title: from the latest `meta` record that sets one.
    ///
    /// # Errors
    ///
    /// When the file cannot be read.
    pub fn title(&self) -> Result<Option<String>> {
        for (index, node) in self.scan.nodes.iter().enumerate().rev() {
            if node.kind == Kind::Meta
                && let Body::Meta(meta) = self.read_index(u32::try_from(index).unwrap_or(0))?.body
                && meta.title.is_some()
            {
                return Ok(meta.title);
            }
        }
        Ok(None)
    }

    /// The tool calls at the end of the active branch that have no result:
    /// a crash during a tool round, or a rewind to a response that asked
    /// for tools. With the turn of the response, and the calls' names.
    pub(crate) fn dangling_calls(&self) -> Result<Option<Dangling>> {
        let Some(active) = self.active_index() else {
            return Ok(None);
        };
        let mut answered = HashSet::new();
        for index in self.branch_indices(active).into_iter().rev() {
            let node = &self.scan.nodes[index as usize];
            match node.kind {
                Kind::ToolResult => {
                    if let Body::ToolResult(result) = self.read_index(index)?.body {
                        answered.insert(result.call_id);
                    }
                }
                Kind::Assistant => {
                    let entry = self.read_index(index)?;
                    let Body::Assistant { item, .. } = entry.body else {
                        return Ok(None);
                    };
                    let missing: Vec<(CallId, String)> = item
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            Part::ToolCall(call) if !answered.contains(&call.id) => {
                                Some((call.id.clone(), call.name.clone()))
                            }
                            _ => None,
                        })
                        .collect();
                    return Ok((!missing.is_empty()).then_some((entry.turn, missing)));
                }
                // Anything that can only come after a round is complete.
                Kind::User | Kind::Notice | Kind::Summary | Kind::Epoch | Kind::Session => {
                    return Ok(None);
                }
                _ => {}
            }
        }
        Ok(None)
    }

    /// What the session index keeps about this session.
    ///
    /// # Errors
    ///
    /// When the file cannot be read.
    pub fn index_record(&self) -> Result<IndexRecord> {
        let first_prompt = match self
            .scan
            .nodes
            .iter()
            .position(|node| node.kind == Kind::User)
        {
            Some(index) => match self.read_index(u32::try_from(index).unwrap_or(0))?.body {
                Body::User(item) => Some(prompt_text(&item.parts)),
                _ => None,
            },
            None => None,
        };
        let mut model: Option<ModelId> = None;
        if let Some(index) = self
            .scan
            .nodes
            .iter()
            .rposition(|node| node.kind == Kind::Epoch)
            && let Body::Epoch(epoch) = self.read_index(u32::try_from(index).unwrap_or(0))?.body
        {
            model = Some(epoch.model);
        }
        let turns: HashSet<&TurnId> = self
            .scan
            .nodes
            .iter()
            .filter_map(|node| node.turn.as_ref())
            .collect();
        Ok(IndexRecord {
            id: self.id().clone(),
            workspace: self.header.workspace.clone(),
            title: self.title()?,
            first_prompt,
            created: self.created.clone(),
            updated: self.updated().clone(),
            entries: self.scan.nodes.len() as u64,
            turns: turns.len() as u64,
            parent: self
                .header
                .forked_from
                .as_ref()
                .map(|origin| origin.session.clone()),
            model,
            bytes: self.scan.end,
        })
    }

    /// The ids every entry has, by index, for the writer's bookkeeping.
    pub(crate) fn index_of(&self, id: &EntryId) -> Option<u32> {
        self.scan.by_id.get(id).copied()
    }

    /// Takes in an entry the writer has just put on disk.
    pub(crate) fn add(&mut self, entry: &Entry, offset: u64, len: usize) {
        let index = u32::try_from(self.scan.nodes.len()).unwrap_or(u32::MAX);
        let parent = entry
            .parent
            .as_ref()
            .and_then(|parent| self.scan.by_id.get(parent).copied());
        let item = entry
            .body
            .as_item()
            .and_then(|item| item.id().map(ItemId::new));
        if let Some(item) = &item {
            self.scan.max_item = self
                .scan
                .max_item
                .max(entry::number_after(item.as_str(), "i_").unwrap_or(0));
            self.scan.by_item.insert(item.clone(), index);
        }
        if let Some(turn) = &entry.turn {
            self.scan.max_turn = self
                .scan
                .max_turn
                .max(entry::number_after(turn.as_str(), "t_").unwrap_or(0));
        }
        self.scan.max_entry = self.scan.max_entry.max(entry.id.number().unwrap_or(0));
        let record = entry.parent.is_none() && index != 0;
        self.scan.by_id.insert(entry.id.clone(), index);
        self.scan.nodes.push(Node {
            id: entry.id.clone(),
            parent,
            kind: entry.body.kind(),
            turn: entry.turn.clone(),
            offset,
            len: u32::try_from(len).unwrap_or(u32::MAX),
            record,
        });
        self.scan.last_ts = Some(entry.ts.clone());
        self.scan.end = offset + len as u64 + 1;
    }

    /// The ids of entries that are session records of a kind.
    pub(crate) fn record_indices(&self, kind: &Kind) -> Vec<u32> {
        self.scan
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.record && &node.kind == kind)
            .filter_map(|(index, _)| u32::try_from(index).ok())
            .collect()
    }
}

/// Tool calls without a result: the turn of the response that made them,
/// and each call's id and tool name.
pub(crate) type Dangling = (Option<TurnId>, Vec<(CallId, String)>);

/// The text of a prompt, for a listing: the text parts, on one line, cut
/// to 200 characters.
fn prompt_text(parts: &[Part]) -> String {
    let mut text = String::new();
    for part in parts {
        if let Part::Text(part) = part {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&part.text);
        }
    }
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match one_line.char_indices().nth(200) {
        Some((cut, _)) => format!("{}…", &one_line[..cut]),
        None => one_line,
    }
}
