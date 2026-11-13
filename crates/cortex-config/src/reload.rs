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
//! files in force are merged again with the project untrusted, so that
//! what needed trust leaves the configuration in force all the same, and
//! the error is reported ([`Reload::TrustLowered`]). Otherwise a trusted
//! repository could commit a broken file and make taking its trust away do
//! nothing.

use std::sync::Arc;
use std::time::Duration;

use cortex_base::{SharedClock, TimedStamp, WatchList};

use crate::dirs::ProjectLocator;
use crate::load::{
    Change, FileLayer, LoadFailure, LoadOptions, Loaded, Location, ProjectTrust, load_layers,
    merge_layers,
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
    /// lowering is in force all the same: the files in force are merged
    /// again, as they were read, with the project untrusted, which is what
    /// an untrusted load of them gives. The project's restrictions and
    /// preferences stay; what needed trust is held back. The error is to be
    /// shown, and once it is fixed the next check loads the files again.
    TrustLowered {
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
    /// The file layers `current` was made from, so that they can be merged
    /// again, under other trust, without reading any file.
    layers: Vec<FileLayer>,
    watch: WatchList,
    /// Where the project's files were found when they were last stamped.
    places: Option<ProjectPlaces>,
    clock: SharedClock,
    /// The clock's monotonic time at the last check.
    last_check: Option<Duration>,
    /// The failure of the last reload, so that the same broken file is
    /// reported once and not on every check.
    rejected: Option<LoadFailure>,
}

/// Where the project's files were found, to be found again before each
/// check.
///
/// The check asks the file system about each watched file, and the
/// standard library's way of asking follows links. A link to a network
/// share that a repository puts at `.cortex` after the load would be
/// followed by the next check, and on Windows that sends the account's
/// credentials to the server it names. So before the files are looked at,
/// the project's are found again through the path resolver, which looks at
/// each name on the way without following a link it has not checked. If
/// any of them is found somewhere else, or is refused now, the check
/// reloads through the resolver instead of looking.
#[derive(Debug)]
struct ProjectPlaces {
    locator: ProjectLocator,
    /// Each project file, as named, and where it was.
    places: Vec<(std::path::PathBuf, Location)>,
}

impl ProjectPlaces {
    /// Whether a project file is now somewhere other than it was.
    fn moved(&self) -> bool {
        self.places
            .iter()
            .any(|(file, was)| Location::of(&self.locator, file) != *was)
    }
}

/// What a load is about to read, looked at before it reads it.
struct Before {
    /// The stamp of each file to watch: where each layer is really read
    /// from. A project file whose way is refused is not watched: finding
    /// it again ([`ProjectPlaces`]) notices when that changes.
    stamps: Vec<(std::path::PathBuf, TimedStamp)>,
    places: Option<ProjectPlaces>,
}

impl LiveConfig {
    /// Loads configuration for the first time.
    ///
    /// # Errors
    ///
    /// The [`LoadFailure`] when it cannot be loaded. There is then no
    /// configuration to fall back to, and the caller reports it and stops.
    pub fn start(options: LoadOptions, clock: SharedClock) -> Result<Self, LoadFailure> {
        let watch = WatchList::new(Arc::clone(&clock));
        let before = look_before(&options, &watch);
        let (current, layers) = load_layers(&options)?;
        let mut live = Self {
            options,
            current: Arc::new(current),
            layers,
            watch,
            places: None,
            clock,
            last_check: None,
            rejected: None,
        };
        live.register(before);
        Ok(live)
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
    ///   error.** The files in force are merged again with the project
    ///   untrusted, which keeps its restrictions and preferences and holds
    ///   back what needs trust, and the error is reported:
    ///   [`Reload::TrustLowered`]. Any other part of the same change is
    ///   undone, as below.
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
        let before = look_before(&options, &self.watch);
        match load_layers(&options) {
            Ok((loaded, layers)) => {
                self.options = options;
                self.register(before);
                self.accept(loaded, layers)
            }
            Err(failure) if lowers_trust => self.lower_trust(failure),
            // The options and the files watched stay as they were, so a
            // file that changes is still noticed by the next check.
            Err(failure) => Reload::Rejected(failure),
        }
    }

    /// Looks at the files and reloads if one changed. `None` when none did.
    ///
    /// The project's files are found again first ([`ProjectPlaces`]), and
    /// if any is now somewhere else, the files are reloaded through the
    /// path resolver instead of being looked at.
    pub fn check(&mut self) -> Option<Reload> {
        self.last_check = Some(self.clock.monotonic());
        if self.places.as_ref().is_some_and(ProjectPlaces::moved) {
            return Some(self.reload());
        }
        if self.watch.check().is_empty() {
            None
        } else {
            Some(self.reload())
        }
    }

    /// [`check`](Self::check), unless a check ran less than `interval` ago,
    /// in which case nothing is looked at. For the check on a key press,
    /// which runs at most once a second.
    pub fn check_at_most_every(&mut self, interval: Duration) -> Option<Reload> {
        let now = self.clock.monotonic();
        match self.last_check {
            Some(last) if now.saturating_sub(last) < interval => None,
            _ => self.check(),
        }
    }

    /// Loads again, whether or not a file changed: `/reload`.
    pub fn reload(&mut self) -> Reload {
        // The stamps are taken before the files are read, so that a change
        // that lands during the read is seen by the next check.
        let before = look_before(&self.options, &self.watch);
        let loaded = load_layers(&self.options);
        // The files are stamped afresh whatever the outcome: a file that is
        // broken is reported now, and again only when it changes again.
        self.register(before);
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

    /// Watches what [`look_before`] saw, in place of what was watched.
    fn register(&mut self, before: Before) {
        self.watch.clear();
        for (file, stamp) in before.stamps {
            self.watch.watch_with(file, stamp);
        }
        self.places = before.places;
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
    /// force again, as they were read, under the options in force with
    /// trust lowered. No file is read, so the error in one cannot get in
    /// the way. The result is what an untrusted load of those files gives:
    /// the project's tightenings and preferences stay, and what needs trust
    /// is held back.
    fn lower_trust(&mut self, failure: LoadFailure) -> Reload {
        let mut options = self.options.clone();
        options.trust = ProjectTrust::Untrusted;
        // What checking the files noticed, as the load that read them said.
        let notes = self
            .layers
            .iter()
            .flat_map(|layer| layer.notes.iter().cloned())
            .collect();
        match merge_layers(&options, &self.layers, self.current.files.clone(), notes) {
            Ok(loaded) => {
                self.options = options;
                self.rejected = Some(failure.clone());
                let changes = loaded.changes_since(&self.current);
                let previous = std::mem::replace(&mut self.current, Arc::new(loaded));
                Reload::TrustLowered {
                    previous,
                    changes,
                    failure,
                }
            }
            // Not reached: these layers merged under these options with
            // trust, and trust only holds values back. Were it reached,
            // nothing changes and the error is reported, as for any change
            // that does not load.
            Err(_) => Reload::Rejected(failure),
        }
    }
}

/// Finds the files a load reads and stamps them, before the load reads
/// them.
fn look_before(options: &LoadOptions, watch: &WatchList) -> Before {
    let locator = options.project_locator();
    let mut stamps = Vec::new();
    let mut places = Vec::new();
    for (kind, file, location) in options.locations(locator.as_ref()) {
        if let Location::At(path) = &location {
            let stamp = watch.stamp(path);
            stamps.push((path.clone(), stamp));
        }
        if kind.is_project() {
            places.push((file, location));
        }
    }
    Before {
        stamps,
        places: locator.map(|locator| ProjectPlaces { locator, places }),
    }
}
