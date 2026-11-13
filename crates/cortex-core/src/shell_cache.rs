//! The version of each shell the harness has found, kept in the state
//! directory so that a session does not ask for it on its way to its first
//! request (`docs/performance-budgets.md`, "Start-up").
//!
//! The shell tool's description names the shell and its version. Asking
//! PowerShell 7 for its version starts it, which took about 180 ms of a
//! headless run's 212 ms on a hosted Windows runner (step 13b). So the
//! answer is kept here, under the shell's resolved path, its size and its
//! modification time: a shell that is replaced or updated is asked again.
//!
//! On a miss the session starts with the shell's name and no version, and
//! the shell is asked on a thread of its own, which writes the answer here
//! for the sessions after it. The version is only words for the model:
//! nothing that decides what a command may do reads it.
//!
//! **The file is a cache, never an authority.** One that is missing,
//! unreadable, not understood or too large is a miss, and a version in it
//! that does not look like one is a miss. It is written owner-only, like
//! everything in the state directory.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the cache is read while a session is assembled, off the core thread, and written \
              on a thread of its own"
)]

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use cortex_base::{AtomicWriteOptions, Clock, write_atomic};
use serde_json::{Value, json};

/// The file's name in the state directory.
pub(crate) const FILE: &str = "shells.json";

/// The version of the file's shape.
const SHAPE: u64 = 1;

/// The most shells the file remembers. A new one pushes out the oldest.
const KEPT: usize = 16;

/// The largest file that is read. A larger one is a miss.
const LARGEST: u64 = 64 * 1024;

/// What a shell's program file looked like when it was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stamp {
    /// The program, as the search resolved it.
    program: PathBuf,
    size: u64,
    /// Nanoseconds since 1970, as text: a `u128` is not a JSON number.
    modified: String,
}

impl Stamp {
    /// The stamp of `program` now. `None` when the file cannot be looked
    /// at, which leaves the shell to be asked.
    pub(crate) fn of(program: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(program).ok()?;
        let modified = metadata
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos()
            .to_string();
        Some(Self {
            program: program.to_path_buf(),
            size: metadata.len(),
            modified,
        })
    }

    fn matches(&self, entry: &Value) -> bool {
        entry["program"].as_str() == self.program.to_str()
            && entry["size"].as_u64() == Some(self.size)
            && entry["modified"].as_str() == Some(self.modified.as_str())
    }
}

/// The version kept for the shell `stamp` describes, if the file has one.
pub(crate) fn lookup(file: &Path, stamp: &Stamp) -> Option<String> {
    let entries = read(file)?;
    let version = entries.iter().find(|entry| stamp.matches(entry))?["version"].as_str()?;
    plausible(version).then(|| version.to_owned())
}

/// Keeps `version` for the shell `stamp` describes. A failure is said in
/// the log and changes nothing: the shell is asked again next time.
pub(crate) fn store(file: &Path, stamp: &Stamp, version: &str, clock: &dyn Clock) {
    if !plausible(version) {
        return;
    }
    let Some(program) = stamp.program.to_str() else {
        return;
    };
    let mut entries: Vec<Value> = read(file)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry["program"].as_str() != Some(program))
        .collect();
    entries.push(json!({
        "program": program,
        "size": stamp.size,
        "modified": stamp.modified,
        "version": version,
    }));
    let surplus = entries.len().saturating_sub(KEPT);
    entries.drain(..surplus);
    let text = json!({"shape": SHAPE, "shells": entries}).to_string();
    let options = AtomicWriteOptions {
        // A cache: losing it to a power failure costs one more question.
        sync: false,
        owner_only: true,
        ..AtomicWriteOptions::default()
    };
    if let Err(error) = write_atomic(file, text.as_bytes(), &options, clock) {
        tracing::debug!(%error, "the shells' versions could not be kept");
    }
}

fn read(file: &Path) -> Option<Vec<Value>> {
    let metadata = std::fs::metadata(file).ok()?;
    if metadata.len() > LARGEST {
        return None;
    }
    let value: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    if value["shape"].as_u64() != Some(SHAPE) {
        return None;
    }
    value["shells"].as_array().cloned()
}

/// Whether text looks like a version: what the model is told must not be
/// whatever a file says.
fn plausible(version: &str) -> bool {
    version.starts_with(|character: char| character.is_ascii_digit())
        && version.len() <= 32
        && version.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+')
        })
}

#[cfg(test)]
mod tests {
    use cortex_base::FakeClock;

    use super::*;

    struct Place {
        _dir: std::path::PathBuf,
        file: PathBuf,
        program: PathBuf,
    }

    fn place(test: &str) -> Place {
        let dir =
            std::env::temp_dir().join(format!("cortex-shell-cache-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("pwsh");
        std::fs::write(&program, b"a shell").unwrap();
        Place {
            file: dir.join(FILE),
            program,
            _dir: dir,
        }
    }

    fn clock() -> FakeClock {
        FakeClock::starting_at("2026-11-13T10:00:00Z").unwrap()
    }

    #[test]
    fn a_version_kept_is_found_while_the_shell_is_the_same() {
        let place = place("same");
        let stamp = Stamp::of(&place.program).unwrap();
        assert_eq!(lookup(&place.file, &stamp), None);
        store(&place.file, &stamp, "7.4.6", &clock());
        assert_eq!(lookup(&place.file, &stamp), Some("7.4.6".to_owned()));
    }

    #[test]
    fn a_changed_size_or_time_or_program_asks_again() {
        let place = place("changed");
        let stamp = Stamp::of(&place.program).unwrap();
        store(&place.file, &stamp, "7.4.6", &clock());
        let other_size = Stamp {
            size: stamp.size + 1,
            ..stamp.clone()
        };
        let other_time = Stamp {
            modified: format!("{}1", stamp.modified),
            ..stamp.clone()
        };
        let other_program = Stamp {
            program: place.program.with_file_name("bash"),
            ..stamp.clone()
        };
        for changed in [other_size, other_time, other_program] {
            assert_eq!(lookup(&place.file, &changed), None, "{changed:?}");
        }
        // A newer answer for the same program replaces the old one.
        let updated = Stamp {
            size: stamp.size + 1,
            ..stamp.clone()
        };
        store(&place.file, &updated, "7.5.0", &clock());
        assert_eq!(lookup(&place.file, &updated), Some("7.5.0".to_owned()));
        assert_eq!(lookup(&place.file, &stamp), None);
    }

    #[test]
    fn a_file_that_is_not_understood_is_a_miss() {
        let place = place("corrupt");
        let stamp = Stamp::of(&place.program).unwrap();
        for content in [
            "not json".to_owned(),
            json!({"shape": 2, "shells": []}).to_string(),
            json!({"shape": 1, "shells": [{"program": stamp.program.to_str(),
                "size": stamp.size, "modified": stamp.modified,
                "version": "7\u{1b}[2K; told the model"}]})
            .to_string(),
        ] {
            std::fs::write(&place.file, content).unwrap();
            assert_eq!(lookup(&place.file, &stamp), None);
        }
        // And it is replaced by the next answer.
        store(&place.file, &stamp, "7.4.6", &clock());
        assert_eq!(lookup(&place.file, &stamp), Some("7.4.6".to_owned()));
    }

    #[test]
    fn the_file_keeps_the_newest_sixteen_shells_and_is_private() {
        let place = place("kept");
        for index in 0..20 {
            let stamp = Stamp {
                program: place.program.with_file_name(format!("shell{index}")),
                size: 1,
                modified: "1".to_owned(),
            };
            store(&place.file, &stamp, "1.0", &clock());
        }
        assert_eq!(read(&place.file).unwrap().len(), KEPT);
        assert!(cortex_base::private::is_private(&place.file).unwrap());
    }
}
