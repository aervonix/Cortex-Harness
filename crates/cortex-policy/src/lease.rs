//! Leases: narrowing, the subset test, and the standing lease
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Leases").
//!
//! The lease type itself is the protocol's
//! ([`cortex_protocol::lease::Lease`]), read through its strict decoder,
//! which refuses a field or a value it does not know
//! (`docs/decisions/0028-enforced-values-fail-closed.md`). This module adds
//! what a lease *means* between a parent and a child, as an extension trait
//! on that type ([`LeaseExt`]), and adds no other way to read one.
//!
//! # Narrowing
//!
//! A session that starts another proposes a lease, and the harness gives
//! the child `parent.narrow(proposed)`: the widest lease inside both.
//! Field by field:
//!
//! | Field | The child gets |
//! |---|---|
//! | Level | The lower of the two, and never `unrestricted`, which is not passed down. |
//! | Rules | Every `deny` and `ask` rule of both. An `allow` rule of the proposal only if the parent has the same rule. |
//! | Writable and readable roots | The intersection: a root of one that lies inside a root of the other. |
//! | Protected paths | Those of both. |
//! | Network | On only if both have it on. |
//! | Models | Those both name. A lease that names none means "the configured ones", and takes the other's list. |
//! | Budget, team size | The smaller of each, where "none" is no limit. |
//! | Merging to the main branch | Only if both allow it. |
//! | Stop conditions | Those of both. |
//! | Expiry | No later than either: see below. |
//!
//! **An allow rule is "the same" when its effect and its pattern are the
//! same text.** A child that asks for `cargo test *` under a parent that
//! allows `cargo *` does not get the rule: telling that one pattern lies
//! inside another is where a mistake would widen a lease, so it is not
//! attempted. The child loses nothing it could do under the parent's rule
//! only if the parent's rule is proposed; a proposal should name the
//! parent's rules it wants.
//!
//! **Expiry.** A lease ends at an event (`end_of_task`, `end_of_session`),
//! at a moment (`at`), or never. Two leases can end at an event and at a
//! moment that cannot be ordered ahead of time. The child's expiry is then
//! the event, and the moment becomes a `time` stop condition, which the
//! harness enforces the same way. So the child never outlasts either.
//!
//! # What does not narrow
//!
//! A proposal that names models the parent may not use, and none it may,
//! has no lease inside both: an empty list would mean "any configured
//! model", which is wider. [`LeaseExt::narrow`] refuses it
//! ([`NarrowError::NoModelInCommon`]).

use cortex_base::{AbsPath, PathStyle, UtcTime};
use cortex_protocol::ids::{LeaseId, ModelId, Timestamp};
use cortex_protocol::lease::{
    Budget, Expiry, ExpiryKind, Grant, Grantor, GrantorKind, HardLimits, Lease, Level,
    NetworkAccess, PathSet, Rule, RuleAction, SandboxPolicy, StopCondition, TimeStop,
};

use crate::level::{self, LevelError, SandboxState};

/// Why a proposed lease cannot be narrowed under a parent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NarrowError {
    /// The proposal names models, the parent names models, and they have
    /// none in common.
    #[error(
        "the lease asks for the models {requested:?} and its parent may only use {parent:?}. Ask \
         for one of the parent's models, or for none, which means any the parent may use"
    )]
    NoModelInCommon {
        /// The models the parent's lease names.
        parent: Vec<String>,
        /// The models the proposal names.
        requested: Vec<String>,
    },
}

/// What a lease means between a parent and a child.
pub trait LeaseExt {
    /// The widest lease inside both this one and `requested`, for a child of
    /// the session that holds this one. Never wider than `self`, in any
    /// field. Its id is `requested`'s, and it is granted by this lease.
    ///
    /// The parent's budget is taken as written. To cap a child by what the
    /// parent has **left**, narrow a copy of the parent's lease whose
    /// budget is what remains ([`with_budget`](Self::with_budget)).
    ///
    /// # Errors
    ///
    /// When no lease is inside both ([`NarrowError`]).
    fn narrow(&self, requested: &Lease) -> Result<Lease, NarrowError>;

    /// Whether everything this lease permits, `parent` permits too.
    fn is_within(&self, parent: &Lease) -> bool;

    /// This lease with another budget: what is left of it.
    #[must_use]
    fn with_budget(&self, left: Budget) -> Lease;
}

impl LeaseExt for Lease {
    fn narrow(&self, requested: &Lease) -> Result<Lease, NarrowError> {
        narrow_in(self, requested, PathStyle::NATIVE)
    }

    fn is_within(&self, parent: &Lease) -> bool {
        is_within_in(self, parent, PathStyle::NATIVE)
    }

    fn with_budget(&self, left: Budget) -> Lease {
        let mut lease = self.clone();
        lease.grant.budget = left;
        lease
    }
}

/// [`LeaseExt::narrow`], reading the path sets in a given style, so that
/// the Windows rules are tested on every OS.
pub(crate) fn narrow_in(
    parent: &Lease,
    requested: &Lease,
    style: PathStyle,
) -> Result<Lease, NarrowError> {
    let (ours, theirs) = (&parent.grant, &requested.grant);

    // `unrestricted` is chosen by a person for one session, and is never
    // handed on.
    let level = level::lower(&level::lower(&ours.level, &theirs.level), &Level::Autonomous);

    let mut rules: Vec<Rule> = Vec::new();
    let restrictive = |rule: &&Rule| rule.action != RuleAction::Allow;
    for rule in ours
        .rules
        .iter()
        .filter(restrictive)
        .chain(theirs.rules.iter().filter(restrictive))
        .chain(
            theirs
                .rules
                .iter()
                .filter(|rule| rule.action == RuleAction::Allow && ours.rules.contains(rule)),
        )
    {
        push_new(&mut rules, rule.clone());
    }

    let sandbox = SandboxPolicy {
        read: intersect(&ours.sandbox.read, &theirs.sandbox.read, style),
        write: intersect(&ours.sandbox.write, &theirs.sandbox.write, style),
        protect: union(&ours.sandbox.protect, &theirs.sandbox.protect),
        network: if ours.sandbox.network == NetworkAccess::Full
            && theirs.sandbox.network == NetworkAccess::Full
        {
            NetworkAccess::Full
        } else {
            NetworkAccess::Off
        },
    };

    let models = match (ours.models.is_empty(), theirs.models.is_empty()) {
        (true, _) => theirs.models.clone(),
        (false, true) => ours.models.clone(),
        (false, false) => {
            let shared: Vec<ModelId> = theirs
                .models
                .iter()
                .filter(|model| ours.models.contains(model))
                .cloned()
                .collect();
            if shared.is_empty() {
                let names = |models: &[ModelId]| {
                    models.iter().map(|model| model.as_str().to_owned()).collect()
                };
                return Err(NarrowError::NoModelInCommon {
                    parent: names(&ours.models),
                    requested: names(&theirs.models),
                });
            }
            shared
        }
    };

    let grant = Grant {
        level,
        rules,
        sandbox,
        models,
        budget: Budget {
            usd_nanos: smaller(ours.budget.usd_nanos, theirs.budget.usd_nanos),
            tokens: smaller(ours.budget.tokens, theirs.budget.tokens),
            requests: smaller(ours.budget.requests, theirs.budget.requests),
            wall_clock_ms: smaller(ours.budget.wall_clock_ms, theirs.budget.wall_clock_ms),
        },
        limits: HardLimits {
            merge_to_main: ours.limits.merge_to_main && theirs.limits.merge_to_main,
            team_size: smaller(ours.limits.team_size, theirs.limits.team_size),
        },
    };

    let mut stop_when: Vec<StopCondition> = Vec::new();
    for condition in parent.stop_when.iter().chain(&requested.stop_when) {
        push_new(&mut stop_when, condition.clone());
    }
    let expires = narrow_expiry(&parent.expires, &requested.expires, &mut stop_when);

    Ok(Lease {
        id: requested.id.clone(),
        grant,
        stop_when,
        expires,
        granted_by: Grantor {
            kind: GrantorKind::Parent,
            lease_id: Some(parent.id.clone()),
        },
    })
}

/// [`LeaseExt::is_within`], in a given style.
pub(crate) fn is_within_in(child: &Lease, parent: &Lease, style: PathStyle) -> bool {
    let (ours, theirs) = (&child.grant, &parent.grant);
    let rules_hold = theirs
        .rules
        .iter()
        .filter(|rule| rule.action != RuleAction::Allow)
        .all(|rule| ours.rules.contains(rule))
        && ours
            .rules
            .iter()
            .filter(|rule| rule.action == RuleAction::Allow)
            .all(|rule| theirs.rules.contains(rule));
    let models_hold = theirs.models.is_empty()
        || (!ours.models.is_empty()
            && ours.models.iter().all(|model| theirs.models.contains(model)));
    let network_holds =
        ours.sandbox.network == NetworkAccess::Off || theirs.sandbox.network == NetworkAccess::Full;
    let stops_hold = parent
        .stop_when
        .iter()
        .all(|condition| child.stop_when.contains(condition));

    level::no_higher_than(&ours.level, &theirs.level)
        && rules_hold
        && subset(&ours.sandbox.read, &theirs.sandbox.read, style)
        && subset(&ours.sandbox.write, &theirs.sandbox.write, style)
        && protects_all_of(&ours.sandbox.protect, &theirs.sandbox.protect, style)
        && network_holds
        && models_hold
        && no_more(ours.budget.usd_nanos, theirs.budget.usd_nanos)
        && no_more(ours.budget.tokens, theirs.budget.tokens)
        && no_more(ours.budget.requests, theirs.budget.requests)
        && no_more(ours.budget.wall_clock_ms, theirs.budget.wall_clock_ms)
        && (!ours.limits.merge_to_main || theirs.limits.merge_to_main)
        && no_more(ours.limits.team_size, theirs.limits.team_size)
        && stops_hold
        && ends_no_later(child, parent)
}

fn push_new<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
}

/// The smaller of two limits, where `None` is no limit.
fn smaller<T: Ord + Copy>(first: Option<T>, second: Option<T>) -> Option<T> {
    match (first, second) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (limit, None) | (None, limit) => limit,
    }
}

/// Whether a limit is no looser than another, where `None` is no limit.
fn no_more<T: Ord + Copy>(limit: Option<T>, than: Option<T>) -> bool {
    match (limit, than) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(a), Some(b)) => a <= b,
    }
}

/// A root of a path set, read as a path. A root that is not an absolute
/// path in this style names nothing that could be enforced.
fn root_of(text: &str, style: PathStyle) -> Option<AbsPath> {
    AbsPath::parse(text, style).ok()
}

/// Whether `path` is in the set. Names are compared exactly: a set grants,
/// and a grant must not reach a folder that differs only in case.
fn covers(set: &PathSet, path: &AbsPath, style: PathStyle) -> bool {
    set.all
        || set
            .roots
            .iter()
            .filter_map(|root| root_of(root, style))
            .any(|root| path.starts_with_exactly(&root))
}

/// The paths in both sets: every root of one that lies inside the other. A
/// root that cannot be read as a path is left out.
fn intersect(first: &PathSet, second: &PathSet, style: PathStyle) -> PathSet {
    if first.all && second.all {
        return PathSet {
            all: true,
            roots: Vec::new(),
        };
    }
    let mut roots = Vec::new();
    for (from, other) in [(first, second), (second, first)] {
        if from.all {
            continue;
        }
        for text in &from.roots {
            if root_of(text, style).is_some_and(|root| covers(other, &root, style)) {
                push_new(&mut roots, text.clone());
            }
        }
    }
    PathSet { all: false, roots }
}

/// Whether every path in `inner` is in `outer`. A root of `inner` that
/// cannot be read as a path is not known to be inside anything.
fn subset(inner: &PathSet, outer: &PathSet, style: PathStyle) -> bool {
    if outer.all {
        return true;
    }
    !inner.all
        && inner
            .roots
            .iter()
            .all(|text| root_of(text, style).is_some_and(|root| covers(outer, &root, style)))
}

/// The paths in either set.
fn union(first: &PathSet, second: &PathSet) -> PathSet {
    if first.all || second.all {
        return PathSet {
            all: true,
            roots: Vec::new(),
        };
    }
    let mut roots = first.roots.clone();
    for root in &second.roots {
        push_new(&mut roots, root.clone());
    }
    PathSet { all: false, roots }
}

/// Whether `ours` protects everything `theirs` protects. A root is
/// protected when it is listed as it is written, or lies inside a root
/// that is.
fn protects_all_of(ours: &PathSet, theirs: &PathSet, style: PathStyle) -> bool {
    if ours.all {
        return true;
    }
    !theirs.all
        && theirs.roots.iter().all(|text| {
            ours.roots.contains(text)
                || root_of(text, style).is_some_and(|root| covers(ours, &root, style))
        })
}

/// How early an event ends a lease: a task ends inside its session, and a
/// lease that ends at a moment, or never, ends at no event.
fn event_rank(kind: ExpiryKind) -> u8 {
    match kind {
        ExpiryKind::Never | ExpiryKind::At => 0,
        ExpiryKind::EndOfSession => 1,
        ExpiryKind::EndOfTask => 2,
    }
}

/// A moment, read. One that cannot be read is taken to be long past, so
/// that a lease with a time nobody can read is one that has ended.
fn moment(text: &Timestamp) -> UtcTime {
    UtcTime::try_from(text).unwrap_or(EARLIEST)
}

/// The earliest time there is: 0000-01-01.
const EARLIEST: UtcTime = match UtcTime::from_unix(-62_167_219_200, 0) {
    Some(time) => time,
    None => UtcTime::UNIX_EPOCH,
};

/// The moment an expiry names. An `at` with no moment has already ended.
fn expiry_moment(expires: &Expiry) -> Option<(UtcTime, Timestamp)> {
    if expires.kind != ExpiryKind::At {
        return None;
    }
    Some(match &expires.at {
        Some(text) => (moment(text), text.clone()),
        None => (EARLIEST, EARLIEST.to_timestamp()),
    })
}

/// The latest moment a lease can still be in force, from its expiry and
/// its `time` stop conditions. `None` when no moment bounds it.
fn deadline(lease: &Lease) -> Option<UtcTime> {
    let stops = lease.stop_when.iter().filter_map(|condition| match condition {
        StopCondition::Time(stop) => Some(moment(&stop.at)),
        _ => None,
    });
    expiry_moment(&lease.expires)
        .map(|(time, _)| time)
        .into_iter()
        .chain(stops)
        .min()
}

/// Whether `child` ends no later than `parent`: at an event that comes no
/// later, and by a moment that is no later.
fn ends_no_later(child: &Lease, parent: &Lease) -> bool {
    event_rank(child.expires.kind) >= event_rank(parent.expires.kind)
        && no_more(deadline(child), deadline(parent))
}

/// The expiry of a child of two leases. Where one ends at an event and the
/// other at a moment, the event is the expiry and the moment is added to
/// `stop_when`.
fn narrow_expiry(ours: &Expiry, theirs: &Expiry, stop_when: &mut Vec<StopCondition>) -> Expiry {
    let moments: Vec<(UtcTime, Timestamp)> = [ours, theirs]
        .into_iter()
        .filter_map(expiry_moment)
        .collect();
    let kind = if event_rank(ours.kind) >= event_rank(theirs.kind) {
        ours.kind
    } else {
        theirs.kind
    };
    if event_rank(kind) == 0 {
        // Neither ends at an event: the earlier moment, or never.
        return match moments.into_iter().min_by_key(|(time, _)| *time) {
            Some((_, text)) => Expiry {
                kind: ExpiryKind::At,
                at: Some(text),
            },
            None => Expiry {
                kind: ExpiryKind::Never,
                at: None,
            },
        };
    }
    for (_, at) in moments {
        push_new(stop_when, StopCondition::Time(TimeStop { at }));
    }
    Expiry { kind, at: None }
}

/// The terms of a session's standing lease: what configuration, flags and
/// the user's stored approvals come to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandingTerms {
    /// The configured permission mode.
    pub level: Level,
    /// The permission rules in effect.
    pub rules: Vec<Rule>,
    /// What a command may touch.
    pub sandbox: SandboxPolicy,
    /// The models that may be used. Empty: the configured ones.
    pub models: Vec<ModelId>,
    /// The session's limits.
    pub budget: Budget,
    /// What no level relaxes.
    pub limits: HardLimits,
}

/// The lease every session holds: granted by the user, lasting the session.
///
/// # Errors
///
/// When the level cannot be held with the sandbox as it is
/// ([`level::check_standing`]): `autonomous` where nothing confines
/// commands, `guided` before task leases exist, a level this build does
/// not know.
pub fn standing_lease(
    id: LeaseId,
    terms: StandingTerms,
    sandbox: SandboxState,
) -> Result<Lease, LevelError> {
    level::check_standing(&terms.level, sandbox)?;
    Ok(Lease {
        id,
        grant: Grant {
            level: terms.level,
            rules: terms.rules,
            sandbox: terms.sandbox,
            models: terms.models,
            budget: terms.budget,
            limits: terms.limits,
        },
        stop_when: Vec::new(),
        expires: Expiry {
            kind: ExpiryKind::EndOfSession,
            at: None,
        },
        granted_by: Grantor {
            kind: GrantorKind::User,
            lease_id: None,
        },
    })
}

/// The names, at the top of a workspace, that a command must not change:
/// whoever can write them can run code later, outside the sandbox
/// (`docs/architecture/06-permissions-and-sandbox.md`, "The sandbox").
pub const PROTECTED_IN_WORKSPACE: [&str; 11] = [
    ".git/hooks",
    ".git/config",
    ".cortex",
    ".bashrc",
    ".bash_profile",
    ".bash_login",
    ".profile",
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".envrc",
];

/// The default sandbox policy of a session: read anywhere, write only
/// inside the workspace and the directories in `also_writable` (the
/// session's private temporary directory, and `sandbox.write`), no network,
/// and the protected names of the workspace plus `also_protected`
/// (`sandbox.protect`).
#[must_use]
pub fn default_sandbox(
    workspace: &AbsPath,
    also_writable: &[String],
    also_protected: &[String],
) -> SandboxPolicy {
    let mut write = vec![workspace.as_str().to_owned()];
    for root in also_writable {
        push_new(&mut write, root.clone());
    }
    let mut protect: Vec<String> = PROTECTED_IN_WORKSPACE
        .iter()
        .filter_map(|name| workspace.join(name).ok())
        .map(|path| path.as_str().to_owned())
        .collect();
    for root in also_protected {
        push_new(&mut protect, root.clone());
    }
    SandboxPolicy {
        read: PathSet {
            all: true,
            roots: Vec::new(),
        },
        write: PathSet {
            all: false,
            roots: write,
        },
        protect: PathSet {
            all: false,
            roots: protect,
        },
        network: NetworkAccess::Off,
    }
}

#[cfg(test)]
mod tests;
