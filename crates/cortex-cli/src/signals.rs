//! Signals in a headless run (`docs/architecture/10-headless.md`,
//! "Signals"; `docs/architecture/02-runtime.md`, "Cancellation").
//!
//! They are watched on the main thread, on a runtime of its own, so that a
//! core thread that is busy, or stuck, cannot keep the process from being
//! stopped. What each one means:
//!
//! | Signal | Then |
//! |---|---|
//! | The first, of either kind | The turn is interrupted. The run writes its record and exits with 130 or 143. |
//! | A second SIGINT (Ctrl-C) | `turn/interrupt` with `now`: running commands are killed at once. |
//! | A third, or any after a SIGTERM | The process exits at once. |
//!
//! On Windows, Ctrl-C is SIGINT, and Ctrl-Break and the console's close
//! event are SIGTERM. A process with no console gets none of them; it is
//! stopped by being ended, and the Job Object ends its commands with it.

use tokio::sync::mpsc::UnboundedSender;

use crate::exit::Signal;

/// What the driver is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Interrupt {
    /// The signal.
    pub(crate) signal: Signal,
    /// Whether to kill running commands at once.
    pub(crate) now: bool,
}

/// What to do about one more signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Act {
    /// Tell the driver.
    Tell(Interrupt),
    /// Exit at once, with this code.
    Exit(u8),
}

/// The signals seen so far, and what each new one means.
#[derive(Debug, Default)]
pub(crate) struct Counter {
    seen: u32,
    terminated: bool,
}

impl Counter {
    /// What `signal` means, after the ones seen before it.
    pub(crate) fn next(&mut self, signal: Signal) -> Act {
        self.seen += 1;
        let after_terminate = self.terminated;
        self.terminated |= signal == Signal::Terminate;
        match self.seen {
            1 => Act::Tell(Interrupt { signal, now: false }),
            2 if signal == Signal::Interrupt && !after_terminate => {
                Act::Tell(Interrupt { signal, now: true })
            }
            _ => Act::Exit(signal.code()),
        }
    }
}

/// Watches for signals until the process ends, telling `driver`, and ends
/// the process itself when the table says so. Returns only when signals
/// cannot be watched at all (a process with no console on Windows): the
/// run then goes on without them.
pub(crate) async fn watch(driver: UnboundedSender<Interrupt>) {
    let Some(mut source) = Source::open() else {
        return;
    };
    let mut counter = Counter::default();
    loop {
        let Some(signal) = source.next().await else {
            return;
        };
        match counter.next(signal) {
            Act::Tell(interrupt) => {
                // A driver that is gone has finished: the process is about
                // to end by itself.
                let _ = driver.send(interrupt);
            }
            Act::Exit(code) => {
                #[expect(
                    clippy::exit,
                    reason = "a repeated signal ends the process at once, as documented; the \
                              guard and the Job Object end its commands"
                )]
                fn exit_now(code: u8) -> ! {
                    std::process::exit(i32::from(code))
                }
                tracing::warn!(code, "exiting at once, on a repeated signal");
                exit_now(code);
            }
        }
    }
}

#[cfg(unix)]
struct Source {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Source {
    fn open() -> Option<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        let opened = signal(SignalKind::interrupt())
            .and_then(|interrupt| Ok((interrupt, signal(SignalKind::terminate())?)));
        match opened {
            Ok((interrupt, terminate)) => Some(Self {
                interrupt,
                terminate,
            }),
            Err(error) => {
                tracing::warn!(%error, "signals cannot be watched; Ctrl-C ends the process");
                None
            }
        }
    }

    async fn next(&mut self) -> Option<Signal> {
        let interrupt = &mut self.interrupt;
        let terminate = &mut self.terminate;
        std::future::poll_fn(|context| {
            if let std::task::Poll::Ready(got) = interrupt.poll_recv(context) {
                return std::task::Poll::Ready(got.map(|()| Signal::Interrupt));
            }
            terminate
                .poll_recv(context)
                .map(|got| got.map(|()| Signal::Terminate))
        })
        .await
    }
}

#[cfg(windows)]
struct Source {
    ctrl_c: tokio::signal::windows::CtrlC,
    ctrl_break: tokio::signal::windows::CtrlBreak,
    close: Option<tokio::signal::windows::CtrlClose>,
}

#[cfg(windows)]
impl Source {
    fn open() -> Option<Self> {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close};
        let opened = ctrl_c().and_then(|ctrl_c| Ok((ctrl_c, ctrl_break()?)));
        match opened {
            Ok((ctrl_c, ctrl_break)) => Some(Self {
                ctrl_c,
                ctrl_break,
                close: ctrl_close().ok(),
            }),
            Err(error) => {
                tracing::warn!(%error, "console events cannot be watched");
                None
            }
        }
    }

    async fn next(&mut self) -> Option<Signal> {
        let ctrl_c = &mut self.ctrl_c;
        let ctrl_break = &mut self.ctrl_break;
        let close = &mut self.close;
        std::future::poll_fn(|context| {
            if let std::task::Poll::Ready(got) = ctrl_c.poll_recv(context) {
                return std::task::Poll::Ready(got.map(|()| Signal::Interrupt));
            }
            if let std::task::Poll::Ready(got) = ctrl_break.poll_recv(context) {
                return std::task::Poll::Ready(got.map(|()| Signal::Terminate));
            }
            match close {
                Some(close) => close
                    .poll_recv(context)
                    .map(|got| got.map(|()| Signal::Terminate)),
                None => std::task::Poll::Pending,
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acts(signals: &[Signal]) -> Vec<Act> {
        let mut counter = Counter::default();
        signals.iter().map(|signal| counter.next(*signal)).collect()
    }

    const INT: Signal = Signal::Interrupt;
    const TERM: Signal = Signal::Terminate;

    #[test]
    fn a_second_ctrl_c_kills_now_and_a_third_exits() {
        assert_eq!(
            acts(&[INT, INT, INT]),
            [
                Act::Tell(Interrupt {
                    signal: INT,
                    now: false
                }),
                Act::Tell(Interrupt {
                    signal: INT,
                    now: true
                }),
                Act::Exit(130),
            ]
        );
    }

    #[test]
    fn a_second_sigterm_exits() {
        assert_eq!(
            acts(&[TERM, TERM]),
            [
                Act::Tell(Interrupt {
                    signal: TERM,
                    now: false
                }),
                Act::Exit(143),
            ]
        );
        // A Ctrl-C after a SIGTERM is not the gentler second step either.
        assert_eq!(acts(&[TERM, INT])[1], Act::Exit(130));
        // A SIGTERM after a Ctrl-C exits.
        assert_eq!(acts(&[INT, TERM])[1], Act::Exit(143));
    }
}
