//! What the operating system offers the sandbox, and what the harness makes
//! of it (`docs/decisions/0032-what-the-linux-sandbox-needs.md`).
//!
//! [`detect`] asks once, at start-up: on Linux it is one system call that
//! returns the kernel's Landlock version. [`Support::assess`] turns the
//! answer into what a session reports: whether the sandbox enforces, and
//! what it leaves open.
//!
//! | Landlock | Linux | The sandbox |
//! |---|---|---|
//! | none, or turned off | | Does not enforce. Commands run unconfined. |
//! | 1, 2 | 5.13, 5.19 | Does not enforce: a command could empty any file the user can write (truncation is controlled from version 3), and on version 1 every rename across directories fails. Commands run unconfined, as with none. |
//! | 3 to 5 | 6.2 to 6.11 | Enforces. A command can still send a signal to a process outside the sandbox (controlled from version 6), and the status says so. |
//! | 6 and newer | 6.12 | Enforces. |

/// The oldest Landlock version the sandbox enforces with.
pub const MIN_LANDLOCK: u8 = 3;
/// The first Landlock version that keeps a command's signals inside the
/// sandbox.
pub const SCOPED_LANDLOCK: u8 = 6;

/// What this build and this system offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Support {
    /// The kernel has Landlock, of this version.
    Landlock(u8),
    /// The kernel has no Landlock, or it is turned off at boot.
    NoLandlock,
    /// This build has no sandbox: the `backends` feature is off.
    NotBuilt,
    /// There is no sandbox for this operating system yet.
    UnsupportedSystem,
}

/// What a session makes of [`Support`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Assessment {
    /// Commands are confined.
    Enforcing {
        /// The Landlock version the launcher applies.
        landlock: u8,
        /// What a confined command can still do that a newer kernel would
        /// stop, one sentence each. Empty on a kernel with everything.
        gaps: Vec<&'static str>,
    },
    /// Commands are not confined.
    NotEnforcing(Unconfined),
}

/// Why commands are not confined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unconfined {
    /// This build has no sandbox.
    NotBuilt,
    /// There is no sandbox for this operating system yet.
    UnsupportedSystem,
    /// The kernel has no Landlock, or it is turned off.
    NoLandlock,
    /// The kernel's Landlock is older than the sandbox needs: this
    /// version.
    LandlockTooOld(u8),
}

impl Support {
    /// Whether the sandbox enforces with this support, and what it leaves
    /// open.
    #[must_use]
    pub fn assess(self) -> Assessment {
        match self {
            Self::Landlock(version) if version >= MIN_LANDLOCK => {
                let mut gaps = Vec::new();
                if version < SCOPED_LANDLOCK {
                    gaps.push(
                        "A command can send a signal to a process outside the sandbox, the \
                         harness's included: stopping that needs Landlock 6 (Linux 6.12).",
                    );
                }
                Assessment::Enforcing {
                    landlock: version.min(crate::policy::NEWEST_LANDLOCK),
                    gaps,
                }
            }
            Self::Landlock(version) => Assessment::NotEnforcing(Unconfined::LandlockTooOld(version)),
            Self::NoLandlock => Assessment::NotEnforcing(Unconfined::NoLandlock),
            Self::NotBuilt => Assessment::NotEnforcing(Unconfined::NotBuilt),
            Self::UnsupportedSystem => Assessment::NotEnforcing(Unconfined::UnsupportedSystem),
        }
    }
}

impl Unconfined {
    /// The reason in a sentence, for a status line's detail.
    #[must_use]
    pub fn explain(self) -> String {
        match self {
            Self::NotBuilt => "This build of Cortex-Harness has no sandbox, so a command that runs \
                               is not confined."
                .to_owned(),
            Self::UnsupportedSystem => "Cortex-Harness has no sandbox for this operating system \
                                        yet, so a command that runs is not confined."
                .to_owned(),
            Self::NoLandlock => "This kernel has no Landlock, or it is turned off, so a command \
                                 that runs is not confined."
                .to_owned(),
            Self::LandlockTooOld(version) => format!(
                "This kernel's Landlock is version {version}, and the sandbox needs version 3 \
                 (Linux 6.2): without it a command could empty any file you can write, outside \
                 the workspace too{}. So a command that runs is not confined.",
                if version < 2 {
                    ", and every rename from one directory to another would fail"
                } else {
                    ""
                }
            ),
        }
    }
}

/// Asks the system what the sandbox can use. Cheap: one system call on
/// Linux, none elsewhere.
#[must_use]
pub fn detect() -> Support {
    if cfg!(not(target_os = "linux")) {
        return Support::UnsupportedSystem;
    }
    if cfg!(not(feature = "backends")) {
        return Support::NotBuilt;
    }
    #[cfg(target_os = "linux")]
    {
        match landlock_version() {
            Some(version) => Support::Landlock(version),
            None => Support::NoLandlock,
        }
    }
    #[cfg(not(target_os = "linux"))]
    Support::UnsupportedSystem
}

/// The kernel's Landlock version, or `None` when it has none or it is off.
#[cfg(target_os = "linux")]
#[allow(
    unsafe_code,
    reason = "decision 0030: the kernel's Landlock version is only reachable through a system \
              call; cortex-sandbox is where such calls live"
)]
fn landlock_version() -> Option<u8> {
    /// `LANDLOCK_CREATE_RULESET_VERSION`: ask for the version, make nothing.
    const VERSION: libc::c_uint = 1;
    // SAFETY: with a null attribute pointer, a size of zero and the
    // version flag, the call reads no memory and creates nothing: it
    // returns the version, or -1 with `ENOSYS` or `EOPNOTSUPP`.
    let answer = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            VERSION,
        )
    };
    u8::try_from(answer).ok().filter(|version| *version > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_3_and_newer_enforce_and_6_has_no_gap() {
        for version in 3..=5 {
            let Assessment::Enforcing { landlock, gaps } = Support::Landlock(version).assess()
            else {
                panic!("version {version} does not enforce");
            };
            assert_eq!(landlock, version);
            assert_eq!(gaps.len(), 1, "{gaps:?}");
            assert!(gaps[0].contains("signal"), "{gaps:?}");
        }
        for version in 6..=9 {
            assert_eq!(
                Support::Landlock(version).assess(),
                Assessment::Enforcing {
                    landlock: version,
                    gaps: Vec::new()
                }
            );
        }
        // A kernel newer than this build is applied at what the build
        // knows.
        assert_eq!(
            Support::Landlock(12).assess(),
            Assessment::Enforcing {
                landlock: crate::policy::NEWEST_LANDLOCK,
                gaps: Vec::new()
            }
        );
    }

    #[test]
    fn versions_1_and_2_and_no_landlock_do_not_enforce() {
        assert_eq!(
            Support::Landlock(1).assess(),
            Assessment::NotEnforcing(Unconfined::LandlockTooOld(1))
        );
        assert_eq!(
            Support::Landlock(2).assess(),
            Assessment::NotEnforcing(Unconfined::LandlockTooOld(2))
        );
        assert_eq!(
            Support::NoLandlock.assess(),
            Assessment::NotEnforcing(Unconfined::NoLandlock)
        );
        assert_eq!(
            Support::NotBuilt.assess(),
            Assessment::NotEnforcing(Unconfined::NotBuilt)
        );
        assert_eq!(
            Support::UnsupportedSystem.assess(),
            Assessment::NotEnforcing(Unconfined::UnsupportedSystem)
        );
    }

    #[test]
    fn an_old_landlock_names_what_it_lacks() {
        let two = Unconfined::LandlockTooOld(2).explain();
        assert!(two.contains("version 2") && two.contains("empty any file"), "{two}");
        assert!(!two.contains("rename"), "{two}");
        let one = Unconfined::LandlockTooOld(1).explain();
        assert!(one.contains("rename"), "{one}");
    }

    #[test]
    fn detection_matches_what_this_build_and_system_have() {
        let found = detect();
        if cfg!(not(target_os = "linux")) {
            assert_eq!(found, Support::UnsupportedSystem);
        } else if cfg!(not(feature = "backends")) {
            assert_eq!(found, Support::NotBuilt);
        } else {
            assert!(
                matches!(found, Support::Landlock(_) | Support::NoLandlock),
                "{found:?}"
            );
        }
    }
}
