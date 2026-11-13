//! Reading a log as a stream: every line once, in order, keeping only what
//! the tree needs (`docs/architecture/07-sessions-and-context.md`,
//! "Reading").
//!
//! For each entry the scan keeps its id, its parent, its kind, its turn,
//! the id of the item it holds, and where its line is in the file. The rest
//! of a line is checked and skipped, and read again from the file when it
//! is asked for. A session is never parsed whole into one value, and a
//! long session costs little memory.
//!
//! - A last line with no newline is kept if it is a whole entry: a crash
//!   can cut a line, and a cut can never turn part of an entry into a
//!   different whole one. Otherwise it is the **cut tail**: ignored, and
//!   reported.
//! - A line that is not an entry is skipped and reported, wherever it is.
//! - An entry whose parent is not an earlier entry starts a broken branch,
//!   and is reported.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::{self, Read};

use cortex_protocol::ids::{ItemId, Timestamp, TurnId};
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::entry::{self, EntryId, FORMAT_VERSION, HEADER_ID, Kind};

/// The longest line that is read. Large content is stored as artifacts, so
/// a real entry is far smaller; a longer line is damage.
pub(crate) const MAX_LINE: usize = 64 * 1024 * 1024;

/// How much of the file is read at a time.
const CHUNK: usize = 1024 * 1024;

/// One entry, as the tree needs it.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub(crate) id: EntryId,
    /// The parent's index in the scan's list.
    pub(crate) parent: Option<u32>,
    pub(crate) kind: Kind,
    pub(crate) turn: Option<TurnId>,
    /// Where the line starts, and its length without the newline.
    pub(crate) offset: u64,
    pub(crate) len: u32,
    /// A session record: outside the conversation tree.
    pub(crate) record: bool,
}

/// A last line that a crash cut off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutTail {
    /// Where it starts in the file.
    pub offset: u64,
    /// How many bytes of it there are.
    pub bytes: u64,
}

/// What a scan found.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    pub(crate) nodes: Vec<Node>,
    pub(crate) by_id: HashMap<EntryId, u32>,
    pub(crate) by_item: HashMap<ItemId, u32>,
    /// The format version of the first line.
    pub(crate) version: u64,
    /// The end of the last whole entry: where the next line goes once a
    /// missing newline is added.
    pub(crate) end: u64,
    /// The last line is a whole entry with no newline after it.
    pub(crate) missing_newline: bool,
    pub(crate) cut: Option<CutTail>,
    /// Line numbers (from 1) of lines that are not entries.
    pub(crate) damaged: Vec<u64>,
    /// The damaged lines that may be lease records: a lease that cannot be
    /// read makes the leases unknown, so these are never just skipped.
    pub(crate) damaged_leases: Vec<u64>,
    /// Entries whose parent is not an earlier entry.
    pub(crate) broken_links: Vec<EntryId>,
    /// Kinds this build does not know, with how many of each.
    pub(crate) unknown_kinds: BTreeMap<String, u32>,
    /// Lines written in a newer format version.
    pub(crate) newer: u32,
    pub(crate) last_ts: Option<Timestamp>,
    pub(crate) max_entry: u64,
    pub(crate) max_item: u64,
    pub(crate) max_turn: u64,
}

/// Why a file is not a session log at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NotALog(pub(crate) String);

impl Scan {
    /// Reads a whole log from `source`.
    pub(crate) fn read(source: impl Read) -> io::Result<Result<Self, NotALog>> {
        Self::read_limited(source, MAX_LINE)
    }

    /// Reads a whole log whose lines are at most `max_line` bytes. Only a
    /// test asks for another limit than [`MAX_LINE`].
    pub(crate) fn read_limited(
        mut source: impl Read,
        max_line: usize,
    ) -> io::Result<Result<Self, NotALog>> {
        let mut scan = Self::default();
        let mut buffer: Vec<u8> = Vec::with_capacity(CHUNK);
        // Where `buffer[0]` is in the file.
        let mut base: u64 = 0;
        let mut line_number: u64 = 0;
        // Skipping the rest of a line that is too long.
        let mut skipping = false;
        let mut chunk = vec![0u8; CHUNK];
        loop {
            let read = match source.read(&mut chunk) {
                Ok(0) => break,
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            buffer.extend_from_slice(&chunk[..read]);
            let mut start = 0;
            while let Some(found) = buffer[start..].iter().position(|&byte| byte == b'\n') {
                let line_end = start + found;
                if skipping {
                    skipping = false;
                } else {
                    line_number += 1;
                    let offset = base + start as u64;
                    if let Err(not_a_log) = scan.line(&buffer[start..line_end], offset, line_number)
                    {
                        return Ok(Err(not_a_log));
                    }
                }
                start = line_end + 1;
                scan.end = base + start as u64;
            }
            buffer.drain(..start);
            base += start as u64;
            if buffer.len() > max_line {
                // A line longer than any entry: damage. Skip to its end.
                if !skipping {
                    line_number += 1;
                    scan.damaged.push(line_number);
                }
                skipping = true;
                base += buffer.len() as u64;
                buffer.clear();
            }
        }
        if !buffer.is_empty() {
            // The last line has no newline.
            let offset = base;
            let whole = !skipping && skeleton(&buffer).is_ok() && scan.version != 0;
            if whole {
                line_number += 1;
                if let Err(not_a_log) = scan.line(&buffer, offset, line_number) {
                    return Ok(Err(not_a_log));
                }
                scan.end = base + buffer.len() as u64;
                scan.missing_newline = true;
            } else {
                scan.cut = Some(CutTail {
                    offset,
                    bytes: buffer.len() as u64,
                });
            }
        } else if skipping {
            scan.cut = Some(CutTail {
                offset: scan.end,
                bytes: base - scan.end,
            });
        }
        if scan.nodes.is_empty() {
            return Ok(Err(NotALog("it has no first line".to_owned())));
        }
        Ok(Ok(scan))
    }

    /// Takes in one whole line.
    fn line(&mut self, line: &[u8], offset: u64, number: u64) -> Result<(), NotALog> {
        let parsed = skeleton(line);
        if self.nodes.is_empty() {
            // The first line must be the session's.
            let header = parsed
                .as_ref()
                .ok()
                .filter(|found| found.kind == "session" && found.id == HEADER_ID);
            let Some(header) = header else {
                return Err(NotALog(
                    "its first line is not the start of a session".to_owned(),
                ));
            };
            self.version = header.v;
        }
        let Ok(found) = parsed else {
            self.damaged.push(number);
            if may_be_lease(line) {
                self.damaged_leases.push(number);
            }
            return Ok(());
        };
        let id = EntryId::new(found.id.as_ref());
        if self.by_id.contains_key(&id) {
            // Two lines with one id: the later one is damage.
            self.damaged.push(number);
            if found.kind == Kind::Lease.as_str() {
                self.damaged_leases.push(number);
            }
            return Ok(());
        }
        let index = u32::try_from(self.nodes.len()).map_err(|_| NotALog("too long".to_owned()))?;
        let parent = match &found.parent {
            None => None,
            Some(parent) => match self.by_id.get(&EntryId::new(parent.as_ref())) {
                Some(&parent) => Some(parent),
                None => {
                    self.broken_links.push(id.clone());
                    None
                }
            },
        };
        let newer = found.v > FORMAT_VERSION;
        let kind = if !newer {
            Kind::from_name(&found.kind)
        } else if found.kind == Kind::Lease.as_str() {
            // Kept as a lease, so that asking for the lease in force finds
            // it and refuses: a newer build's lease is never skipped.
            self.newer += 1;
            Kind::Lease
        } else {
            self.newer += 1;
            Kind::Unknown(found.kind.as_ref().into())
        };
        if let Kind::Unknown(name) = &kind {
            *self.unknown_kinds.entry(name.to_string()).or_default() += 1;
        }
        let item = if kind.holds_item() {
            found.item.0.map(ItemId::new)
        } else {
            None
        };
        if let Some(item) = &item {
            self.max_item = self
                .max_item
                .max(entry::number_after(item.as_str(), "i_").unwrap_or(0));
            self.by_item.insert(item.clone(), index);
        }
        let turn = found.turn.map(|turn| TurnId::new(turn.into_owned()));
        if let Some(turn) = &turn {
            self.max_turn = self
                .max_turn
                .max(entry::number_after(turn.as_str(), "t_").unwrap_or(0));
        }
        self.max_entry = self.max_entry.max(id.number().unwrap_or(0));
        self.last_ts = Some(Timestamp::new(found.ts.into_owned()));
        self.by_id.insert(id.clone(), index);
        self.nodes.push(Node {
            id,
            parent,
            kind,
            turn,
            offset,
            len: u32::try_from(line.len()).unwrap_or(u32::MAX),
            record: found.parent.is_none() && index != 0,
        });
        Ok(())
    }
}

/// Whether a line that could not be read may be a lease record: whether it
/// holds the JSON string `"lease"`, as the kind or the key of one does. Text
/// that merely mentions a lease is inside a longer string, and does not.
fn may_be_lease(line: &[u8]) -> bool {
    line.windows(7).any(|window| window == br#""lease""#)
}

/// The envelope of a line, and the id of the item it holds.
#[derive(Deserialize)]
struct Skeleton<'a> {
    v: u64,
    #[serde(borrow)]
    id: Cow<'a, str>,
    #[serde(borrow, default)]
    parent: Option<Cow<'a, str>>,
    #[serde(borrow)]
    ts: Cow<'a, str>,
    #[serde(borrow, default)]
    turn: Option<Cow<'a, str>>,
    #[serde(borrow)]
    kind: Cow<'a, str>,
    #[serde(default)]
    item: ItemRef,
}

fn skeleton(line: &[u8]) -> Result<Skeleton<'_>, serde_json::Error> {
    serde_json::from_slice(line)
}

/// The `id` of an object, read without keeping the rest of it. Anything
/// that is not an object with a string `id` gives `None`.
#[derive(Default)]
struct ItemRef(Option<String>);

impl<'de> Deserialize<'de> for ItemRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(ItemRefVisitor)
    }
}

struct ItemRefVisitor;

impl<'de> Visitor<'de> for ItemRefVisitor {
    type Value = ItemRef;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("anything")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ItemRef, A::Error> {
        let mut id = None;
        while let Some(is_id) = map.next_key_seed(IsId)? {
            if is_id && id.is_none() {
                id = map.next_value::<MaybeText>()?.0;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(ItemRef(id))
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<ItemRef, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(ItemRef(None))
    }

    fn visit_str<E>(self, _: &str) -> Result<ItemRef, E> {
        Ok(ItemRef(None))
    }

    fn visit_bool<E>(self, _: bool) -> Result<ItemRef, E> {
        Ok(ItemRef(None))
    }

    fn visit_i64<E>(self, _: i64) -> Result<ItemRef, E> {
        Ok(ItemRef(None))
    }

    fn visit_u64<E>(self, _: u64) -> Result<ItemRef, E> {
        Ok(ItemRef(None))
    }

    fn visit_f64<E>(self, _: f64) -> Result<ItemRef, E> {
        Ok(ItemRef(None))
    }

    fn visit_unit<E>(self) -> Result<ItemRef, E> {
        Ok(ItemRef(None))
    }
}

/// Whether a key is `id`, without keeping the key.
struct IsId;

impl<'de> DeserializeSeed<'de> for IsId {
    type Value = bool;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<bool, D::Error> {
        deserializer.deserialize_str(IsIdVisitor)
    }
}

struct IsIdVisitor;

impl Visitor<'_> for IsIdVisitor {
    type Value = bool;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a key")
    }

    fn visit_str<E>(self, key: &str) -> Result<bool, E> {
        Ok(key == "id")
    }
}

/// A string, or `None` for anything else.
struct MaybeText(Option<String>);

impl<'de> Deserialize<'de> for MaybeText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Text;
        impl<'de> Visitor<'de> for Text {
            type Value = MaybeText;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("anything")
            }
            fn visit_str<E>(self, text: &str) -> Result<MaybeText, E> {
                Ok(MaybeText(Some(text.to_owned())))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<MaybeText, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(MaybeText(None))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<MaybeText, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(MaybeText(None))
            }
            fn visit_bool<E>(self, _: bool) -> Result<MaybeText, E> {
                Ok(MaybeText(None))
            }
            fn visit_i64<E>(self, _: i64) -> Result<MaybeText, E> {
                Ok(MaybeText(None))
            }
            fn visit_u64<E>(self, _: u64) -> Result<MaybeText, E> {
                Ok(MaybeText(None))
            }
            fn visit_f64<E>(self, _: f64) -> Result<MaybeText, E> {
                Ok(MaybeText(None))
            }
            fn visit_unit<E>(self) -> Result<MaybeText, E> {
                Ok(MaybeText(None))
            }
        }
        deserializer.deserialize_any(Text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = r#"{"v":1,"id":"e_0000","parent":null,"ts":"2026-11-12T10:00:00Z","kind":"session","session":"s_1","workspace":"/w","harness":"0"}"#;

    fn scan(text: &str) -> Scan {
        Scan::read(text.as_bytes()).unwrap().unwrap()
    }

    #[test]
    fn the_tree_is_read_from_the_envelopes() {
        let text = format!(
            "{HEADER}\n\
             {{\"v\":1,\"id\":\"e_0001\",\"parent\":\"e_0000\",\"ts\":\"2026-11-12T10:00:01Z\",\"turn\":\"t_1\",\"kind\":\"user\",\"item\":{{\"parts\":[],\"id\":\"i_3\"}}}}\n\
             {{\"v\":1,\"id\":\"e_0002\",\"parent\":null,\"ts\":\"2026-11-12T10:00:02Z\",\"kind\":\"meta\",\"title\":\"x\"}}\n"
        );
        let scan = scan(&text);
        assert_eq!(scan.nodes.len(), 3);
        assert_eq!(scan.nodes[1].parent, Some(0));
        assert_eq!(scan.by_item.get(&ItemId::new("i_3")), Some(&1));
        assert_eq!(scan.nodes[1].turn, Some(TurnId::new("t_1")));
        assert!(!scan.nodes[1].record);
        assert!(scan.nodes[2].record);
        assert_eq!(scan.max_item, 3);
        assert_eq!(scan.max_turn, 1);
        assert_eq!(scan.max_entry, 2);
        assert_eq!(scan.end, text.len() as u64);
        assert!(scan.cut.is_none());
    }

    #[test]
    fn a_file_that_does_not_start_as_a_session_is_not_a_log() {
        for text in [
            "",
            "{\"v\":1}\n",
            "hello\n",
            "{\"v\":1,\"id\":\"e_0001\",\"parent\":null,\"ts\":\"x\",\"kind\":\"user\"}\n",
        ] {
            assert!(Scan::read(text.as_bytes()).unwrap().is_err(), "{text:?}");
        }
    }

    #[test]
    fn damage_in_the_middle_is_skipped_and_reported() {
        let text = format!(
            "{HEADER}\nnot json\n\
             {{\"v\":1,\"id\":\"e_0002\",\"parent\":\"e_0001\",\"ts\":\"t\",\"kind\":\"notice\",\"item\":{{\"id\":\"i_1\"}}}}\n\
             {{\"v\":1,\"id\":\"e_0002\",\"parent\":\"e_0000\",\"ts\":\"t\",\"kind\":\"notice\"}}\n"
        );
        let scan = scan(&text);
        assert_eq!(scan.damaged, vec![2, 4]);
        assert_eq!(scan.broken_links, vec![EntryId::new("e_0002")]);
        assert_eq!(scan.nodes.len(), 2);
    }

    #[test]
    fn damage_that_may_be_a_lease_is_noted() {
        let lines = [
            HEADER,
            r#"{"v":1,"id":"e_0001","parent":null,"kind":"lease","event":"en"#,
            r#"{"v":1,"id":"e_0002","parent":null,"ts":"t","kind":"meta","title":"the lease ended""#,
            r#"{"v":1,"id":"e_0003","parent":null,"ts":"t","kind":"meta"}"#,
            r#"{"v":1,"id":"e_0003","parent":null,"ts":"t","kind":"lease"}"#,
        ];
        let scan = scan(&format!("{}\n", lines.join("\n")));
        assert_eq!(scan.damaged, vec![2, 3, 5]);
        assert_eq!(scan.damaged_leases, vec![2, 5]);
    }

    #[test]
    fn a_whole_last_line_without_a_newline_is_kept() {
        let text = format!(
            "{HEADER}\n{{\"v\":1,\"id\":\"e_0001\",\"parent\":\"e_0000\",\"ts\":\"t\",\"kind\":\"hologram\"}}"
        );
        let scan = scan(&text);
        assert_eq!(scan.nodes.len(), 2);
        assert!(scan.missing_newline);
        assert_eq!(scan.end, text.len() as u64);
        assert_eq!(scan.unknown_kinds.get("hologram"), Some(&1));
    }

    #[test]
    fn a_cut_last_line_is_reported_and_not_read() {
        let full = format!(
            "{HEADER}\n{{\"v\":1,\"id\":\"e_0001\",\"parent\":\"e_0000\",\"ts\":\"t\",\"kind\":\"meta\"}}"
        );
        let cut = &full[..full.len() - 3];
        let scan = scan(cut);
        assert_eq!(scan.nodes.len(), 1);
        assert_eq!(
            scan.cut,
            Some(CutTail {
                offset: HEADER.len() as u64 + 1,
                bytes: (cut.len() - HEADER.len() - 1) as u64
            })
        );
        assert_eq!(scan.end, HEADER.len() as u64 + 1);
    }

    #[test]
    fn the_item_id_is_found_whatever_else_the_item_holds() {
        let line = br#"{"v":1,"id":"e_1","ts":"t","kind":"user","item":{"parts":[{"x":[1,{"id":"no"}]}],"n":null,"id":"i_9","id":"i_10"}}"#;
        assert_eq!(skeleton(line).unwrap().item.0.as_deref(), Some("i_9"));
        let line = br#"{"v":1,"id":"e_1","ts":"t","kind":"hologram","item":[1,2]}"#;
        assert_eq!(skeleton(line).unwrap().item.0, None);
        let line = br#"{"v":1,"id":"e_1","ts":"t","kind":"user","item":{"id":5}}"#;
        assert_eq!(skeleton(line).unwrap().item.0, None);
    }
}
