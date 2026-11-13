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
//!
//! **Except for one thing: lowering trust always takes effect.** When the
//! user takes trust away from the project while a file has an error, the
//! project's layers leave the configuration in force all the same, and the
//! error is reported ([`Reload::ProjectDropped`]). Otherwise a trusted
//! repository could commit a broken file and make taking its trust away do
//! nothing.

use std::sync::Arc;
use std::time::Duration;

use cortex_base::{SharedClock, TimedStamp, WatchList};

use crate::load::{
    Change, FileLayer, LoadFailure, LoadOptions, Loaded, MissingProfile, ProjectTrust,
    load_layers, merge_layers,
};

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
    /// Trust in the project was lowered while a file has an error. The
    /// lowering is in force all the same: the configuration in force is
    /// the one that was, without the project's two files and the profiles
    /// they define. The error is to be shown; once it is fixed, the next
    /// check loads everything again, the project held to the trust rules.
    ProjectDropped {
        /// What was in force before.
        previous: Arc<Loaded>,
        /// What differs, with when each change takes effect.
        changes: Vec<Change>,
        /// Why the project's files could not simply be loaded again.
        failure: LoadFailure,
    },
}

/// The configuration in force, and the means to notice that it has changed
/// on disk.
#[derive(Debug)]
pub struct LiveConfig {
    options: LoadOptions,
    current: Arc<Loaded>,
    /// The file layers `current` was made from, so that the project's can
    /// be taken out of it without reading any file again.
    layers: Vec<FileLayer>,
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
        let (current, layers) = load_layers(&options)?;
        register(&mut watch, stamps);
        Ok(Self {
            options,
            current: Arc::new(current),
            layers,
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
    ///
    /// - **A change that loads** is applied, options and all.
    /// - **A change that lowers trust takes effect even when a file has an
    ///   error.** The project's layers are dropped from the configuration
    ///   in force, the rest of it stays as it was, and the error is
    ///   reported: [`Reload::ProjectDropped`]. Any other part of the same
    ///   change is undone, as below.
    /// - **Any other change that does not load is undone:** the options
    ///   stay as they were, the configuration in force stays, and the
    ///   answer is [`Reload::Rejected`], every time, even when the error is
    ///   one a check has reported already.
    pub fn update(&mut self, change: impl FnOnce(&mut LoadOptions)) -> Reload {
        let mut options = self.options.clone();
        change(&mut options);
        let lowers_trust =
            self.options.trust == ProjectTrust::Trusted && options.trust == ProjectTrust::Untrusted;
        // As for any reload, the stamps are taken before the files are read.
        let stamps = stamps(&options, &self.watch);
        match load_layers(&options) {
            Ok((loaded, layers)) => {
                self.options = options;
                self.watch.clear();
                register(&mut self.watch, stamps);
                self.accept(loaded, layers)
            }
            Err(failure) if lowers_trust => self.drop_project(failure),
            // The options and the files watched stay as they were, so a
            // file that changes is still noticed by the next check.
            Err(failure) => Reload::Rejected(failure),
        }
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
        let loaded = load_layers(&self.options);
        // The files are stamped afresh whatever the outcome: a file that is
        // broken is reported now, and again only when it changes again.
        self.watch.clear();
        register(&mut self.watch, stamps);
        match loaded {
            Ok((loaded, layers)) => self.accept(loaded, layers),
            Err(failure) => {
                if self.rejected.as_ref() == Some(&failure) {
                    return Reload::Unchanged;
                }
                self.rejected = Some(failure.clone());
                Reload::Rejected(failure)
            }
        }
    }

    /// Puts a configuration that loaded in force.
    fn accept(&mut self, loaded: Loaded, layers: Vec<FileLayer>) -> Reload {
        self.rejected = None;
        self.layers = layers;
        let changes = loaded.changes_since(&self.current);
        let previous = std::mem::replace(&mut self.current, Arc::new(loaded));
        if changes.is_empty() {
            Reload::Unchanged
        } else {
            Reload::Applied { previous, changes }
        }
    }

    /// Lowers trust although the files did not load: merges the layers in
    /// force again without the project's, under the options that were in
    /// force with trust lowered. No file is read, so the error in one
    /// cannot get in the way.
    fn drop_project(&mut self, failure: LoadFailure) -> Reload {
        let mut options = self.options.clone();
        options.trust = ProjectTrust::Untrusted;
        let layers: Vec<FileLayer> = self
            .layers
            .iter()
            .filter(|layer| !layer.kind.is_project())
            .cloned()
            .collect();
        let files = self
            .current
            .files
            .iter()
            .filter(|(kind, ..)| !kind.is_project())
            .cloned()
            .collect();
        // These layers merged before under these options, with the project
        // beside them. Taking a layer away can make one thing fail: a
        // profile that only the project defined, which is a warning here.
        match merge_layers(
            &options,
            &layers,
            files,
            Vec::new(),
            MissingProfile::Warning,
        ) {
            Ok(loaded) => {
                self.options = options;
                self.layers = layers;
                self.rejected = Some(failure.clone());
                let changes = loaded.changes_since(&self.current);
                let previous = std::mem::replace(&mut self.current, Arc::new(loaded));
                Reload::ProjectDropped {
                    previous,
                    changes,
                    failure,
                }
            }
            // Not reached: nothing else in a merge depends on which file
            // layers are there. Were it reached, nothing changes and the
            // error is reported, as for any change that does not load.
            Err(_) => Reload::Rejected(failure),
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
