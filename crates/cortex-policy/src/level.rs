//! The ladder of levels
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Levels").
//!
//! A level says who decides when no rule has settled a question: the
//! **holder** of the lease, which is the agent, or its **supervisor**, which
//! for an ordinary session is the user. The table on that page is
//! [`decider`], one row per level and one column per [`Matter`].
//!
//! | Level | Read | Read beyond | Edit | Write beyond | Read-only command | Safe command | Other command | Network |
//! |---|---|---|---|---|---|---|---|---|
//! | `read-only` | holder | supervisor | refused | refused | holder | supervisor | supervisor | supervisor |
//! | `supervised` | holder | supervisor | holder | supervisor | holder | holder | supervisor | supervisor |
//! | `guided` | holder | supervisor | holder | supervisor | holder | holder | supervisor | supervisor |
//! | `autonomous` | holder | supervisor | holder | supervisor | holder | holder | holder | supervisor |
//! | `unrestricted` | holder | holder | holder | holder | holder | holder | holder | holder |
//!
//! - **`guided` is defined and not yet offered.** The kinds of command a
//!   guided lease names are allow rules of that lease, and rules are settled
//!   before the level is asked, so its row reads like `supervised`. Task
//!   leases, which are what make it differ, arrive in step 37, and until
//!   then a session cannot be started at it ([`check_standing`]).
//! - **`guided` and `autonomous` need an enforcing sandbox.** They let
//!   commands run unasked, and that is offered only where the commands are
//!   confined. Where they are not, the level is refused with the reason.
//! - **A level the build does not know decides nothing.** Every matter is
//!   refused.

use cortex_protocol::error::{ErrorData, ErrorKind, code};
use cortex_protocol::lease::Level;

/// Who decides a matter that no rule has settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Decider {
    /// The agent: it happens without asking.
    Holder,
    /// Whoever supervises the session: they are asked.
    Supervisor,
    /// Nobody: it does not happen at this level, and a rule cannot make it.
    Refused,
}

/// A kind of thing to decide: one column of the level table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Matter {
    /// A read inside the lease's scope.
    Read,
    /// A read beyond it: outside the workspace, or of a file that looks
    /// like a secret.
    ReadBeyond,
    /// An edit inside the lease's scope.
    Edit,
    /// A write beyond it: outside the workspace, of a protected path, or of
    /// a file that looks like a secret.
    WriteBeyond,
    /// A command on the built-in table of commands that only read.
    ReadOnlyCommand,
    /// A command on a safe list that may write: one a rule allows, one the
    /// user approved, one learned for the project.
    SafeCommand,
    /// Any other command.
    OtherCommand,
    /// Network access from a tool.
    Network,
    /// A command run outside the sandbox, where the sandbox enforces
    /// (`docs/architecture/06-permissions-and-sandbox.md`, "Leaving the
    /// sandbox").
    LeaveSandbox,
}

impl Matter {
    /// Every matter, in the order of the table's columns.
    pub const ALL: [Self; 9] = [
        Self::Read,
        Self::ReadBeyond,
        Self::Edit,
        Self::WriteBeyond,
        Self::ReadOnlyCommand,
        Self::SafeCommand,
        Self::OtherCommand,
        Self::Network,
        Self::LeaveSandbox,
    ];
}

/// Who decides `matter` at `level`, when no rule has settled it.
#[must_use]
pub fn decider(level: &Level, matter: Matter) -> Decider {
    use Decider::{Holder, Refused, Supervisor};
    match level {
        Level::ReadOnly => match matter {
            Matter::Read | Matter::ReadOnlyCommand => Holder,
            Matter::Edit | Matter::WriteBeyond => Refused,
            Matter::ReadBeyond
            | Matter::SafeCommand
            | Matter::OtherCommand
            | Matter::Network
            | Matter::LeaveSandbox => Supervisor,
        },
        Level::Supervised | Level::Guided => match matter {
            Matter::Read | Matter::Edit | Matter::ReadOnlyCommand | Matter::SafeCommand => Holder,
            Matter::ReadBeyond
            | Matter::WriteBeyond
            | Matter::OtherCommand
            | Matter::Network
            | Matter::LeaveSandbox => Supervisor,
        },
        Level::Autonomous => match matter {
            Matter::Read
            | Matter::Edit
            | Matter::ReadOnlyCommand
            | Matter::SafeCommand
            | Matter::OtherCommand => Holder,
            Matter::ReadBeyond | Matter::WriteBeyond | Matter::Network | Matter::LeaveSandbox => {
                Supervisor
            }
        },
        Level::Unrestricted => Holder,
        // A level this build has never heard of: it cannot know what the
        // level was meant to hold back, so it holds everything back.
        Level::Unknown(_) => Refused,
    }
}

/// A level's place on the ladder, lowest first. `None` for a level this
/// build does not know, which has no place.
#[must_use]
pub fn rank(level: &Level) -> Option<u8> {
    match level {
        Level::ReadOnly => Some(0),
        Level::Supervised => Some(1),
        Level::Guided => Some(2),
        Level::Autonomous => Some(3),
        Level::Unrestricted => Some(4),
        Level::Unknown(_) => None,
    }
}

/// The lower of two levels. A level this build does not know is lower than
/// every other: it grants nothing.
#[must_use]
pub fn lower(first: &Level, second: &Level) -> Level {
    match (rank(first), rank(second)) {
        (None, _) => first.clone(),
        (_, None) => second.clone(),
        (Some(a), Some(b)) => {
            if a <= b {
                first.clone()
            } else {
                second.clone()
            }
        }
    }
}

/// Whether `level` is no higher than `other`. A level this build does not
/// know is no higher than anything, and nothing known is no higher than it.
#[must_use]
pub fn no_higher_than(level: &Level, other: &Level) -> bool {
    match (rank(level), rank(other)) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(a), Some(b)) => a <= b,
    }
}

/// Whether a level lets commands run unasked, and so needs them confined.
#[must_use]
pub fn needs_sandbox(level: &Level) -> bool {
    matches!(level, Level::Guided | Level::Autonomous)
}

/// Whether the OS sandbox confines the commands a session starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxState {
    /// It does.
    Enforcing,
    /// It does not, and why.
    NotEnforcing(NoSandbox),
}

/// Why commands are not confined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoSandbox {
    /// This build has no sandbox: it is built in a later step.
    NotBuilt,
    /// There is no sandbox for this operating system yet.
    UnsupportedSystem,
    /// The kernel has no Landlock, or it is turned off.
    NoLandlock,
    /// The kernel's Landlock is older than the sandbox needs: this version
    /// (`docs/decisions/0032-what-the-linux-sandbox-needs.md`).
    LandlockTooOld(u8),
    /// A directory a command may write holds the harness's own
    /// configuration, state or data, which the sandbox cannot keep a
    /// command from changing.
    HarnessFilesWritable,
    /// The user turned the sandbox off (`sandbox.enabled = false`).
    TurnedOff,
}

impl NoSandbox {
    /// The reason in a sentence, for the status line's detail and for the
    /// refusal of a level.
    #[must_use]
    pub fn explain(self) -> String {
        match self {
            Self::NotBuilt => {
                "This build of Cortex-Harness has no sandbox, so a command that runs \
                               is not confined."
                    .to_owned()
            }
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
            Self::HarnessFilesWritable => "The workspace, or a directory `sandbox.write` \
                                           names, holds Cortex-Harness's own configuration, \
                                           state or data, which the sandbox cannot keep a \
                                           command from changing: a command could change what \
                                           it is allowed. So a command that runs is not \
                                           confined. Open the project's own directory instead."
                .to_owned(),
            Self::TurnedOff => "The sandbox is turned off in configuration (`sandbox.enabled = \
                                false`), so a command that runs is not confined."
                .to_owned(),
        }
    }
}

/// Why a session cannot be started at a level.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LevelError {
    /// The level lets commands run unasked, and nothing confines them.
    #[error(
        "The permission mode `{level}` is not available here. It lets commands run without asking, \
         which is only offered where the sandbox confines them. {why} Use `supervised`, which asks \
         before each command that is not on a safe list, or `unrestricted` if you accept that \
         nothing is confined or asked.",
        level = .level.as_str(),
        why = .why.explain()
    )]
    SandboxRequired {
        /// The level that was asked for.
        level: Level,
        /// Why commands are not confined.
        why: NoSandbox,
    },
    /// The level exists and cannot be chosen yet.
    #[error(
        "The permission mode `guided` is not available yet. It belongs to task leases, which name \
         the kinds of command that may run without asking. Use `supervised` with allow rules for \
         those commands."
    )]
    GuidedNotAvailable,
    /// This build does not know the level.
    #[error(
        "The permission mode `{level}` is not one this version knows. The modes are `read-only`, \
         `supervised`, `autonomous` and `unrestricted`."
    )]
    Unknown {
        /// The level, as written.
        level: String,
    },
}

impl LevelError {
    /// The error as a client is sent it.
    #[must_use]
    pub fn to_error_data(&self) -> ErrorData {
        let code = match self {
            Self::SandboxRequired { .. } | Self::GuidedNotAvailable => code::SANDBOX_REQUIRED,
            Self::Unknown { .. } => code::UNKNOWN_VALUE,
        };
        ErrorData::new(ErrorKind::Permission, code, self.to_string())
    }
}

/// Whether a session's standing lease may have `level`, with the sandbox in
/// the state given.
///
/// - `read-only`, `supervised` and `unrestricted` are always available.
/// - `autonomous` needs an enforcing sandbox.
/// - `guided` is not offered until task leases exist (step 37).
/// - A level this build does not know is refused.
///
/// # Errors
///
/// With the reason, written for the user.
pub fn check_standing(level: &Level, sandbox: SandboxState) -> Result<(), LevelError> {
    match level {
        Level::ReadOnly | Level::Supervised | Level::Unrestricted => Ok(()),
        Level::Guided => Err(LevelError::GuidedNotAvailable),
        Level::Autonomous => match sandbox {
            SandboxState::Enforcing => Ok(()),
            SandboxState::NotEnforcing(why) => Err(LevelError::SandboxRequired {
                level: level.clone(),
                why,
            }),
        },
        Level::Unknown(name) => Err(LevelError::Unknown {
            level: name.clone(),
        }),
    }
}

/// One line for the status line and for the start of a session, when the
/// sandbox does not confine commands. `None` when it does.
///
/// The level matters to what the line says: at `unrestricted` nothing is
/// asked either, and the user should see both facts together.
#[must_use]
pub fn sandbox_notice(level: &Level, sandbox: SandboxState) -> Option<String> {
    let SandboxState::NotEnforcing(why) = sandbox else {
        return None;
    };
    let consequence = match level {
        Level::Unrestricted => {
            "At `unrestricted` nothing is asked either: every command runs with your full rights."
        }
        Level::ReadOnly => {
            "A command that only reads runs without asking; every other command asks first, and \
             one you approve runs with your full rights."
        }
        _ => {
            "Commands on a safe list run without asking; every other command asks first, and one \
             you approve runs with your full rights. `autonomous` is not available."
        }
    };
    Some(format!("sandbox: off. {} {consequence}", why.explain()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEVELS: [Level; 5] = [
        Level::ReadOnly,
        Level::Supervised,
        Level::Guided,
        Level::Autonomous,
        Level::Unrestricted,
    ];

    /// The level table of the architecture page, row by row.
    #[test]
    fn the_table_is_the_one_on_the_page() {
        use Decider::{Holder as H, Refused as R, Supervisor as S};
        let rows: [(Level, [Decider; 9]); 5] = [
            (Level::ReadOnly, [H, S, R, R, H, S, S, S, S]),
            (Level::Supervised, [H, S, H, S, H, H, S, S, S]),
            (Level::Guided, [H, S, H, S, H, H, S, S, S]),
            (Level::Autonomous, [H, S, H, S, H, H, H, S, S]),
            (Level::Unrestricted, [H, H, H, H, H, H, H, H, H]),
        ];
        assert_eq!(
            Matter::ALL.len(),
            rows[0].1.len(),
            "a column of the table is not checked"
        );
        for (level, expected) in rows {
            for (matter, expected) in Matter::ALL.into_iter().zip(expected) {
                assert_eq!(decider(&level, matter), expected, "{level:?} {matter:?}");
            }
        }
    }

    #[test]
    fn an_unknown_level_decides_nothing() {
        let unknown = Level::Unknown("omnipotent".to_owned());
        for matter in Matter::ALL {
            assert_eq!(decider(&unknown, matter), Decider::Refused);
        }
        assert_eq!(rank(&unknown), None);
        assert!(no_higher_than(&unknown, &Level::ReadOnly));
        assert!(!no_higher_than(&Level::ReadOnly, &unknown));
        assert_eq!(lower(&unknown, &Level::Unrestricted), unknown);
        assert_eq!(lower(&Level::Unrestricted, &unknown), unknown);
    }

    /// A higher level never asks or refuses where a lower one lets the
    /// holder decide: the ladder only ever opens.
    #[test]
    fn a_higher_level_never_holds_back_what_a_lower_one_lets_through() {
        let openness = |decider| match decider {
            Decider::Refused => 0,
            Decider::Supervisor => 1,
            Decider::Holder => 2,
        };
        for pair in LEVELS.windows(2) {
            let [lower_level, higher] = pair else {
                continue;
            };
            for matter in Matter::ALL {
                assert!(
                    openness(decider(higher, matter)) >= openness(decider(lower_level, matter)),
                    "{higher:?} is more closed than {lower_level:?} for {matter:?}"
                );
            }
            assert!(rank(lower_level) < rank(higher));
            assert_eq!(&lower(lower_level, higher), lower_level);
            assert_eq!(&lower(higher, lower_level), lower_level);
            assert!(no_higher_than(lower_level, higher));
            assert!(!no_higher_than(higher, lower_level));
        }
    }

    #[test]
    fn an_old_landlock_names_what_it_lacks() {
        let two = NoSandbox::LandlockTooOld(2).explain();
        assert!(
            two.contains("version 2") && two.contains("empty any file"),
            "{two}"
        );
        assert!(!two.contains("rename"), "{two}");
        let one = NoSandbox::LandlockTooOld(1).explain();
        assert!(one.contains("rename"), "{one}");
    }

    #[test]
    fn autonomous_is_refused_where_the_sandbox_does_not_enforce() {
        for why in [
            NoSandbox::NotBuilt,
            NoSandbox::UnsupportedSystem,
            NoSandbox::NoLandlock,
            NoSandbox::LandlockTooOld(2),
            NoSandbox::HarnessFilesWritable,
            NoSandbox::TurnedOff,
        ] {
            let error =
                check_standing(&Level::Autonomous, SandboxState::NotEnforcing(why)).unwrap_err();
            assert_eq!(
                error,
                LevelError::SandboxRequired {
                    level: Level::Autonomous,
                    why
                }
            );
            let data = error.to_error_data();
            assert_eq!(data.code, code::SANDBOX_REQUIRED);
            assert_eq!(data.kind, ErrorKind::Permission);
            // The refusal names the level, says why, and says what to use.
            assert!(data.message.contains("`autonomous`"), "{}", data.message);
            assert!(data.message.contains(&why.explain()), "{}", data.message);
            assert!(data.message.contains("`supervised`"), "{}", data.message);
            // The levels that do not depend on the sandbox still start.
            for level in [Level::ReadOnly, Level::Supervised, Level::Unrestricted] {
                assert_eq!(
                    check_standing(&level, SandboxState::NotEnforcing(why)),
                    Ok(())
                );
            }
        }
        assert_eq!(
            check_standing(&Level::Autonomous, SandboxState::Enforcing),
            Ok(())
        );
    }

    #[test]
    fn guided_and_unknown_levels_cannot_be_started() {
        for sandbox in [
            SandboxState::Enforcing,
            SandboxState::NotEnforcing(NoSandbox::NotBuilt),
        ] {
            assert_eq!(
                check_standing(&Level::Guided, sandbox),
                Err(LevelError::GuidedNotAvailable)
            );
            let error = check_standing(&Level::Unknown("root".to_owned()), sandbox).unwrap_err();
            assert_eq!(error.to_error_data().code, code::UNKNOWN_VALUE);
            assert!(error.to_string().contains("`root`"));
        }
        assert!(needs_sandbox(&Level::Guided) && needs_sandbox(&Level::Autonomous));
        assert!(!needs_sandbox(&Level::Supervised) && !needs_sandbox(&Level::Unrestricted));
    }

    #[test]
    fn what_the_user_is_told_about_a_missing_sandbox_is_plain() {
        assert_eq!(
            sandbox_notice(&Level::Supervised, SandboxState::Enforcing),
            None
        );
        let off = SandboxState::NotEnforcing(NoSandbox::NotBuilt);
        let supervised = sandbox_notice(&Level::Supervised, off).unwrap();
        assert!(supervised.starts_with("sandbox: off."), "{supervised}");
        assert!(supervised.contains("has no sandbox"), "{supervised}");
        assert!(
            supervised.contains("`autonomous` is not available"),
            "{supervised}"
        );
        assert!(supervised.contains("your full rights"), "{supervised}");
        let unrestricted = sandbox_notice(&Level::Unrestricted, off).unwrap();
        assert!(
            unrestricted.contains("nothing is asked either"),
            "{unrestricted}"
        );
        let read_only = sandbox_notice(&Level::ReadOnly, off).unwrap();
        assert!(read_only.contains("only reads"), "{read_only}");
    }
}
