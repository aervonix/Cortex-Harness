//! Reloading without a restart (`docs/architecture/08-configuration.md`).
//!
//! [`LiveConfig`] holds the configuration in force and notices when a file
//! it was loaded from has changed. There is no watcher and no timer: the
//! harness asks at the moments that page lists (the terminal regains focus,
//! a key press at most once a second, the start of a turn, `/reload`), and
//! asking is a handful of `stat` calls.
//!
//! **An invalid file never takes the configuration away.** When a reload
//! fails, the configuration that was in force stays in force and the errors
//! are returned to be shown. A harness that started with a valid
//! configuration always has one.

use std::sync::Arc;
use std::time::Duration;

use cortex_base::{SharedClock, TimedStamp, WatchList};

use crate::load::{Change, LoadFailure, LoadOptions, Loaded, load};

/// What a reload did.
#[derive(Debug, Clone, PartialEq)]
pub enum Reload {
    /// Nothing that is in effect changed. Not worth announcing: a file may
    /// have been saved without a change, or a comment edited.
    Unchanged,
    /// The new configuration is in force.
    Applied {
        /// What was in force before.
        previous: Arc<Loaded>,
        /// What differs, with when each change takes effect.
        changes: Vec<Change>,
    },
    /// The new configuration has an error. **The old one stays in force.**
    Rejected(LoadFailure),
}

/// The configuration in force, and the means to notice that it has changed
/// on disk.
#[derive(Debug)]
pub struct LiveConfig {
    options: LoadOptions,
    current: Arc<Loaded>,
    watch: WatchList,
    /// The failure of the last reload, so that the same broken file is
    /// reported once and not on every check.
    rejected: Option<LoadFailure>,
}

impl LiveConfig {
    /// Loads configuration for the first time.
    ///
    /// # Errors
    ///
    /// The [`LoadFailure`] when it cannot be loaded. There is then no
    /// configuration to fall back to, and the caller reports it and stops.
    pub fn start(options: LoadOptions, clock: SharedClock) -> Result<Self, LoadFailure> {
        let mut watch = WatchList::new(clock);
        let stamps = stamps(&options, &watch);
        let current = Arc::new(load(&options)?);
        register(&mut watch, stamps);
        Ok(Self {
            options,
            current,
            watch,
            rejected: None,
        })
    }

    /// The configuration in force.
    #[must_use]
    pub fn current(&self) -> Arc<Loaded> {
        Arc::clone(&self.current)
    }

    /// What the configuration is loaded from.
    #[must_use]
    pub fn options(&self) -> &LoadOptions {
        &self.options
    }

    /// Changes what the configuration is loaded from (another profile, the
    /// project now trusted) and reloads.
    pub fn update(&mut self, change: impl FnOnce(&mut LoadOptions)) -> Reload {
        change(&mut self.options);
        self.reload()
    }

    /// Looks at the files and reloads if one changed. `None` when none did.
    pub fn check(&mut self) -> Option<Reload> {
        if self.watch.check().is_empty() {
            None
        } else {
            Some(self.reload())
        }
    }

    /// [`check`](Self::check), unless a check ran less than `interval` ago.
    /// For the check on a key press, which runs at most once a second.
    pub fn check_at_most_every(&mut self, interval: Duration) -> Option<Reload> {
        if self.watch.check_at_most_every(interval)?.is_empty() {
            None
        } else {
            Some(self.reload())
        }
    }

    /// Loads again, whether or not a file changed: `/reload`.
    pub fn reload(&mut self) -> Reload {
        // The stamps are taken before the files are read, so that a change
        // that lands during the read is seen by the next check.
        let stamps = stamps(&self.options, &self.watch);
        let loaded = load(&self.options);
        // The files are stamped afresh whatever the outcome: a file that is
        // broken is reported now, and again only when it changes again.
        self.watch.clear();
        register(&mut self.watch, stamps);
        match loaded {
            Ok(loaded) => {
                self.rejected = None;
                let changes = loaded.changes_since(&self.current);
                let previous = std::mem::replace(&mut self.current, Arc::new(loaded));
                if changes.is_empty() {
                    Reload::Unchanged
                } else {
                    Reload::Applied { previous, changes }
                }
            }
            Err(failure) => {
                if self.rejected.as_ref() == Some(&failure) {
                    return Reload::Unchanged;
                }
                self.rejected = Some(failure.clone());
                Reload::Rejected(failure)
            }
        }
    }
}

fn stamps(options: &LoadOptions, watch: &WatchList) -> Vec<(std::path::PathBuf, TimedStamp)> {
    options
        .files()
        .into_iter()
        .map(|(_, file)| {
            let stamp = watch.stamp(&file);
            (file, stamp)
        })
        .collect()
}

fn register(watch: &mut WatchList, stamps: Vec<(std::path::PathBuf, TimedStamp)>) {
    for (file, stamp) in stamps {
        watch.watch_with(file, stamp);
    }
}
