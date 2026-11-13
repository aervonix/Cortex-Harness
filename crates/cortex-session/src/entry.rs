//! The entries of a session log, and their form on disk
//! (`docs/architecture/07-sessions-and-context.md`, "The log").
//!
//! One JSON object a line. Every entry has the same envelope:
//!
//! ```json
//! {"v":1,"id":"e_0007","parent":"e_0006","ts":"2026-11-12T10:15:02Z","turn":"t_3","kind":"assistant", "...": "the kind's own fields"}
//! ```
//!
//! - `v` is the format version the line was written in.
//! - `id` counts up: `e_0000` is the first line (`kind: "session"`), the
//!   root of the tree, and every other entry is `e_0001` onwards.
//! - `parent` makes the log a tree. An entry with `parent: null` after the
//!   first line is a **session record**, outside the conversation tree:
//!   the base state, a lease, metadata, usage outside the loop, a turn's
//!   outcome, an extension's record, an escalation and a decision. Records
//!   belong to the session, not to a branch, so a rewind never hides them.
//! - The **active position** is the last tree entry in the file. A rewind,
//!   an undo and a switch of branch each append a `rewind` entry whose
//!   parent is the entry moved to, which makes it the last tree entry.
//! - `turn`, when present, is the turn the entry was made in.
//!
//! Reading is tolerant: a kind this build does not know, and a field it
//! does not know, are skipped, and kept as they are so that a fork copies
//! them. **A lease is the exception** (`docs/decisions/0028-enforced-values-fail-closed.md`):
//! its record is read through the protocol's strict decoder, and one this
//! build does not understand in full is refused whole, never read in part.

use std::fmt;

use cortex_base::ContentHash;
use cortex_protocol::error::{ErrorData, ErrorKind as WireKind, code};
use cortex_protocol::event::{TurnCompleted, TurnOutcome, Verification};
use cortex_protocol::ids::{ArtifactId, ItemId, ModelId, ProviderId, SessionId, Timestamp, TurnId};
use cortex_protocol::item::{
    AssistantMessageItem, Item, NoticeItem, StopReason, SummaryItem, ToolResultItem,
    UserMessageItem,
};
use cortex_protocol::lease::Lease;
use cortex_protocol::message::Part;
use cortex_protocol::usage::{RequestUsage, UsageTotals};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::base::BaseState;
use crate::redact::{self, Redactor};

/// The format version this build writes, and the newest it reads in full.
/// A session whose first line has a newer one opens read-only.
pub const FORMAT_VERSION: u64 = 1;

/// The id of the first line, the root of the tree.
pub(crate) const HEADER_ID: &str = "e_0000";

/// The id of an entry, unique within its session: `e_0007`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryId(String);

impl EntryId {
    /// Wraps a string.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The id as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The number in an id this build made: 7 for `e_0007`.
    #[must_use]
    pub fn number(&self) -> Option<u64> {
        number_after(&self.0, "e_")
    }

    /// The id of the first line.
    #[must_use]
    pub fn header() -> Self {
        Self(HEADER_ID.to_owned())
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for EntryId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&str> for EntryId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

/// The number after `prefix` in an id such as `t_12`, when the rest is
/// digits.
pub(crate) fn number_after(id: &str, prefix: &str) -> Option<u64> {
    let digits = id.strip_prefix(prefix)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// What sort of entry a line is, from its `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Kind {
    /// The first line.
    Session,
    /// The workspace's starting state.
    Base,
    /// The start of a context epoch.
    Epoch,
    /// A user message.
    User,
    /// An assistant message.
    Assistant,
    /// One tool result.
    ToolResult,
    /// A message the harness added to the conversation.
    Notice,
    /// A compaction summary.
    Summary,
    /// A failed or abandoned request.
    Attempt,
    /// Usage outside the main loop.
    Usage,
    /// The files a tool round changed.
    Checkpoint,
    /// How a turn ended, or that it was later undone.
    Outcome,
    /// Title, labels, notes.
    Meta,
    /// An extension's record.
    Ext,
    /// A question for the user (reserved).
    Escalation,
    /// A Manager's decision (reserved).
    Decision,
    /// A lease granted, extended or ended.
    Lease,
    /// The active position moved.
    Rewind,
    /// A kind this build does not know.
    Unknown(Box<str>),
}

impl Kind {
    /// Every kind this build knows, as written.
    pub const KNOWN: [&'static str; 18] = [
        "session",
        "base",
        "epoch",
        "user",
        "assistant",
        "tool_result",
        "notice",
        "summary",
        "attempt",
        "usage",
        "checkpoint",
        "outcome",
        "meta",
        "ext",
        "escalation",
        "decision",
        "lease",
        "rewind",
    ];

    /// The kind a line names.
    #[must_use]
    pub fn from_name(name: &str) -> Self {
        match name {
            "session" => Self::Session,
            "base" => Self::Base,
            "epoch" => Self::Epoch,
            "user" => Self::User,
            "assistant" => Self::Assistant,
            "tool_result" => Self::ToolResult,
            "notice" => Self::Notice,
            "summary" => Self::Summary,
            "attempt" => Self::Attempt,
            "usage" => Self::Usage,
            "checkpoint" => Self::Checkpoint,
            "outcome" => Self::Outcome,
            "meta" => Self::Meta,
            "ext" => Self::Ext,
            "escalation" => Self::Escalation,
            "decision" => Self::Decision,
            "lease" => Self::Lease,
            "rewind" => Self::Rewind,
            other => Self::Unknown(other.into()),
        }
    }

    /// The kind as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Session => "session",
            Self::Base => "base",
            Self::Epoch => "epoch",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::ToolResult => "tool_result",
            Self::Notice => "notice",
            Self::Summary => "summary",
            Self::Attempt => "attempt",
            Self::Usage => "usage",
            Self::Checkpoint => "checkpoint",
            Self::Outcome => "outcome",
            Self::Meta => "meta",
            Self::Ext => "ext",
            Self::Escalation => "escalation",
            Self::Decision => "decision",
            Self::Lease => "lease",
            Self::Rewind => "rewind",
            Self::Unknown(name) => name,
        }
    }

    /// Whether entries of this kind hold one of the protocol's items: what
    /// is sent to the model.
    #[must_use]
    pub fn holds_item(&self) -> bool {
        matches!(
            self,
            Self::User | Self::Assistant | Self::ToolResult | Self::Notice | Self::Summary
        )
    }
}

/// One entry of a session log.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Its id.
    pub id: EntryId,
    /// The entry before it in the conversation. `None` for the first line
    /// and for a session record.
    pub parent: Option<EntryId>,
    /// When it was written.
    pub ts: Timestamp,
    /// The turn it was made in, when it was made in one.
    pub turn: Option<TurnId>,
    /// What it holds.
    pub body: Body,
}

/// What an entry holds, by kind.
#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "an entry is made and written one at a time; the log keeps no bodies in memory"
)]
pub enum Body {
    /// The first line.
    Session(Header),
    /// The workspace's starting state.
    Base(BaseState),
    /// The start of a context epoch.
    Epoch(Epoch),
    /// A user message.
    User(UserMessageItem),
    /// An assistant message, and the provider's id for the request.
    Assistant {
        /// The message.
        item: AssistantMessageItem,
        /// The provider's id for the request that produced it.
        request_id: Option<String>,
    },
    /// One tool result.
    ToolResult(ToolResultItem),
    /// A message the harness added to the conversation.
    Notice(NoticeItem),
    /// A compaction summary, and what was carried across mechanically.
    Summary {
        /// The summary.
        item: SummaryItem,
        /// The carried state: the plan, the files read and changed, the last
        /// prompt, recent artifact references. Its shape is the agent's.
        carried: Value,
    },
    /// A failed or abandoned request, with any partial output.
    Attempt(Attempt),
    /// Usage outside the main loop: compaction, titles.
    Usage(UsageRecord),
    /// The files one tool round changed.
    Checkpoint(Checkpoint),
    /// How a turn ended, or that it was later undone or rewound.
    Outcome(Outcome),
    /// Title, labels, notes.
    Meta(Meta),
    /// An extension's record.
    Ext(ExtRecord),
    /// A question waiting for the user, or its answer. Reserved for the
    /// Coordinator.
    Escalation(Escalation),
    /// Something a Manager decided without asking. Reserved for the
    /// Coordinator.
    Decision(Decision),
    /// A lease granted, extended or ended, understood in full.
    Lease(LeaseRecord),
    /// A lease record this build does not understand in full. It is never
    /// read in part: the error is the refusal, with the code
    /// `lease_not_understood`.
    LeaseRefused(Box<ErrorData>),
    /// The active position moved to this entry's parent.
    Rewind(Rewind),
    /// A kind this build does not know, or a line written in a newer format
    /// version: skipped, and kept as it is.
    Unknown {
        /// The kind as written.
        kind: String,
        /// Every field besides the envelope.
        fields: Map<String, Value>,
    },
    /// A kind this build knows whose fields could not be read. Skipped, like
    /// an unknown kind, and reported.
    Unreadable {
        /// The kind as written.
        kind: String,
        /// What was wrong.
        reason: String,
    },
}

impl Body {
    /// The kind it is written as.
    #[must_use]
    pub fn kind(&self) -> Kind {
        match self {
            Self::Session(_) => Kind::Session,
            Self::Base(_) => Kind::Base,
            Self::Epoch(_) => Kind::Epoch,
            Self::User(_) => Kind::User,
            Self::Assistant { .. } => Kind::Assistant,
            Self::ToolResult(_) => Kind::ToolResult,
            Self::Notice(_) => Kind::Notice,
            Self::Summary { .. } => Kind::Summary,
            Self::Attempt(_) => Kind::Attempt,
            Self::Usage(_) => Kind::Usage,
            Self::Checkpoint(_) => Kind::Checkpoint,
            Self::Outcome(_) => Kind::Outcome,
            Self::Meta(_) => Kind::Meta,
            Self::Ext(_) => Kind::Ext,
            Self::Escalation(_) => Kind::Escalation,
            Self::Decision(_) => Kind::Decision,
            Self::Lease(_) | Self::LeaseRefused(_) => Kind::Lease,
            Self::Rewind(_) => Kind::Rewind,
            Self::Unknown { kind, .. } | Self::Unreadable { kind, .. } => Kind::from_name(kind),
        }
    }

    /// Whether an entry of this kind is a session record, outside the
    /// conversation tree, rather than a step of the conversation.
    #[must_use]
    pub fn is_record(&self) -> bool {
        matches!(
            self,
            Self::Base(_)
                | Self::Usage(_)
                | Self::Outcome(_)
                | Self::Meta(_)
                | Self::Ext(_)
                | Self::Escalation(_)
                | Self::Decision(_)
                | Self::Lease(_)
                | Self::LeaseRefused(_)
        )
    }

    /// The body that stores a protocol item. `None` for a kind of item this
    /// build does not know.
    #[must_use]
    pub fn from_item(item: Item) -> Option<Self> {
        Some(match item {
            Item::UserMessage(item) => Self::User(item),
            Item::AssistantMessage(item) => Self::Assistant {
                item,
                request_id: None,
            },
            Item::ToolResult(item) => Self::ToolResult(item),
            Item::Notice(item) => Self::Notice(item),
            Item::Summary(item) => Self::Summary {
                item,
                carried: Value::Null,
            },
            Item::Unknown(_) => return None,
        })
    }

    /// The protocol item this body stores, if it stores one.
    #[must_use]
    pub fn as_item(&self) -> Option<Item> {
        Some(match self {
            Self::User(item) => Item::UserMessage(item.clone()),
            Self::Assistant { item, .. } => Item::AssistantMessage(item.clone()),
            Self::ToolResult(item) => Item::ToolResult(item.clone()),
            Self::Notice(item) => Item::Notice(item.clone()),
            Self::Summary { item, .. } => Item::Summary(item.clone()),
            _ => return None,
        })
    }
}

/// The first line of a session log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Header {
    /// The session.
    pub session: SessionId,
    /// The workspace root, as an absolute path with `/` between names.
    pub workspace: String,
    /// The version of Cortex-Harness that created the session.
    pub harness: String,
    /// Where the session was forked from, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from: Option<ForkOrigin>,
}

/// Where a forked session came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkOrigin {
    /// The session it was forked from.
    pub session: SessionId,
    /// The last entry of that session that the fork copied.
    pub entry: EntryId,
}

/// The start of a context epoch: what the request prefix is for every
/// request in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Epoch {
    /// The provider.
    pub provider: ProviderId,
    /// The wire dialect.
    pub protocol: String,
    /// The model.
    pub model: ModelId,
    /// The reasoning setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The system prompt.
    pub system: String,
    /// The tool definitions, in order.
    #[serde(default)]
    pub tools: Vec<Value>,
    /// A hash of the system prompt and the tool definitions together.
    pub hash: ContentHash,
}

impl Epoch {
    /// The hash an epoch records for a system prompt and tool list.
    #[must_use]
    pub fn hash_of(system: &str, tools: &[Value]) -> ContentHash {
        let mut hasher = cortex_base::ContentHasher::new();
        hasher.update(&(system.len() as u64).to_le_bytes());
        hasher.update(system.as_bytes());
        for tool in tools {
            let text = tool.to_string();
            hasher.update(&(text.len() as u64).to_le_bytes());
            hasher.update(text.as_bytes());
        }
        hasher.finish()
    }
}

/// A failed or abandoned request attempt, kept for debugging. Never sent
/// to the model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Attempt {
    /// The item the response would have been.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    /// Which attempt of the request it was, from 1.
    #[serde(default)]
    pub attempt: u32,
    /// The error it ended with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorData>,
    /// What had arrived of the response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
}

/// Usage of a request made outside the main loop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    /// What the request was for: `compaction`, `title`.
    pub purpose: String,
    /// What it used and cost.
    pub usage: RequestUsage,
}

/// The files one tool round changed. The checkpoints of a turn are its
/// snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// One change a file, in the order they were first touched.
    pub files: Vec<FileChange>,
}

/// How one file changed in a tool round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// The file: relative to the workspace root with `/` between names, or
    /// an absolute path for a file outside it.
    pub path: String,
    /// What the file was before the round. A line without it is read as
    /// [`Before::Unknown`], never as "did not exist".
    #[serde(default)]
    pub before: Before,
    /// The file as the round left it. `null`: the round removed it.
    #[serde(default)]
    pub after: Option<FileVersion>,
}

/// What a file was before a tool round changed it.
///
/// On disk: `null` for [`Absent`](Self::Absent), a version object for
/// [`Version`](Self::Version), and the string `"unknown"` for
/// [`Unknown`](Self::Unknown). A missing `before`, and any other string,
/// read as `Unknown`: only an explicit `null` says that a file did not
/// exist, because undoing that change removes the file. A build from
/// before `"unknown"` existed cannot read such a checkpoint at all, and so
/// skips it rather than undo it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Before {
    /// The file did not exist. Undoing the change removes it.
    Absent,
    /// This version. Undoing the change puts it back when its content was
    /// kept ([`FileVersion::kept`]).
    Version(FileVersion),
    /// Not known: the change was seen only after it was made, as with a
    /// shell command, and what the file held before could not be
    /// recovered. A rewind cannot put it back, and never removes or
    /// overwrites the file.
    #[default]
    Unknown,
}

impl Before {
    /// The version, when there was a file and it is known.
    #[must_use]
    pub fn version(&self) -> Option<&FileVersion> {
        match self {
            Self::Version(version) => Some(version),
            Self::Absent | Self::Unknown => None,
        }
    }

    /// Whether `now` is this state: no file for [`Absent`](Self::Absent),
    /// the same content for [`Version`](Self::Version). Never for
    /// [`Unknown`](Self::Unknown).
    #[must_use]
    pub fn is(&self, now: Option<&FileVersion>) -> bool {
        match (self, now) {
            (Self::Absent, None) => true,
            (Self::Version(before), Some(now)) => before.hash == now.hash,
            _ => false,
        }
    }
}

impl From<Option<FileVersion>> for Before {
    /// A file that was read: `None` when there was no file.
    fn from(version: Option<FileVersion>) -> Self {
        version.map_or(Self::Absent, Self::Version)
    }
}

impl Serialize for Before {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Absent => serializer.serialize_unit(),
            Self::Version(version) => version.serialize(serializer),
            Self::Unknown => serializer.serialize_str("unknown"),
        }
    }
}

impl<'de> Deserialize<'de> for Before {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BeforeVisitor;

        impl<'de> serde::de::Visitor<'de> for BeforeVisitor {
            type Value = Before;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("null, a file version, or \"unknown\"")
            }

            fn visit_unit<E>(self) -> Result<Before, E> {
                Ok(Before::Absent)
            }

            fn visit_none<E>(self) -> Result<Before, E> {
                Ok(Before::Absent)
            }

            // "unknown", or anything else that is not null or a version: a
            // state a later build added, or damage. Either way not one this
            // build can put back, and not a reason to lose the rest of the
            // checkpoint.
            fn visit_str<E>(self, _: &str) -> Result<Before, E> {
                Ok(Before::Unknown)
            }

            fn visit_bool<E>(self, _: bool) -> Result<Before, E> {
                Ok(Before::Unknown)
            }

            fn visit_i64<E>(self, _: i64) -> Result<Before, E> {
                Ok(Before::Unknown)
            }

            fn visit_u64<E>(self, _: u64) -> Result<Before, E> {
                Ok(Before::Unknown)
            }

            fn visit_f64<E>(self, _: f64) -> Result<Before, E> {
                Ok(Before::Unknown)
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Before, A::Error> {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(Before::Unknown)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Before, A::Error> {
                let value = Value::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(FileVersion::deserialize(value).map_or(Before::Unknown, Before::Version))
            }
        }

        deserializer.deserialize_any(BeforeVisitor)
    }
}

/// One version of a file: its hash, its size, and whether its content is
/// in the checkpoint store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersion {
    /// The content's hash.
    pub hash: ContentHash,
    /// The content's size in bytes.
    pub bytes: u64,
    /// Whether the content is kept in `checkpoints/`. A file over the size
    /// cap is recorded by hash and size only, and cannot be put back.
    #[serde(default = "yes", skip_serializing_if = "is_yes")]
    pub kept: bool,
}

fn yes() -> bool {
    true
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
fn is_yes(value: &bool) -> bool {
    *value
}

/// Defines a string enumeration that keeps a value it does not know.
macro_rules! open_names {
    ($(#[$meta:meta])* pub enum $name:ident { $($(#[$vmeta:meta])* $variant:ident => $text:literal,)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vmeta])* $variant,)*
            /// A value this build does not know, kept as written.
            Other(String),
        }

        impl $name {
            /// The value as written.
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $text,)*
                    Self::Other(text) => text,
                }
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                Ok(match text.as_str() {
                    $($text => Self::$variant,)*
                    _ => Self::Other(text),
                })
            }
        }
    };
}

open_names! {
    /// What later happened to a turn.
    pub enum Reverted {
        /// The user undid it.
        Undone => "undone",
        /// The user rewound to before it, or to a point inside it.
        Rewound => "rewound",
    }
}

open_names! {
    /// Why the active position moved.
    pub enum RewindReason {
        /// A rewind to an earlier entry.
        Rewind => "rewind",
        /// An undo of the last turn.
        Undo => "undo",
        /// A switch to another branch.
        Switch => "switch",
    }
}

/// How a turn ended, or what later happened to it. The turn is the
/// entry's `turn`. A turn has one record when it ends, and one more for
/// each later undo or rewind.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    /// How it ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<TurnOutcome>,
    /// What the project's checks said at its end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification: Option<Verification>,
    /// Why the last response ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// The failure, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorData>,
    /// What the turn used and cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageTotals>,
    /// Milliseconds the turn took.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// That it was later undone or rewound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reverted: Option<Reverted>,
}

impl Outcome {
    /// The record of how a turn ended, from the event that says so. The
    /// turn goes in the entry's `turn`.
    #[must_use]
    pub fn completed(event: &TurnCompleted) -> Self {
        Self {
            ended: Some(event.outcome.clone()),
            verification: event.verification.clone(),
            stop_reason: event.stop_reason.clone(),
            error: event.error.clone(),
            usage: Some(event.usage.clone()),
            duration_ms: event.duration_ms,
            reverted: None,
        }
    }

    /// The record that a turn was later undone or rewound.
    #[must_use]
    pub fn reverted(reverted: Reverted) -> Self {
        Self {
            reverted: Some(reverted),
            ..Self::default()
        }
    }
}

/// Title, labels and notes. A later record replaces what it sets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// The session's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Labels.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    /// A note from the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// State an extension saved, namespaced by the extension's id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtRecord {
    /// The extension.
    pub ext: String,
    /// What it saved. Its shape is the extension's.
    #[serde(default)]
    pub data: Value,
}

/// A question waiting for the user, or the answer to one
/// (`docs/architecture/17-coordinator.md`, "Two records"). Reserved: the
/// first release writes none, and the shape may change before the first
/// use. Its `kind` there is `category` here, because `kind` is the
/// envelope's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Escalation {
    /// Who asks: the Manager, or an Employee by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// What it is about: plan, approval, merge, spend, and so on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    /// References to what the user needs to decide.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub detail: Value,
    /// The choices on offer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// For an answer: the escalation it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<EntryId>,
    /// For an answer: how it was resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// For an answer: the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// For an answer: who answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_by: Option<String>,
}

/// Something a Manager session decided without asking
/// (`docs/architecture/17-coordinator.md`, "Two records"). Reserved, like
/// [`Escalation`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    /// What was decided.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// What it is about, as for an escalation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The Manager's reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What it relied on.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub basis: Value,
    /// The level and the grant clause that allowed it, filled in by the
    /// harness.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub allowed_by: Value,
}

/// What happened to a lease. A closed set: a record with an event this
/// build does not know is refused whole, like an unknown field of a lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseEvent {
    /// The lease was granted. The standing lease is granted when a session
    /// starts.
    Granted,
    /// The lease was extended: these are its new terms.
    Extended,
    /// The lease ended.
    Ended,
}

/// A lease granted, extended or ended, with its exact terms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LeaseRecord {
    /// What happened.
    pub event: LeaseEvent,
    /// The lease's terms, after the event.
    pub lease: Lease,
}

/// A move of the active position. The entry moved to is the `rewind`
/// entry's parent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rewind {
    /// Why it moved.
    pub reason: RewindReason,
    /// The active position before the move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<EntryId>,
}

/// The payload of an entry that stores an item: `{"item": …}`.
#[derive(Serialize, Deserialize)]
struct ItemPayload<T> {
    item: T,
}

#[derive(Serialize, Deserialize)]
struct AssistantPayload {
    item: AssistantMessageItem,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct SummaryPayload {
    item: SummaryItem,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    carried: Value,
}

/// The envelope's keys, which no kind's own fields may use.
const ENVELOPE: [&str; 6] = ["v", "id", "parent", "ts", "turn", "kind"];

/// The kind's own fields of a body, as a JSON object.
fn payload(body: &Body) -> Result<Map<String, Value>, serde_json::Error> {
    fn object<T: Serialize>(value: &T) -> Result<Map<String, Value>, serde_json::Error> {
        match serde_json::to_value(value)? {
            Value::Object(map) => Ok(map),
            _ => Err(serde::ser::Error::custom(
                "an entry's fields must be an object",
            )),
        }
    }
    match body {
        Body::Session(header) => object(header),
        Body::Base(base) => object(base),
        Body::Epoch(epoch) => object(epoch),
        Body::User(item) => object(&ItemPayload { item }),
        Body::Assistant { item, request_id } => object(&AssistantPayload {
            item: item.clone(),
            request_id: request_id.clone(),
        }),
        Body::ToolResult(item) => object(&ItemPayload { item }),
        Body::Notice(item) => object(&ItemPayload { item }),
        Body::Summary { item, carried } => object(&SummaryPayload {
            item: item.clone(),
            carried: carried.clone(),
        }),
        Body::Attempt(attempt) => object(attempt),
        Body::Usage(usage) => object(usage),
        Body::Checkpoint(checkpoint) => object(checkpoint),
        Body::Outcome(outcome) => object(outcome),
        Body::Meta(meta) => object(meta),
        Body::Ext(ext) => object(ext),
        Body::Escalation(escalation) => object(escalation),
        Body::Decision(decision) => object(decision),
        Body::Lease(record) => object(record),
        Body::Rewind(rewind) => object(rewind),
        Body::Unknown { fields, .. } => Ok(fields.clone()),
        Body::LeaseRefused(_) | Body::Unreadable { .. } => Err(serde::ser::Error::custom(
            "an entry that was not understood is never written",
        )),
    }
}

/// Writes an entry as one line, newline included, with the redactor applied
/// to its text. The provider's own blocks (`raw`) are left as they are.
pub(crate) fn encode(entry: &Entry, redactor: &dyn Redactor) -> Result<Vec<u8>, serde_json::Error> {
    let mut fields = payload(&entry.body)?;
    if !redactor.is_noop() {
        for value in fields.values_mut() {
            redact::value(value, redactor);
        }
    }
    let mut line = Map::with_capacity(fields.len() + ENVELOPE.len());
    line.insert("v".to_owned(), Value::from(FORMAT_VERSION));
    line.insert("id".to_owned(), Value::from(entry.id.as_str()));
    line.insert(
        "parent".to_owned(),
        entry
            .parent
            .as_ref()
            .map_or(Value::Null, |parent| Value::from(parent.as_str())),
    );
    line.insert("ts".to_owned(), Value::from(entry.ts.as_str()));
    if let Some(turn) = &entry.turn {
        line.insert("turn".to_owned(), Value::from(turn.as_str()));
    }
    line.insert("kind".to_owned(), Value::from(entry.body.kind().as_str()));
    for (key, value) in fields {
        if ENVELOPE.contains(&key.as_str()) {
            return Err(serde::ser::Error::custom(format_args!(
                "the field `{key}` is the envelope's"
            )));
        }
        line.insert(key, value);
    }
    let mut bytes = serde_json::to_vec(&Value::Object(line))?;
    // Compact JSON escapes every newline inside a string, so the only one
    // is the one that ends the line.
    debug_assert!(!bytes.contains(&b'\n'));
    bytes.push(b'\n');
    Ok(bytes)
}

/// Why a line could not be read as an entry at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotAnEntry(pub(crate) String);

/// Reads one line (without its newline) as an entry.
pub(crate) fn decode(line: &[u8]) -> Result<Entry, NotAnEntry> {
    let mut fields: Map<String, Value> =
        serde_json::from_slice(line).map_err(|error| NotAnEntry(error.to_string()))?;
    let text = |fields: &mut Map<String, Value>, key: &str| match fields.remove(key) {
        Some(Value::String(text)) => Ok(Some(text)),
        None | Some(Value::Null) => Ok(None),
        Some(_) => Err(NotAnEntry(format!("`{key}` is not a string"))),
    };
    let version = fields
        .remove("v")
        .and_then(|value| value.as_u64())
        .ok_or_else(|| NotAnEntry("no format version".to_owned()))?;
    let id = text(&mut fields, "id")?.ok_or_else(|| NotAnEntry("no id".to_owned()))?;
    let parent = text(&mut fields, "parent")?;
    let ts = text(&mut fields, "ts")?.ok_or_else(|| NotAnEntry("no time".to_owned()))?;
    let turn = text(&mut fields, "turn")?;
    let kind = text(&mut fields, "kind")?.ok_or_else(|| NotAnEntry("no kind".to_owned()))?;

    let body = if version <= FORMAT_VERSION {
        decode_body(kind, fields)
    } else if kind == Kind::Lease.as_str() {
        // A newer build's lease: skipping it could leave an older, wider
        // lease in force, so it is refused, never read and never skipped.
        Body::LeaseRefused(lease_refusal(format!(
            "it was written in format version {version}, and this version reads up to \
             {FORMAT_VERSION}"
        )))
    } else {
        Body::Unknown { kind, fields }
    };
    Ok(Entry {
        id: EntryId(id),
        parent: parent.map(EntryId),
        ts: Timestamp::new(ts),
        turn: turn.map(TurnId::new),
        body,
    })
}

fn decode_body(kind: String, fields: Map<String, Value>) -> Body {
    fn read<T: DeserializeOwned>(fields: Map<String, Value>) -> Result<T, String> {
        T::deserialize(Value::Object(fields)).map_err(|error| error.to_string())
    }
    let decoded = match Kind::from_name(&kind) {
        Kind::Session => read(fields).map(Body::Session),
        Kind::Base => read(fields).map(Body::Base),
        Kind::Epoch => read(fields).map(Body::Epoch),
        Kind::User => read::<ItemPayload<_>>(fields).map(|payload| Body::User(payload.item)),
        Kind::Assistant => read::<AssistantPayload>(fields).map(|payload| Body::Assistant {
            item: payload.item,
            request_id: payload.request_id,
        }),
        Kind::ToolResult => {
            read::<ItemPayload<_>>(fields).map(|payload| Body::ToolResult(payload.item))
        }
        Kind::Notice => read::<ItemPayload<_>>(fields).map(|payload| Body::Notice(payload.item)),
        Kind::Summary => read::<SummaryPayload>(fields).map(|payload| Body::Summary {
            item: payload.item,
            carried: payload.carried,
        }),
        Kind::Attempt => read(fields).map(Body::Attempt),
        Kind::Usage => read(fields).map(Body::Usage),
        Kind::Checkpoint => read(fields).map(Body::Checkpoint),
        Kind::Outcome => read(fields).map(Body::Outcome),
        Kind::Meta => read(fields).map(Body::Meta),
        Kind::Ext => read(fields).map(Body::Ext),
        Kind::Escalation => read(fields).map(Body::Escalation),
        Kind::Decision => read(fields).map(Body::Decision),
        Kind::Lease => Ok(match decode_lease(fields) {
            Ok(record) => Body::Lease(record),
            Err(refusal) => Body::LeaseRefused(refusal),
        }),
        Kind::Rewind => read(fields).map(Body::Rewind),
        Kind::Unknown(_) => return Body::Unknown { kind, fields },
    };
    decoded.unwrap_or_else(|reason| Body::Unreadable { kind, reason })
}

/// Reads a lease record strictly: exactly `event` and `lease`, the event
/// one this build knows, and the lease through the protocol's strict
/// decoder. Anything else refuses the record whole.
fn decode_lease(mut fields: Map<String, Value>) -> Result<LeaseRecord, Box<ErrorData>> {
    let refuse = lease_refusal;
    let event = fields
        .remove("event")
        .ok_or_else(|| refuse("it has no `event`".to_owned()))?;
    let event: LeaseEvent = serde_json::from_value(event).map_err(|error| {
        refuse(format!(
            "its `event` is not one this version knows ({error})"
        ))
    })?;
    let lease = fields
        .remove("lease")
        .ok_or_else(|| refuse("it has no `lease`".to_owned()))?;
    if let Some(unknown) = fields.keys().next() {
        return Err(refuse(format!(
            "it has a field this version does not know, `{unknown}`"
        )));
    }
    let lease = Lease::from_wire(lease)?;
    Ok(LeaseRecord { event, lease })
}

/// The refusal of a lease record, saying what was wrong with it.
pub(crate) fn lease_refusal(what: String) -> Box<ErrorData> {
    Box::new(ErrorData::new(
        WireKind::Permission,
        code::LEASE_NOT_UNDERSTOOD,
        format!(
            "The lease record was refused: {what}. A lease is enforced, so a record that this \
             version does not understand in full is refused whole, never applied in part."
        ),
    ))
}

/// An artifact id this build made: `a_` and digits. Anything else could
/// name a file outside the artifacts directory, so it is refused.
pub(crate) fn is_artifact_id(id: &ArtifactId) -> bool {
    number_after(id.as_str(), "a_").is_some()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::redact::NoRedaction;

    fn entry(body: Body) -> Entry {
        Entry {
            id: EntryId::new("e_0003"),
            parent: Some(EntryId::new("e_0002")),
            ts: Timestamp::new("2026-11-12T10:15:02Z"),
            turn: Some(TurnId::new("t_1")),
            body,
        }
    }

    fn round_trip(body: Body) -> Value {
        let line = encode(&entry(body.clone()), &NoRedaction).unwrap();
        assert_eq!(line.last(), Some(&b'\n'));
        let back = decode(&line[..line.len() - 1]).unwrap();
        assert_eq!(back.body, body);
        assert_eq!(back.id.as_str(), "e_0003");
        serde_json::from_slice(&line).unwrap()
    }

    #[test]
    fn an_item_entry_has_the_envelope_and_the_item_nested() {
        let user = UserMessageItem {
            id: "i_1".into(),
            parts: vec![Part::text("hello\nthere")],
            delivery: None,
            created_at: None,
        };
        let line = round_trip(Body::User(user));
        assert_eq!(
            line,
            json!({
                "v": 1, "id": "e_0003", "parent": "e_0002", "ts": "2026-11-12T10:15:02Z",
                "turn": "t_1", "kind": "user",
                "item": {"id": "i_1", "parts": [{"kind": "text", "text": "hello\nthere"}]}
            })
        );
    }

    #[test]
    fn every_known_kind_round_trips() {
        let hash = ContentHash::of(b"x");
        round_trip(Body::Checkpoint(Checkpoint {
            files: vec![
                FileChange {
                    path: "src/a.rs".into(),
                    before: Before::Absent,
                    after: Some(FileVersion {
                        hash,
                        bytes: 1,
                        kept: true,
                    }),
                },
                FileChange {
                    path: "big.bin".into(),
                    before: Before::Version(FileVersion {
                        hash,
                        bytes: 1 << 30,
                        kept: false,
                    }),
                    after: None,
                },
                FileChange {
                    path: "made-by-a-command.txt".into(),
                    before: Before::Unknown,
                    after: None,
                },
            ],
        }));
        round_trip(Body::Outcome(Outcome {
            ended: Some(TurnOutcome::Completed),
            verification: Some(Verification::Passed),
            ..Outcome::default()
        }));
        round_trip(Body::Outcome(Outcome::reverted(Reverted::Undone)));
        round_trip(Body::Meta(Meta {
            title: Some("A title".into()),
            labels: vec!["x".into()],
            note: None,
        }));
        round_trip(Body::Ext(ExtRecord {
            ext: "my-ext".into(),
            data: json!({"n": 1}),
        }));
        round_trip(Body::Escalation(Escalation {
            question: Some("Merge?".into()),
            category: Some("merge".into()),
            ..Escalation::default()
        }));
        round_trip(Body::Decision(Decision {
            decision: Some("retry".into()),
            ..Decision::default()
        }));
        round_trip(Body::Rewind(Rewind {
            reason: RewindReason::Undo,
            from: Some("e_0009".into()),
        }));
        round_trip(Body::Epoch(Epoch {
            provider: "p".into(),
            protocol: "test".into(),
            model: "m".into(),
            effort: None,
            system: "be brief".into(),
            tools: vec![json!({"name": "read"})],
            hash: Epoch::hash_of("be brief", &[json!({"name": "read"})]),
        }));
        round_trip(Body::Session(Header {
            session: "s_1".into(),
            workspace: "/w".into(),
            harness: "0.0.0".into(),
            forked_from: Some(ForkOrigin {
                session: "s_0".into(),
                entry: "e_0004".into(),
            }),
        }));
        round_trip(Body::Attempt(Attempt {
            item: Some("i_4".into()),
            attempt: 2,
            error: None,
            parts: vec![Part::text("half an ans")],
        }));
    }

    #[test]
    fn only_an_explicit_null_says_a_file_did_not_exist_before() {
        let hash = ContentHash::of(b"x");
        let line = |files: Value| {
            let mut line = json!({"v": 1, "id": "e_0003", "parent": "e_0002",
                "ts": "2026-11-12T10:15:02Z", "kind": "checkpoint"});
            line["files"] = files;
            serde_json::to_vec(&line).unwrap()
        };
        let version = json!({"hash": hash, "bytes": 1});
        let read = decode(&line(json!([
            // As step 8 wrote them, and as this build still does.
            {"path": "new.txt", "before": null, "after": version},
            {"path": "old.txt", "before": version, "after": null},
            // What a shell command changed, earlier content not known.
            {"path": "seen.txt", "before": "unknown", "after": version},
            // No `before` at all, and a state from a later build.
            {"path": "bare.txt", "after": version},
            {"path": "later.txt", "before": "in_git", "after": version},
            // Not a version: nothing to put back, and the rest still read.
            {"path": "odd-1.txt", "before": true, "after": version},
            {"path": "odd-2.txt", "before": 7, "after": version},
            {"path": "odd-3.txt", "before": [1], "after": version},
            {"path": "odd-4.txt", "before": {"hash": 5}, "after": version},
        ])))
        .unwrap();
        let Body::Checkpoint(checkpoint) = read.body else {
            panic!("{:?}", read.body)
        };
        let befores: Vec<Before> = checkpoint.files.iter().map(|file| file.before).collect();
        let kept = FileVersion {
            hash,
            bytes: 1,
            kept: true,
        };
        assert_eq!(
            befores,
            [
                Before::Absent,
                Before::Version(kept),
                Before::Unknown,
                Before::Unknown,
                Before::Unknown,
                Before::Unknown,
                Before::Unknown,
                Before::Unknown,
                Before::Unknown
            ]
        );
        // Written back explicitly, so that no reader has to guess.
        let written = round_trip(Body::Checkpoint(checkpoint));
        assert_eq!(written["files"][0]["before"], Value::Null);
        assert_eq!(written["files"][2]["before"], json!("unknown"));
        assert_eq!(written["files"][3]["before"], json!("unknown"));
    }

    #[test]
    fn an_unknown_kind_and_unknown_fields_are_skipped_and_kept() {
        let line = br#"{"v":1,"id":"e_0004","parent":"e_0003","ts":"2026-11-12T10:15:02Z","kind":"hologram","beam":2}"#;
        let entry = decode(line).unwrap();
        assert_eq!(
            entry.body,
            Body::Unknown {
                kind: "hologram".into(),
                fields: json!({"beam": 2}).as_object().unwrap().clone()
            }
        );
        let line = br#"{"v":1,"id":"e_0004","parent":null,"ts":"2026-11-12T10:15:02Z","kind":"meta","title":"t","sparkle":true}"#;
        assert_eq!(
            decode(line).unwrap().body,
            Body::Meta(Meta {
                title: Some("t".into()),
                ..Meta::default()
            })
        );
    }

    #[test]
    fn a_line_from_a_newer_format_is_not_interpreted() {
        let line = br#"{"v":2,"id":"e_0004","parent":null,"ts":"2026-11-12T10:15:02Z","kind":"meta","title":"t"}"#;
        assert!(matches!(decode(line).unwrap().body, Body::Unknown { .. }));
    }

    #[test]
    fn a_lease_from_a_newer_format_is_refused_not_skipped() {
        // Even one this build could read: a newer format may give its
        // fields another meaning.
        let mut line: Value = serde_json::from_slice(&lease_line(
            json!({"event": "ended", "lease": lease_json()}),
        ))
        .unwrap();
        line["v"] = json!(FORMAT_VERSION + 1);
        let body = decode(&serde_json::to_vec(&line).unwrap()).unwrap().body;
        let Body::LeaseRefused(error) = body else {
            panic!("a newer lease was read or skipped: {body:?}")
        };
        assert_eq!(error.code, code::LEASE_NOT_UNDERSTOOD);
        assert!(
            error.message.contains("format version 2"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_known_kind_with_unreadable_fields_is_skipped_not_fatal() {
        let line = br#"{"v":1,"id":"e_0004","parent":null,"ts":"2026-11-12T10:15:02Z","kind":"meta","title":5}"#;
        assert!(matches!(
            decode(line).unwrap().body,
            Body::Unreadable { .. }
        ));
        assert!(decode(b"{not json").is_err());
        assert!(decode(br#"{"v":1,"kind":"meta"}"#).is_err());
    }

    fn lease_json() -> Value {
        json!({
            "id": "l_1",
            "grant": {"level": "supervised", "sandbox": {"read": {"all": true}, "write": {"roots": ["/w"]}, "network": "off"}},
            "expires": {"kind": "end_of_session"},
            "granted_by": {"kind": "user"}
        })
    }

    fn lease_line(fields: Value) -> Vec<u8> {
        let mut line = json!({"v": 1, "id": "e_0002", "parent": null, "ts": "2026-11-12T10:15:02Z", "kind": "lease"});
        for (key, value) in fields.as_object().unwrap() {
            line[key] = value.clone();
        }
        serde_json::to_vec(&line).unwrap()
    }

    #[test]
    fn a_lease_understood_in_full_is_read() {
        let body = decode(&lease_line(
            json!({"event": "granted", "lease": lease_json()}),
        ))
        .unwrap()
        .body;
        let Body::Lease(record) = body else {
            panic!("{body:?}")
        };
        assert_eq!(record.event, LeaseEvent::Granted);
        assert_eq!(record.lease.id.as_str(), "l_1");
        round_trip(Body::Lease(record));
    }

    #[test]
    fn a_lease_not_understood_in_full_is_refused_whole() {
        let mut unknown_restriction = lease_json();
        unknown_restriction["grant"]["sandbox"]["hosts"] = json!(["example.com"]);
        let mut unknown_level = lease_json();
        unknown_level["grant"]["level"] = json!("cautious");
        let cases = [
            json!({"event": "granted", "lease": unknown_restriction}),
            json!({"event": "granted", "lease": unknown_level}),
            json!({"event": "narrowed", "lease": lease_json()}),
            json!({"event": "granted", "lease": lease_json(), "scope": "src/"}),
            json!({"event": "granted"}),
            json!({"lease": lease_json()}),
        ];
        for fields in cases {
            let body = decode(&lease_line(fields.clone())).unwrap().body;
            let Body::LeaseRefused(error) = body else {
                panic!("read in part: {fields}")
            };
            assert_eq!(error.code, code::LEASE_NOT_UNDERSTOOD, "{fields}");
        }
    }

    #[test]
    fn an_entry_that_was_not_understood_is_never_written() {
        let body = Body::Unreadable {
            kind: "meta".into(),
            reason: "x".into(),
        };
        assert!(encode(&entry(body), &NoRedaction).is_err());
    }

    #[test]
    fn ids_are_numbered() {
        assert_eq!(EntryId::new("e_0042").number(), Some(42));
        assert_eq!(EntryId::header().number(), Some(0));
        assert_eq!(EntryId::new("e_").number(), None);
        assert_eq!(EntryId::new("x_1").number(), None);
        assert!(is_artifact_id(&ArtifactId::new("a_0001")));
        assert!(!is_artifact_id(&ArtifactId::new("a_../x")));
        assert!(!is_artifact_id(&ArtifactId::new("b_1")));
    }
}
