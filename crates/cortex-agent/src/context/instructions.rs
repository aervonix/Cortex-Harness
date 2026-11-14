//! Project instruction files (`docs/architecture/07-sessions-and-context.md`,
//! "Project instruction files").
//!
//! - **Discovery** at the start of an epoch: the user's own file in the
//!   configuration directory, then the workspace's, `AGENTS.md` before
//!   `CLAUDE.md` and then any other names configured, outermost first.
//! - **The cap**: 48 KiB in all, by default. Past it the innermost files are
//!   cut, and the user is warned.
//! - **Nested files**: when a tool first touches a file under a directory
//!   with an instruction file of its own, that file is appended once as a
//!   notice.
//! - **Changes on disk**, looked for at the start of each turn: a changed,
//!   new or removed file is appended as a notice. The system prompt is not
//!   rewritten until the next epoch.
//!
//! The loop reads no file itself: [`InstructionFiles`] is the seam, which
//! the core fills with reads that follow no link out of the workspace and
//! leave out what the permission engine would not let the agent read.
//!
//! What the model has been given of each file is known from the epoch's
//! record and from the notices on the branch, whose first line names the
//! file in a fixed form ([`parse_notice`]). So a resumed or rewound session
//! knows it again without anything else in the log.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use cortex_base::{AbsPath, ContentHash};

use super::prompt::InstructionText;

/// The default names, in the order a directory's files are read.
pub const DEFAULT_NAMES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// The default cap on what the instruction files may hold, in bytes.
pub const DEFAULT_CAP: usize = 48 * 1024;

/// What was read of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileText {
    /// Its first bytes, at most as many as were asked for.
    pub bytes: Vec<u8>,
    /// Its whole length.
    pub len: u64,
}

/// Where instruction files are read from. The core's reads the file
/// system; [`MemoryInstructionFiles`] is for tests.
///
/// Called on the blocking pool.
pub trait InstructionFiles: Send + Sync {
    /// Reads at most `limit` bytes of the file at `path`. `Ok(None)` when
    /// there is no file there that the harness may read: none at all, one
    /// that is not a regular file, a link out of the workspace, or one the
    /// permission engine would not let the agent read.
    ///
    /// # Errors
    ///
    /// When the file is there and could not be read.
    fn read(&self, path: &AbsPath, limit: usize) -> Result<Option<FileText>, String>;
}

/// No instruction files: a session that reads none.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoInstructionFiles;

impl InstructionFiles for NoInstructionFiles {
    fn read(&self, _path: &AbsPath, _limit: usize) -> Result<Option<FileText>, String> {
        Ok(None)
    }
}

/// Instruction files kept in memory, which a test changes as it goes.
#[derive(Debug, Default)]
pub struct MemoryInstructionFiles {
    files: Mutex<BTreeMap<String, Vec<u8>>>,
    reads: Mutex<u64>,
}

impl MemoryInstructionFiles {
    /// No files.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Puts a file at `path`, or replaces it.
    pub fn set(&self, path: &AbsPath, text: impl Into<Vec<u8>>) {
        self.lock().insert(path.key().to_owned(), text.into());
    }

    /// Removes the file at `path`.
    pub fn remove(&self, path: &AbsPath) {
        self.lock().remove(path.key());
    }

    /// How many reads have been asked for.
    #[must_use]
    pub fn reads(&self) -> u64 {
        *self.reads.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Vec<u8>>> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl InstructionFiles for MemoryInstructionFiles {
    fn read(&self, path: &AbsPath, limit: usize) -> Result<Option<FileText>, String> {
        *self.reads.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        Ok(self.lock().get(path.key()).map(|bytes| FileText {
            bytes: bytes[..bytes.len().min(limit)].to_vec(),
            len: bytes.len() as u64,
        }))
    }
}

/// Where an instruction file may be, and how the model is shown its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Place {
    pub(crate) path: AbsPath,
    /// Relative to the workspace with `/`, or the user's own file as the
    /// system writes it.
    pub(crate) shown: String,
    pub(crate) user: bool,
    /// The directory it is in, for telling two names of one file apart.
    pub(crate) dir: AbsPath,
}

/// The places looked at when an epoch starts, outermost first.
pub(crate) fn places(root: &AbsPath, user_dir: Option<&AbsPath>, names: &[String]) -> Vec<Place> {
    let mut out = Vec::new();
    if let Some(dir) = user_dir {
        for name in names {
            if let Ok(path) = dir.join(name) {
                out.push(Place {
                    shown: path.to_native_string(),
                    path,
                    user: true,
                    dir: dir.clone(),
                });
            }
        }
    }
    for name in names {
        if let Ok(path) = root.join(name) {
            out.push(Place {
                shown: shown(root, &path),
                path,
                user: false,
                dir: root.clone(),
            });
        }
    }
    out
}

/// How the model is shown a file in the workspace: relative, with `/`.
pub(crate) fn shown(root: &AbsPath, path: &AbsPath) -> String {
    path.strip_prefix(root)
        .map_or_else(|| path.to_native_string(), |rel| rel.as_str().to_owned())
}

/// The directories between the workspace root (left out) and the
/// directory of `file` (in), outermost first: where a nested instruction
/// file of `file` may be.
pub(crate) fn nested_dirs(root: &AbsPath, file: &AbsPath) -> Vec<AbsPath> {
    let Some(parent) = file.parent() else {
        return Vec::new();
    };
    if !parent.starts_with(root) {
        return Vec::new();
    }
    let mut dirs = Vec::new();
    let mut at = parent;
    while at != *root && at.starts_with(root) {
        dirs.push(at.clone());
        match at.parent() {
            Some(up) => at = up,
            None => break,
        }
    }
    dirs.reverse();
    dirs
}

/// One file as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Found {
    pub(crate) place: Place,
    /// What was read, as text, cut to the cap.
    pub(crate) text: String,
    /// Whether it was cut.
    pub(crate) cut: bool,
}

impl Found {
    pub(crate) fn hash(&self) -> ContentHash {
        ContentHash::of(self.text.as_bytes())
    }
}

/// Reads one place: `None` when there is no file the harness may read
/// there, and an error in words when it could not be read.
pub(crate) fn read_one(
    io: &dyn InstructionFiles,
    place: &Place,
    cap: usize,
) -> Result<Option<Found>, String> {
    let Some(read) = io.read(&place.path, cap)? else {
        return Ok(None);
    };
    let mut text = String::from_utf8_lossy(&read.bytes).into_owned();
    let cut = read.len > read.bytes.len() as u64 || text.len() > cap;
    if text.len() > cap {
        let mut at = cap;
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        text.truncate(at);
    }
    Ok(Some(Found {
        place: place.clone(),
        text,
        cut,
    }))
}

/// What the start of an epoch read.
#[derive(Debug, Clone, Default)]
pub(crate) struct Discovered {
    /// What section 4 of the prompt holds, in order.
    pub(crate) texts: Vec<InstructionText>,
    /// Each file the prompt holds or left out, by the name it is shown
    /// under, and the hash of what was read of it.
    pub(crate) marks: Vec<(String, ContentHash)>,
    /// What the user is warned of: files cut or left out for the cap, and
    /// files that could not be read.
    pub(crate) warnings: Vec<String>,
    /// The files named in the warnings, for the notice's list.
    pub(crate) warned: Vec<String>,
}

/// Reads every place, keeps the cap, and says what was left out.
pub(crate) fn discover(io: &dyn InstructionFiles, places: &[Place], cap: usize) -> Discovered {
    let mut out = Discovered::default();
    let mut room = cap;
    let mut seen_in: BTreeMap<String, BTreeSet<ContentHash>> = BTreeMap::new();
    let mut cut_files = Vec::new();
    let mut left_out = Vec::new();
    for place in places {
        let found = match read_one(io, place, cap) {
            Ok(Some(found)) => found,
            Ok(None) => continue,
            Err(error) => {
                out.warnings.push(format!(
                    "The instruction file {} could not be read: {error}",
                    place.shown
                ));
                out.warned.push(place.shown.clone());
                continue;
            }
        };
        let hash = found.hash();
        // Two names of one file in one directory (a `CLAUDE.md` that links
        // to `AGENTS.md`) are read once.
        if !seen_in
            .entry(place.dir.key().to_owned())
            .or_default()
            .insert(hash)
        {
            continue;
        }
        out.marks.push((place.shown.clone(), hash));
        let mut text = found.text;
        let mut cut = found.cut;
        if room == 0 {
            left_out.push(place.shown.clone());
            continue;
        }
        if text.len() > room {
            let mut at = room;
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            text.truncate(at);
            cut = true;
        }
        room -= text.len();
        if cut {
            cut_files.push(place.shown.clone());
            text.push_str(&cut_marker(cap));
        }
        out.texts.push(InstructionText {
            path: place.shown.clone(),
            user: place.user,
            text,
        });
    }
    if !cut_files.is_empty() || !left_out.is_empty() {
        let mut message = format!(
            "The instruction files hold more than the {} KiB they may, so ",
            cap / 1024
        );
        if !cut_files.is_empty() {
            message.push_str(&format!("the end of {} was", cut_files.join(", ")));
        }
        if !left_out.is_empty() {
            if !cut_files.is_empty() {
                message.push_str(", and ");
            }
            message.push_str(&format!("{} was", left_out.join(", ")));
        }
        message.push_str(" left out of the prompt. `context.instructions_cap` sets the limit.");
        out.warnings.push(message);
        out.warned.extend(cut_files);
        out.warned.extend(left_out);
    }
    out
}

/// What is said at the end of a file that was cut.
fn cut_marker(cap: usize) -> String {
    format!("\n\n[The rest of this file was left out: instruction files may hold {cap} bytes.]")
}

/// What a notice about an instruction file says happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// It changed since the model was given it.
    Changed(Found),
    /// It appeared where there was none.
    New(Found),
    /// It is gone.
    Removed(String),
    /// A file under a directory a tool touched, given for the first time.
    Nested { found: Found, dir: String },
}

/// The text of a notice about an instruction file. Its first line names
/// the file in a fixed form, which [`parse_notice`] reads back.
pub(crate) fn notice_text(change: &Change, cap: usize) -> String {
    let content = |found: &Found| {
        if found.cut {
            format!("{}{}", found.text, cut_marker(cap))
        } else {
            found.text.clone()
        }
    };
    match change {
        Change::Changed(found) => format!(
            "Instruction file `{}` changed. This is all of it now, and it replaces the version \
             you were given before:\n\n{}",
            found.place.shown,
            content(found)
        ),
        Change::New(found) => format!(
            "Instruction file `{}` is new. It reads:\n\n{}",
            found.place.shown,
            content(found)
        ),
        Change::Removed(shown) => {
            format!("Instruction file `{shown}` was removed. Its instructions no longer apply.")
        }
        Change::Nested { found, dir } => format!(
            "Instruction file `{}` applies to the files under `{dir}/`, and was not in your \
             instructions until now. It reads:\n\n{}",
            found.place.shown,
            content(found)
        ),
    }
}

/// What a notice of ours says the model now has of a file: its name, and
/// the hash of its content, or `None` for one that was removed. `None` for
/// a notice that is not about an instruction file.
pub(crate) fn parse_notice(text: &str) -> Option<(String, Option<ContentHash>)> {
    let rest = text.strip_prefix("Instruction file `")?;
    let (first, body) = rest.split_once('\n').unwrap_or((rest, ""));
    let end = first.find("` ")?;
    let shown = first[..end].to_owned();
    let what = &first[end + 2..];
    if what.starts_with("was removed") {
        return Some((shown, None));
    }
    let content = body.strip_prefix('\n')?;
    let content = strip_cut_marker(content);
    Some((shown, Some(ContentHash::of(content.as_bytes()))))
}

fn strip_cut_marker(content: &str) -> &str {
    if let Some(at) = content.rfind("\n\n[The rest of this file was left out: ")
        && content.ends_with(" bytes.]")
    {
        return &content[..at];
    }
    content
}

/// What the model has of each instruction file: by the name it was shown
/// under, the hash of the version it was given, or `None` when it was told
/// the file is gone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Given {
    pub(crate) files: BTreeMap<String, Option<ContentHash>>,
    /// The directories already looked at for a nested file.
    pub(crate) looked: BTreeSet<String>,
}

impl Given {
    /// What an epoch's prompt gave.
    pub(crate) fn from_marks(marks: &[(String, ContentHash)]) -> Self {
        Self {
            files: marks
                .iter()
                .map(|(shown, hash)| (shown.clone(), Some(*hash)))
                .collect(),
            looked: BTreeSet::new(),
        }
    }

    /// A notice on the branch, read back.
    pub(crate) fn note(&mut self, notice: &str) {
        if let Some((shown, hash)) = parse_notice(notice) {
            self.files.insert(shown, hash);
        }
    }
}

/// The date line an epoch's first message carries.
pub(crate) fn date_text(date: &str) -> String {
    format!("Today's date is {date} (UTC).")
}

#[cfg(test)]
mod tests {
    use cortex_base::PathStyle;

    use super::*;

    fn path(text: &str) -> AbsPath {
        AbsPath::parse(text, PathStyle::Posix).unwrap()
    }

    fn names() -> Vec<String> {
        DEFAULT_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect()
    }

    #[test]
    fn the_user_file_comes_first_then_agents_before_claude() {
        let found = places(&path("/work/app"), Some(&path("/config/cx")), &names());
        let shown: Vec<&str> = found.iter().map(|place| place.shown.as_str()).collect();
        assert_eq!(
            shown,
            [
                "/config/cx/AGENTS.md",
                "/config/cx/CLAUDE.md",
                "AGENTS.md",
                "CLAUDE.md"
            ]
        );
    }

    #[test]
    fn the_cap_cuts_the_innermost_files_and_says_so() {
        let files = MemoryInstructionFiles::new();
        let root = path("/work/app");
        let user = path("/config/cx");
        files.set(&user.join("AGENTS.md").unwrap(), "u".repeat(60));
        files.set(&root.join("AGENTS.md").unwrap(), "a".repeat(60));
        files.set(&root.join("CLAUDE.md").unwrap(), "c".repeat(60));
        let found = discover(&files, &places(&root, Some(&user), &names()), 100);
        assert_eq!(found.texts.len(), 2, "{found:?}");
        assert_eq!(found.texts[0].text, "u".repeat(60));
        assert!(found.texts[1].text.starts_with(&"a".repeat(40)));
        assert!(found.texts[1].text.contains("was left out"));
        assert_eq!(found.marks.len(), 3, "the file left out is known");
        assert_eq!(found.warned, ["AGENTS.md", "CLAUDE.md"]);
        assert!(found.warnings[0].contains("left out of the prompt"));
    }

    #[test]
    fn two_names_of_one_file_are_read_once() {
        let files = MemoryInstructionFiles::new();
        let root = path("/work/app");
        files.set(&root.join("AGENTS.md").unwrap(), "same");
        files.set(&root.join("CLAUDE.md").unwrap(), "same");
        let found = discover(&files, &places(&root, None, &names()), DEFAULT_CAP);
        assert_eq!(found.texts.len(), 1);
        assert_eq!(found.texts[0].path, "AGENTS.md");
    }

    #[test]
    fn a_notice_is_read_back_as_what_the_model_was_given() {
        let place = places(&path("/w"), None, &names()).remove(0);
        let found = Found {
            place: place.clone(),
            text: "Use tabs.\n\nAlways.".to_owned(),
            cut: true,
        };
        for change in [
            Change::Changed(found.clone()),
            Change::New(found.clone()),
            Change::Nested {
                found: found.clone(),
                dir: "src".to_owned(),
            },
        ] {
            let text = notice_text(&change, 10);
            assert_eq!(
                parse_notice(&text),
                Some(("AGENTS.md".to_owned(), Some(found.hash()))),
                "{text}"
            );
        }
        assert_eq!(
            parse_notice(&notice_text(&Change::Removed("AGENTS.md".to_owned()), 10)),
            Some(("AGENTS.md".to_owned(), None))
        );
        assert_eq!(parse_notice("The plan changed."), None);
    }

    #[test]
    fn the_nested_directories_are_between_the_root_and_the_file() {
        let dirs = nested_dirs(&path("/w"), &path("/w/src/api/handler.rs"));
        let shown: Vec<String> = dirs.iter().map(AbsPath::to_string).collect();
        assert_eq!(shown, ["/w/src", "/w/src/api"]);
        assert!(nested_dirs(&path("/w"), &path("/w/top.rs")).is_empty());
        assert!(nested_dirs(&path("/w"), &path("/elsewhere/x.rs")).is_empty());
    }
}
