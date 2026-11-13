//! The log file and the thread that writes it
//! (`docs/architecture/02-runtime.md`, "Sinks").
//!
//! A line is handed to the writer through a bounded queue and written by a
//! thread of its own, so that logging never makes the core thread wait for
//! a disk. When the queue is full the line is dropped and counted, and the
//! count is written when there is room again: a log that says it lost
//! lines is better than a session that stalls.
//!
//! **Rotation by size.** The file is `cortex.log`. When a line would take
//! it past its size limit it becomes `cortex.log.1`, the file that had
//! that name becomes `cortex.log.2`, and so on; the oldest is removed.
//! With the defaults that is five files of 5 MB.
//!
//! **Owner-only.** Every file is created through [`crate::private`], so it
//! is private from the moment it exists, and an existing file keeps what it
//! was given.
//!
//! **Several processes can write one directory.** Each appends whole
//! lines. Two that rotate at the same moment can move a file twice, which
//! loses nothing but the order of the numbered files; the harness accepts
//! that rather than take a lock on every line.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "blocking file calls on the log-writer thread, which exists to make them"
)]

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

use crate::private;

/// The log file's name.
pub const FILE_NAME: &str = "cortex.log";
/// How large one file may grow before it is rotated.
pub const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// How many files are kept, the one being written included.
pub const DEFAULT_FILES: usize = 5;
/// How many lines may wait for the writer.
const QUEUE: usize = 4096;

/// Where the log file is and how it is rotated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOptions {
    /// The directory: `logs/` in the state directory.
    pub dir: PathBuf,
    /// How large one file may grow.
    pub max_bytes: u64,
    /// How many files are kept, the one being written included. At least
    /// one.
    pub files: usize,
}

impl FileOptions {
    /// The default rotation in `dir`.
    #[must_use]
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            max_bytes: DEFAULT_MAX_BYTES,
            files: DEFAULT_FILES,
        }
    }

    /// The file being written.
    #[must_use]
    pub fn current(&self) -> PathBuf {
        self.dir.join(FILE_NAME)
    }

    /// The `n`th rotated file, 1 being the newest.
    #[must_use]
    pub fn rotated(&self, n: usize) -> PathBuf {
        self.dir.join(format!("{FILE_NAME}.{n}"))
    }
}

enum Message {
    Line(Vec<u8>),
    /// Write what is queued, and answer when it is written.
    Flush(SyncSender<()>),
}

/// The sending side of the log-writer thread. Clones send to the same
/// thread.
#[derive(Debug, Clone)]
pub(crate) struct FileSink {
    queue: SyncSender<Message>,
    dropped: Arc<AtomicU64>,
}

impl FileSink {
    /// Creates the directory, opens the file and starts the thread.
    ///
    /// # Errors
    ///
    /// When the directory cannot be made or the file cannot be opened.
    pub(crate) fn start(options: FileOptions) -> io::Result<(Self, JoinHandle<()>)> {
        private::create_dir_all(&options.dir)?;
        let mut rotating = Rotating::open(options)?;
        let (queue, lines) = sync_channel(QUEUE);
        let dropped = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&dropped);
        let thread = std::thread::Builder::new()
            .name("cortex-log".to_owned())
            .spawn(move || write_lines(&lines, &mut rotating, &counted))?;
        Ok((Self { queue, dropped }, thread))
    }

    /// Hands a line, newline included, to the writer. Never waits: with the
    /// queue full the line is dropped and counted.
    pub(crate) fn send(&self, line: Vec<u8>) {
        if let Err(TrySendError::Full(_)) = self.queue.try_send(Message::Line(line)) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Waits until everything handed over so far is written. For the end of
    /// the process, and for tests; never called on the core thread while a
    /// session runs.
    pub(crate) fn flush(&self) {
        let (done, wait) = sync_channel(1);
        if self.queue.send(Message::Flush(done)).is_ok() {
            // An error is the writer gone, and then there is nothing to
            // wait for.
            let _ = wait.recv();
        }
    }

    /// How many lines were dropped and not yet reported in the file.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// The writer thread: takes lines until every sender is gone.
fn write_lines(lines: &Receiver<Message>, file: &mut Rotating, dropped: &AtomicU64) {
    let mut batch = Vec::new();
    while let Ok(first) = lines.recv() {
        let mut flushes = Vec::new();
        let mut next = Some(first);
        // Everything that is waiting goes out in one write.
        while let Some(message) = next {
            match message {
                Message::Line(line) => batch.push(line),
                Message::Flush(done) => flushes.push(done),
            }
            next = lines.try_recv().ok();
        }
        let lost = dropped.swap(0, Ordering::Relaxed);
        if lost > 0 {
            batch.push(
                format!(
                    "{{\"level\":\"warn\",\"target\":\"cortex_base::log\",\"msg\":\"{lost} log \
                     lines were dropped: the log writer could not keep up\"}}\n"
                )
                .into_bytes(),
            );
        }
        for line in batch.drain(..) {
            // A log that cannot be written has nowhere to say so.
            let _ = file.write_line(&line);
        }
        for done in flushes {
            let _ = done.send(());
        }
    }
}

/// The file being written, and its rotation.
struct Rotating {
    options: FileOptions,
    file: File,
    size: u64,
    /// The size at which rotation is tried next. It moves on when a
    /// rotation fails, so that a file that cannot be moved is not tried
    /// again for every line.
    rotate_at: u64,
}

impl Rotating {
    fn open(options: FileOptions) -> io::Result<Self> {
        let (file, size) = open_for_append(&options.current())?;
        let rotate_at = options.max_bytes;
        Ok(Self {
            options,
            file,
            size,
            rotate_at,
        })
    }

    fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        let length = line.len() as u64;
        if self.size > 0 && self.size + length > self.rotate_at {
            match self.rotate() {
                Ok(()) => self.rotate_at = self.options.max_bytes,
                // The line still goes into the file that is open.
                Err(_) => self.rotate_at = self.size + self.options.max_bytes / 8 + length,
            }
        }
        self.file.write_all(line)?;
        self.size += length;
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        let keep = self.options.files.max(1) - 1;
        if keep == 0 {
            // One file only: it starts again.
            remove_if_there(&self.options.current())?;
        } else {
            remove_if_there(&self.options.rotated(keep))?;
            for n in (1..keep).rev() {
                rename_if_there(&self.options.rotated(n), &self.options.rotated(n + 1))?;
            }
            rename_if_there(&self.options.current(), &self.options.rotated(1))?;
        }
        let (file, size) = open_for_append(&self.options.current())?;
        self.file = file;
        self.size = size;
        Ok(())
    }
}

/// Opens the file for appending, creating it owner-only when it is not
/// there, and says how large it is.
fn open_for_append(path: &Path) -> io::Result<(File, u64)> {
    let open = || OpenOptions::new().append(true).open(path);
    let file = match open() {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            match private::create_new(path) {
                // Another process made it in between: that file is the log.
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            open()?
        }
        Err(error) => return Err(error),
    };
    let size = file.metadata()?.len();
    Ok((file, size))
}

fn remove_if_there(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn rename_if_there(from: &Path, to: &Path) -> io::Result<()> {
    match std::fs::rename(from, to) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("cortex-base-log")
            .join(format!("{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn lines_reach_the_file_in_order_and_the_file_is_private() {
        let dir = scratch("order");
        let options = FileOptions::in_dir(dir.join("logs"));
        let (sink, thread) = FileSink::start(options.clone()).unwrap();
        for n in 0..100 {
            sink.send(format!("line {n}\n").into_bytes());
        }
        sink.flush();
        let text = read(&options.current());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 100);
        assert_eq!(lines[0], "line 0");
        assert_eq!(lines[99], "line 99");
        assert!(private::is_private(&options.current()).unwrap());
        assert_eq!(sink.dropped(), 0);
        // The thread ends when the last sender is gone.
        drop(sink);
        thread.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_file_is_appended_to() {
        let dir = scratch("append");
        let options = FileOptions::in_dir(&dir);
        for run in 0..2 {
            let (sink, thread) = FileSink::start(options.clone()).unwrap();
            sink.send(format!("run {run}\n").into_bytes());
            drop(sink);
            thread.join().unwrap();
        }
        assert_eq!(read(&options.current()), "run 0\nrun 1\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_file_rotates_by_size_and_the_oldest_is_removed() {
        let dir = scratch("rotate");
        let options = FileOptions {
            dir: dir.clone(),
            // Room for two lines of ten bytes.
            max_bytes: 20,
            files: 3,
        };
        private::create_dir_all(&options.dir).unwrap();
        let mut file = Rotating::open(options.clone()).unwrap();
        for n in 0..9 {
            file.write_line(format!("line {n:04}\n").as_bytes())
                .unwrap();
        }
        // Nine lines, two to a file, three files kept: the first three
        // lines are gone, and no file is over its size.
        assert_eq!(read(&options.current()), "line 0008\n");
        assert_eq!(read(&options.rotated(1)), "line 0006\nline 0007\n");
        assert_eq!(read(&options.rotated(2)), "line 0004\nline 0005\n");
        assert!(!options.rotated(3).exists());
        assert!(private::is_private(&options.rotated(2)).unwrap());
        assert!(private::is_private(&options.current()).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_longer_than_a_file_still_goes_in_whole() {
        let dir = scratch("long");
        let options = FileOptions {
            dir: dir.clone(),
            max_bytes: 8,
            files: 2,
        };
        private::create_dir_all(&options.dir).unwrap();
        let mut file = Rotating::open(options.clone()).unwrap();
        file.write_line(b"short\n").unwrap();
        file.write_line(b"a line longer than the limit\n").unwrap();
        file.write_line(b"next\n").unwrap();
        assert_eq!(read(&options.current()), "next\n");
        assert_eq!(read(&options.rotated(1)), "a line longer than the limit\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_one_file_kept_the_file_starts_again() {
        let dir = scratch("one");
        let options = FileOptions {
            dir: dir.clone(),
            max_bytes: 12,
            files: 1,
        };
        private::create_dir_all(&options.dir).unwrap();
        let mut file = Rotating::open(options.clone()).unwrap();
        for text in ["first\n", "second\n", "third\n"] {
            file.write_line(text.as_bytes()).unwrap();
        }
        assert_eq!(read(&options.current()), "third\n");
        assert!(!options.rotated(1).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_queue_drops_lines_and_says_how_many() {
        // No thread reads this queue, so it fills.
        let (queue, lines) = sync_channel(2);
        let sink = FileSink {
            queue,
            dropped: Arc::new(AtomicU64::new(0)),
        };
        for n in 0..5 {
            sink.send(format!("line {n}\n").into_bytes());
        }
        assert_eq!(sink.dropped(), 3);

        // The writer reports the count once it writes again.
        let dir = scratch("dropped");
        let options = FileOptions::in_dir(&dir);
        private::create_dir_all(&options.dir).unwrap();
        let mut file = Rotating::open(options.clone()).unwrap();
        let dropped = Arc::clone(&sink.dropped);
        drop(sink);
        write_lines(&lines, &mut file, &dropped);
        let text = read(&options.current());
        assert!(text.starts_with("line 0\nline 1\n"), "{text}");
        assert!(text.contains("3 log lines were dropped"), "{text}");
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
