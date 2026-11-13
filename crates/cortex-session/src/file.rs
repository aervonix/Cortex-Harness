//! The log file: appending one line at a time, so that memory and disk
//! always agree.
//!
//! The file is opened for writing and each line is written at the end the
//! writer keeps track of. It is not opened in append mode: on Windows a file
//! opened for appending cannot be cut back, and cutting back is how a failed
//! write is undone. One process writes a log at a time (it holds the
//! session's lock), so the end the writer knows is the file's end.

use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::PathBuf;

use crate::error::{Result, SessionError};

/// When the log is flushed to the disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Durability {
    /// After every entry: a crash loses at most the entry being written.
    /// The default.
    #[default]
    Item,
    /// At the end of every turn ([`Session::end_turn`](crate::Session::end_turn)):
    /// a crash of the machine can lose the turn in progress.
    Turn,
    /// Never: the operating system writes the log when it chooses. A crash
    /// of the program loses nothing; a crash of the machine can lose
    /// anything not yet written.
    Never,
}

impl Durability {
    /// The setting's name in configuration: `item`, `turn` or `never`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Item => "item",
            Self::Turn => "turn",
            Self::Never => "never",
        }
    }

    /// Reads a name [`as_str`](Self::as_str) writes.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "item" => Some(Self::Item),
            "turn" => Some(Self::Turn),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

/// The three things the writer does to the file. A test gives the writer a
/// file that fails at any of them, at any byte.
pub(crate) trait LogFile: Send + std::fmt::Debug {
    /// Writes all of `bytes` at `offset`.
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()>;
    /// Cuts the file back to `len` bytes.
    fn truncate(&mut self, len: u64) -> io::Result<()>;
    /// Flushes what was written to the disk.
    fn sync(&mut self) -> io::Result<()>;
}

/// The log file on disk.
#[derive(Debug)]
pub(crate) struct DiskFile(pub(crate) File);

impl LogFile for DiskFile {
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.0.seek(SeekFrom::Start(offset))?;
        self.0.write_all(bytes)
    }

    fn truncate(&mut self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }

    fn sync(&mut self) -> io::Result<()> {
        self.0.sync_data()
    }
}

/// Appends lines to a log, one complete line per write.
#[derive(Debug)]
pub(crate) struct Writer {
    file: Box<dyn LogFile>,
    path: PathBuf,
    /// Where the next line goes: the length of what is on disk.
    end: u64,
    durability: Durability,
    /// A write failed and could not be undone. Nothing more is written.
    broken: bool,
    /// Lines written since the last flush.
    unsynced: bool,
}

impl Writer {
    pub(crate) fn new(
        file: Box<dyn LogFile>,
        path: PathBuf,
        end: u64,
        durability: Durability,
    ) -> Self {
        Self {
            file,
            path,
            end,
            durability,
            broken: false,
            unsynced: false,
        }
    }

    /// Where the next line goes.
    #[cfg(test)]
    pub(crate) fn end(&self) -> u64 {
        self.end
    }

    /// Whether a failed write left the file in a state this writer cannot
    /// vouch for.
    pub(crate) fn is_broken(&self) -> bool {
        self.broken
    }

    /// Writes `line` at the end and returns where it starts.
    ///
    /// On success the line is on disk (and flushed, with
    /// [`Durability::Item`]). On failure the file is cut back to where it
    /// was, so the line is not there at all. If cutting back fails too, the
    /// writer refuses every later write ([`SessionError::Broken`]): the
    /// session must be opened again, which repairs the tail.
    pub(crate) fn append(&mut self, line: &[u8]) -> Result<u64> {
        if self.broken {
            return Err(SessionError::Broken);
        }
        let start = self.end;
        let written = self.file.write_at(start, line).and_then(|()| {
            if self.durability == Durability::Item {
                self.file.sync()
            } else {
                Ok(())
            }
        });
        match written {
            Ok(()) => {
                self.end = start + line.len() as u64;
                self.unsynced = self.durability != Durability::Item;
                Ok(start)
            }
            Err(error) => {
                if self.file.truncate(start).is_err() {
                    self.broken = true;
                    return Err(SessionError::Broken);
                }
                // The cut is flushed where it matters: a line that reached
                // the disk before the failure must not reappear after a
                // crash. With `never`, nothing is flushed, by choice.
                if self.durability != Durability::Never && self.file.sync().is_err() {
                    self.broken = true;
                    return Err(SessionError::Broken);
                }
                Err(SessionError::Io {
                    action: "write the session log",
                    path: self.path.clone(),
                    source: error,
                })
            }
        }
    }

    /// Flushes everything written so far. Used at the end of a turn with
    /// [`Durability::Turn`]. A failure is reported and removes nothing:
    /// the lines were written, and whether they reach the disk is now the
    /// operating system's.
    pub(crate) fn flush(&mut self) -> Result<()> {
        if self.broken || !self.unsynced || self.durability == Durability::Never {
            return Ok(());
        }
        self.file
            .sync()
            .map_err(SessionError::io("flush the session log", self.path.clone()))?;
        self.unsynced = false;
        Ok(())
    }
}

/// Reads `len` bytes at `offset` without moving any file position, so that
/// any number of readers can share one handle.
pub(crate) fn read_at(file: &File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; len];
    let mut done = 0;
    while done < len {
        #[cfg(unix)]
        let read =
            std::os::unix::fs::FileExt::read_at(file, &mut buffer[done..], offset + done as u64)?;
        #[cfg(windows)]
        let read = std::os::windows::fs::FileExt::seek_read(
            file,
            &mut buffer[done..],
            offset + done as u64,
        )?;
        if read == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        done += read;
    }
    Ok(buffer)
}

#[cfg(test)]
pub(crate) mod fake {
    //! A file in memory that fails when told to.

    use std::sync::{Arc, Mutex, PoisonError};

    use super::*;

    /// What the fake file does on the next operations.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub(crate) struct Faults {
        /// The next write stores this many bytes and then fails.
        pub(crate) write_fails_after: Option<usize>,
        /// The next flush fails.
        pub(crate) sync_fails: bool,
        /// The next cut fails.
        pub(crate) truncate_fails: bool,
    }

    /// The disk: what is "on" it, shared with the test.
    #[derive(Debug, Clone, Default)]
    pub(crate) struct FakeDisk {
        pub(crate) bytes: Arc<Mutex<Vec<u8>>>,
        pub(crate) faults: Arc<Mutex<Faults>>,
    }

    impl FakeDisk {
        pub(crate) fn contents(&self) -> Vec<u8> {
            self.bytes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        pub(crate) fn fail(&self, faults: Faults) {
            *self.faults.lock().unwrap_or_else(PoisonError::into_inner) = faults;
        }

        pub(crate) fn file(&self) -> Box<dyn LogFile> {
            Box::new(self.clone())
        }
    }

    impl LogFile for FakeDisk {
        fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
            let mut faults = self.faults.lock().unwrap_or_else(PoisonError::into_inner);
            let mut bytes = self.bytes.lock().unwrap_or_else(PoisonError::into_inner);
            let offset = usize::try_from(offset).expect("small");
            let (data, fails) = match faults.write_fails_after.take() {
                Some(after) => (&data[..after.min(data.len())], true),
                None => (data, false),
            };
            // A write at an offset overwrites, as a real file does.
            let end = offset + data.len();
            if bytes.len() < end {
                bytes.resize(end, 0);
            }
            bytes[offset..end].copy_from_slice(data);
            if fails {
                return Err(io::Error::other("the disk is full"));
            }
            Ok(())
        }

        fn truncate(&mut self, len: u64) -> io::Result<()> {
            let mut faults = self.faults.lock().unwrap_or_else(PoisonError::into_inner);
            if std::mem::take(&mut faults.truncate_fails) {
                return Err(io::Error::other("the disk went away"));
            }
            self.bytes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .truncate(usize::try_from(len).expect("small"));
            Ok(())
        }

        fn sync(&mut self) -> io::Result<()> {
            let mut faults = self.faults.lock().unwrap_or_else(PoisonError::into_inner);
            if std::mem::take(&mut faults.sync_fails) {
                return Err(io::Error::other("the flush failed"));
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{FakeDisk, Faults};
    use super::*;

    fn writer(disk: &FakeDisk, durability: Durability) -> Writer {
        Writer::new(disk.file(), PathBuf::from("log.jsonl"), 0, durability)
    }

    #[test]
    fn lines_go_one_after_another() {
        let disk = FakeDisk::default();
        let mut writer = writer(&disk, Durability::Item);
        assert_eq!(writer.append(b"one\n").unwrap(), 0);
        assert_eq!(writer.append(b"two\n").unwrap(), 4);
        assert_eq!(disk.contents(), b"one\ntwo\n");
    }

    #[test]
    fn a_failed_write_at_any_byte_leaves_the_file_as_it_was() {
        let line = b"{\"a\":\"\xc3\xa9t\xc3\xa9\"}\n";
        for durability in [Durability::Item, Durability::Turn, Durability::Never] {
            for after in 0..=line.len() {
                let disk = FakeDisk::default();
                let mut writer = writer(&disk, durability);
                writer.append(b"first\n").unwrap();
                disk.fail(Faults {
                    write_fails_after: Some(after),
                    ..Faults::default()
                });
                assert!(writer.append(line).is_err());
                assert_eq!(disk.contents(), b"first\n", "{durability:?} after {after}");
                assert!(!writer.is_broken());
                assert_eq!(writer.append(b"next\n").unwrap(), 6);
                assert_eq!(disk.contents(), b"first\nnext\n");
            }
        }
    }

    #[test]
    fn a_failed_flush_takes_the_line_back_out() {
        let disk = FakeDisk::default();
        let mut writer = writer(&disk, Durability::Item);
        writer.append(b"first\n").unwrap();
        disk.fail(Faults {
            sync_fails: true,
            ..Faults::default()
        });
        assert!(writer.append(b"second\n").is_err());
        assert_eq!(disk.contents(), b"first\n");
        assert_eq!(writer.end(), 6);
    }

    #[test]
    fn a_write_that_cannot_be_undone_stops_all_writing() {
        let disk = FakeDisk::default();
        let mut writer = writer(&disk, Durability::Item);
        writer.append(b"first\n").unwrap();
        disk.fail(Faults {
            write_fails_after: Some(3),
            truncate_fails: true,
            ..Faults::default()
        });
        assert!(matches!(
            writer.append(b"second\n"),
            Err(SessionError::Broken)
        ));
        assert!(writer.is_broken());
        let before = disk.contents();
        assert!(matches!(
            writer.append(b"third\n"),
            Err(SessionError::Broken)
        ));
        assert_eq!(disk.contents(), before, "nothing more is written");
    }

    #[test]
    fn durability_names_round_trip() {
        for durability in [Durability::Item, Durability::Turn, Durability::Never] {
            assert_eq!(Durability::from_name(durability.as_str()), Some(durability));
        }
        assert_eq!(Durability::from_name("always"), None);
        assert_eq!(Durability::default(), Durability::Item);
    }
}
