//! The pseudo-terminal driver: a program run in a terminal of its own,
//! its output recorded with the time each piece arrived and parsed by a
//! [`VTerm`], keys typed into it, and waits on what its screen shows.
//!
//! The operating system's half (opening the pseudo-terminal, ConPTY on
//! Windows) is `cortex_term::pty`, where the crate's `unsafe` code is.
//!
//! # Waiting
//!
//! A test never sleeps. [`Session::wait_for`] reads the output as it
//! arrives and returns at the first piece after which the screen shows what
//! the test is waiting for, with the time that piece arrived. Its limit is a
//! bound that turns a mistake into a failed test, and the failure shows the
//! screen.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "test support: it starts a program and reads and writes its terminal from threads"
)]

use std::io::{Read, Write};
use std::process::ExitStatus;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cortex_term::pty::{Pty, is_end};

use crate::vterm::{VTerm, answer};

/// One piece of output, as one read returned it.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// When the read returned.
    pub at: Instant,
    /// What it returned.
    pub bytes: Vec<u8>,
}

/// A program running in a pseudo-terminal, and what it has shown.
pub struct Session {
    pty: Pty,
    writer: Box<dyn Write + Send>,
    chunks: Receiver<Option<Chunk>>,
    reader: Option<JoinHandle<()>>,
    term: VTerm,
    recording: Vec<Chunk>,
    started: Instant,
    ended: bool,
    answering: bool,
    exit: Option<ExitStatus>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("pid", &self.pty.pid())
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Starts `program` with `arguments` in a terminal of `cols` by `rows`.
    ///
    /// # Panics
    ///
    /// When it cannot be started.
    #[must_use]
    pub fn start(program: &str, arguments: &[String], cols: u16, rows: u16) -> Self {
        let started = Instant::now();
        let pty = Pty::spawn(program, arguments, &[], cols, rows)
            .unwrap_or_else(|error| panic!("start {program} in a pseudo-terminal: {error}"));
        let mut reader = pty.reader().expect("the pseudo-terminal's output");
        let writer = pty.writer().expect("the pseudo-terminal's input");
        let (send, chunks) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut buffer = vec![0u8; 65536];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let chunk = Chunk {
                            at: Instant::now(),
                            bytes: buffer[..count].to_vec(),
                        };
                        if send.send(Some(chunk)).is_err() {
                            return;
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    // The other end has gone (`is_end`), or the read
                    // failed: either way the output is over.
                    Err(error) => {
                        let _ = is_end(&error);
                        break;
                    }
                }
            }
            let _ = send.send(None);
        });
        Self {
            pty,
            writer,
            chunks,
            reader: Some(reader),
            term: VTerm::new(cols, rows),
            recording: Vec::new(),
            started,
            ended: false,
            answering: false,
            exit: None,
        }
    }

    /// Answers the program's queries as a modern terminal would
    /// ([`answer`]), from now on.
    #[must_use]
    pub fn answering(mut self) -> Self {
        self.answering = true;
        self
    }

    /// When the program was started.
    #[must_use]
    pub fn started(&self) -> Instant {
        self.started
    }

    /// The program's process id.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pty.pid()
    }

    /// The screen as parsed so far.
    #[must_use]
    pub fn screen(&self) -> &VTerm {
        &self.term
    }

    /// Everything the program wrote, as it arrived.
    #[must_use]
    pub fn recording(&self) -> &[Chunk] {
        &self.recording
    }

    /// Types `bytes`, and returns when they were handed to the terminal.
    ///
    /// # Panics
    ///
    /// When the terminal cannot be written to.
    pub fn send(&mut self, bytes: &[u8]) -> Instant {
        let at = Instant::now();
        self.writer
            .write_all(bytes)
            .and_then(|()| self.writer.flush())
            .expect("type into the pseudo-terminal");
        at
    }

    /// Resizes the terminal, as a window being resized.
    ///
    /// # Panics
    ///
    /// When the system refuses.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.pty
            .resize(cols, rows)
            .expect("resize the pseudo-terminal");
        self.term.resize(cols, rows);
    }

    fn take(&mut self, chunk: Chunk) {
        self.term.feed(&chunk.bytes);
        self.recording.push(chunk);
        if self.answering {
            for query in self.term.take_queries() {
                let reply = answer(query, &self.term);
                let _ = self.writer.write_all(&reply);
            }
        }
    }

    /// Reads output for at most `limit`, until `condition` holds of the
    /// screen, and returns when the piece arrived after which it held. A
    /// condition that already holds returns the time of the last piece read.
    ///
    /// # Panics
    ///
    /// When the time runs out or the output ends first, with the screen.
    pub fn wait_for(
        &mut self,
        limit: Duration,
        what: &str,
        mut condition: impl FnMut(&VTerm) -> bool,
    ) -> Instant {
        if condition(&self.term) {
            return self.recording.last().map_or(self.started, |chunk| chunk.at);
        }
        let deadline = Instant::now() + limit;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.chunks.recv_timeout(left) {
                Ok(Some(chunk)) => {
                    let at = chunk.at;
                    self.take(chunk);
                    if condition(&self.term) {
                        return at;
                    }
                }
                Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                    self.ended = true;
                    panic!(
                        "the output ended before {what}; the screen:\n{}",
                        self.term.dump()
                    );
                }
                Err(RecvTimeoutError::Timeout) => panic!(
                    "waited {limit:?} for {what}, and it did not happen; the screen:\n{}",
                    self.term.dump()
                ),
            }
        }
    }

    /// Reads the next piece of output, waiting at most `limit` for it, and
    /// returns when it arrived; `None` when the time ran out or the output
    /// has ended ([`ended`](Self::ended)).
    pub fn step(&mut self, limit: Duration) -> Option<Instant> {
        if self.ended {
            return None;
        }
        match self.chunks.recv_timeout(limit) {
            Ok(Some(chunk)) => {
                let at = chunk.at;
                self.take(chunk);
                Some(at)
            }
            Ok(None) | Err(RecvTimeoutError::Disconnected) => {
                self.ended = true;
                None
            }
            Err(RecvTimeoutError::Timeout) => None,
        }
    }

    /// Whether the output has ended: everything in the terminal closed it.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.ended
    }

    /// Reads whatever output has arrived, without waiting.
    pub fn pump(&mut self) {
        while let Ok(message) = self.chunks.try_recv() {
            match message {
                Some(chunk) => self.take(chunk),
                None => self.ended = true,
            }
        }
    }

    /// Waits at most `limit` for the program to end, reading its output
    /// meanwhile, and returns how it ended.
    ///
    /// # Panics
    ///
    /// When it does not end in time.
    pub fn wait_exit(&mut self, limit: Duration) -> ExitStatus {
        let deadline = Instant::now() + limit;
        loop {
            self.pump();
            if let Some(status) = self.pty.try_wait().expect("ask how the program ended") {
                self.exit = Some(status);
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "waited {limit:?} for the program to end; the screen:\n{}",
                self.term.dump()
            );
            let step = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(5));
            if let Ok(Some(chunk)) = self.chunks.recv_timeout(step) {
                self.take(chunk);
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.exit.is_none() && !matches!(self.pty.try_wait(), Ok(Some(_))) {
            let _ = self.pty.kill();
        }
        // The reader ends when the terminal closes, which on Windows is when
        // the pseudo-console is dropped: it is not waited for here.
        drop(self.reader.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_go_back_to_the_program() {
        // Exercised end to end by `cortex-term`'s system tests; here only the
        // pieces that need no process.
        let mut term = VTerm::new(10, 2);
        term.feed(b"\x1b[c");
        let queries = term.take_queries();
        assert_eq!(answer(queries[0], &term), b"\x1b[?62;22c");
    }
}
