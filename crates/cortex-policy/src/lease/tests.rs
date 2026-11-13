//! Narrowing and the subset test: worked cases, then the properties the
//! build plan names for step 12.

use cortex_base::{Rng, SeededRng};
use cortex_protocol::lease::{Effect, OutOfScopeStop, SpendingStop, VerificationFailedStop};
use cortex_testkit::cases::{self, Outcome, ensure};

use super::*;
use crate::level::{NoSandbox, rank};

const POSIX: PathStyle = PathStyle::Posix;

fn rule(effect: Effect, pattern: &str, action: RuleAction) -> Rule {
    Rule {
        effect,
        pattern: pattern.to_owned(),
        action,
    }
}

fn set(roots: &[&str]) -> PathSet {
    PathSet {
        all: false,
        roots: roots.iter().map(|root| (*root).to_owned()).collect(),
    }
}

fn everything() -> PathSet {
    PathSet {
        all: true,
        roots: Vec::new(),
    }
}

fn at(text: &str) -> Expiry {
    Expiry {
        kind: ExpiryKind::At,
        at: Some(Timestamp::new(text)),
    }
}

fn ends(kind: ExpiryKind) -> Expiry {
    Expiry { kind, at: None }
}

fn lease(id: &str, level: Level) -> Lease {
    Lease {
        id: LeaseId::new(id),
        grant: Grant {
            level,
            rules: Vec::new(),
            sandbox: SandboxPolicy {
                read: everything(),
                write: set(&["/work/app"]),
                protect: set(&["/work/app/.git/hooks"]),
                network: NetworkAccess::Off,
            },
            models: Vec::new(),
            budget: Budget::default(),
            limits: HardLimits::default(),
        },
        stop_when: Vec::new(),
        expires: ends(ExpiryKind::EndOfSession),
        granted_by: Grantor {
            kind: GrantorKind::User,
            lease_id: None,
        },
    }
}

fn narrow(parent: &Lease, requested: &Lease) -> Lease {
    narrow_in(parent, requested, POSIX).unwrap()
}

fn within(child: &Lease, parent: &Lease) -> bool {
    is_within_in(child, parent, POSIX)
}

#[test]
fn a_child_takes_the_lower_level_and_never_unrestricted() {
    let supervised = lease("l_parent", Level::Supervised);
    let autonomous = lease("l_child", Level::Autonomous);
    let unrestricted = lease("l_x", Level::Unrestricted);
    assert_eq!(
        narrow(&supervised, &autonomous).grant.level,
        Level::Supervised
    );
    assert_eq!(
        narrow(&autonomous, &supervised).grant.level,
        Level::Supervised
    );
    // `unrestricted` is not passed down, even by a parent that holds it.
    assert_eq!(
        narrow(&unrestricted, &unrestricted).grant.level,
        Level::Autonomous
    );
    assert_eq!(
        narrow(&unrestricted, &supervised).grant.level,
        Level::Supervised
    );
    // A level nobody knows grants nothing, and stays.
    let unknown = lease("l_u", Level::Unknown("omnipotent".to_owned()));
    assert_eq!(
        narrow(&supervised, &unknown).grant.level,
        unknown.grant.level
    );
    assert_eq!(
        narrow(&unknown, &supervised).grant.level,
        unknown.grant.level
    );
}

#[test]
fn a_child_is_named_by_the_proposal_and_granted_by_its_parent() {
    let parent = lease("l_parent", Level::Supervised);
    let child = narrow(&parent, &lease("l_child", Level::Supervised));
    assert_eq!(child.id, LeaseId::new("l_child"));
    assert_eq!(child.granted_by.kind, GrantorKind::Parent);
    assert_eq!(child.granted_by.lease_id, Some(LeaseId::new("l_parent")));
}

#[test]
fn deny_and_ask_rules_are_carried_over_and_an_allow_needs_the_parents() {
    let deny = rule(Effect::Write, "**/.env*", RuleAction::Deny);
    let ask = rule(Effect::Exec, "git push *", RuleAction::Ask);
    let shared = rule(Effect::Exec, "cargo test *", RuleAction::Allow);
    let parents_own = rule(Effect::Exec, "cargo build *", RuleAction::Allow);
    let childs_own = rule(Effect::Exec, "rm *", RuleAction::Allow);
    let childs_deny = rule(Effect::Read, "secrets/**", RuleAction::Deny);
    // The same pattern for another effect is another rule.
    let other_effect = rule(Effect::Read, "cargo test *", RuleAction::Allow);
    // A narrower pattern under a wider parent rule is not "the same".
    let narrower = rule(Effect::Exec, "cargo build --release", RuleAction::Allow);

    let mut parent = lease("l_parent", Level::Supervised);
    parent.grant.rules = vec![
        deny.clone(),
        ask.clone(),
        shared.clone(),
        parents_own.clone(),
    ];
    let mut requested = lease("l_child", Level::Supervised);
    requested.grant.rules = vec![
        shared.clone(),
        childs_own.clone(),
        childs_deny.clone(),
        other_effect.clone(),
        narrower.clone(),
    ];

    let child = narrow(&parent, &requested);
    assert_eq!(child.grant.rules, [deny, ask, childs_deny, shared]);
    assert!(within(&child, &parent) && within(&child, &requested));

    // A child that drops a deny, or adds an allow, is not within.
    let mut dropped = child.clone();
    dropped.grant.rules.remove(0);
    assert!(!within(&dropped, &parent));
    let mut added = child.clone();
    added.grant.rules.push(childs_own);
    assert!(!within(&added, &parent));
}

#[test]
fn writable_roots_are_intersected_and_protected_paths_joined() {
    let mut parent = lease("l_parent", Level::Supervised);
    parent.grant.sandbox.write = set(&["/work/app", "/tmp/session"]);
    parent.grant.sandbox.protect = set(&["/work/app/.git/hooks"]);
    let mut requested = lease("l_child", Level::Supervised);
    requested.grant.sandbox.write = set(&["/work/app/src", "/work", "/srv/other", "not a path"]);
    requested.grant.sandbox.protect = set(&["/work/app/src/generated"]);
    requested.grant.sandbox.read = set(&["/work"]);

    let child = narrow(&parent, &requested);
    // `/work/app` is inside the proposal's `/work`, and the proposal's
    // `/work/app/src` is inside the parent's `/work/app`. `/srv/other` and
    // `/tmp/session` are in one only. A root that is not a path is dropped.
    assert_eq!(
        child.grant.sandbox.write,
        set(&["/work/app", "/work/app/src"])
    );
    assert_eq!(child.grant.sandbox.read, set(&["/work"]));
    assert_eq!(
        child.grant.sandbox.protect,
        set(&["/work/app/.git/hooks", "/work/app/src/generated"])
    );
    assert!(within(&child, &parent) && within(&child, &requested));

    // Everything writable, narrowed, is the other's roots.
    let mut open = lease("l_open", Level::Supervised);
    open.grant.sandbox.write = everything();
    assert_eq!(
        narrow(&open, &parent).grant.sandbox.write,
        parent.grant.sandbox.write
    );
    assert_eq!(
        narrow(&parent, &open).grant.sandbox.write,
        parent.grant.sandbox.write
    );
    assert_eq!(narrow(&open, &open).grant.sandbox.write, everything());
    assert!(
        !within(&open, &parent),
        "everything is not inside two roots"
    );

    // A child that stops protecting a path is not within.
    let mut unprotected = child.clone();
    unprotected.grant.sandbox.protect = set(&["/work/app/src/generated"]);
    assert!(!within(&unprotected, &parent));
    // Protecting a directory that holds the path is protecting the path.
    let mut wider = child.clone();
    wider.grant.sandbox.protect = set(&["/work/app/.git", "/work/app/src/generated"]);
    assert!(within(&wider, &parent));
}

#[test]
fn a_writable_root_never_reaches_a_folder_that_differs_only_in_case() {
    let windows = PathStyle::Windows;
    let mut parent = lease("l_parent", Level::Supervised);
    parent.grant.sandbox.read = everything();
    parent.grant.sandbox.write = set(&["C:/Work/App"]);
    parent.grant.sandbox.protect = PathSet::default();
    let mut requested = parent.clone();
    requested.id = LeaseId::new("l_child");

    // The same folder, with the drive letter in another case: kept.
    requested.grant.sandbox.write = set(&["c:/Work/App/src"]);
    let child = narrow_in(&parent, &requested, windows).unwrap();
    assert_eq!(child.grant.sandbox.write, set(&["c:/Work/App/src"]));
    assert!(is_within_in(&child, &parent, windows));

    // `app` may be another folder than `App`: it is not inside the root.
    requested.grant.sandbox.write = set(&["C:/Work/app/src"]);
    let child = narrow_in(&parent, &requested, windows).unwrap();
    assert_eq!(child.grant.sandbox.write, set(&[]));
    assert!(!is_within_in(&requested, &parent, windows));
}

#[test]
fn the_network_is_on_only_when_both_have_it_on() {
    let mut on = lease("l_on", Level::Supervised);
    on.grant.sandbox.network = NetworkAccess::Full;
    let off = lease("l_off", Level::Supervised);
    assert_eq!(narrow(&on, &on).grant.sandbox.network, NetworkAccess::Full);
    assert_eq!(narrow(&on, &off).grant.sandbox.network, NetworkAccess::Off);
    assert_eq!(narrow(&off, &on).grant.sandbox.network, NetworkAccess::Off);
    assert!(within(&off, &on) && !within(&on, &off));
}

#[test]
fn models_are_those_both_name_and_none_in_common_is_refused() {
    let models =
        |names: &[&str]| -> Vec<ModelId> { names.iter().map(|name| ModelId::new(*name)).collect() };
    let mut parent = lease("l_parent", Level::Supervised);
    parent.grant.models = models(&["a", "b"]);
    let mut requested = lease("l_child", Level::Supervised);

    // The proposal names none: the parent's.
    assert_eq!(
        narrow(&parent, &requested).grant.models,
        models(&["a", "b"])
    );
    requested.grant.models = models(&["b", "c"]);
    assert_eq!(narrow(&parent, &requested).grant.models, models(&["b"]));
    // The parent names none: the proposal's.
    let any = lease("l_any", Level::Supervised);
    assert_eq!(narrow(&any, &requested).grant.models, models(&["b", "c"]));

    // None in common: an empty list would mean "any", which is wider.
    requested.grant.models = models(&["c"]);
    assert_eq!(
        narrow_in(&parent, &requested, POSIX),
        Err(NarrowError::NoModelInCommon {
            parent: vec!["a".to_owned(), "b".to_owned()],
            requested: vec!["c".to_owned()],
        })
    );
    // And a child that names none is not within a parent that names some.
    assert!(!within(&any, &parent));
    assert!(within(&parent, &any));
}

#[test]
fn budgets_and_hard_limits_take_the_smaller() {
    let mut parent = lease("l_parent", Level::Supervised);
    parent.grant.budget = Budget {
        usd_nanos: Some(2_000_000_000),
        tokens: None,
        requests: Some(50),
        wall_clock_ms: Some(600_000),
    };
    parent.grant.limits = HardLimits {
        merge_to_main: true,
        team_size: Some(4),
    };
    let mut requested = lease("l_child", Level::Supervised);
    requested.grant.budget = Budget {
        usd_nanos: Some(5_000_000_000),
        tokens: Some(1_000_000),
        requests: None,
        wall_clock_ms: Some(60_000),
    };
    requested.grant.limits = HardLimits {
        merge_to_main: false,
        team_size: None,
    };
    let child = narrow(&parent, &requested);
    assert_eq!(
        child.grant.budget,
        Budget {
            usd_nanos: Some(2_000_000_000),
            tokens: Some(1_000_000),
            requests: Some(50),
            wall_clock_ms: Some(60_000),
        }
    );
    assert_eq!(
        child.grant.limits,
        HardLimits {
            merge_to_main: false,
            team_size: Some(4),
        }
    );
    assert!(within(&child, &parent) && within(&child, &requested));

    // What is left of the parent's budget caps the child.
    let left = parent.with_budget(Budget {
        usd_nanos: Some(250_000_000),
        ..parent.grant.budget.clone()
    });
    assert_eq!(
        narrow(&left, &requested).grant.budget.usd_nanos,
        Some(250_000_000)
    );
    // No limit is not within a limit.
    let unlimited = lease("l_free", Level::Supervised);
    assert!(!within(&unlimited, &parent));
}

/// A lease that holds more than its parent in any one respect is not
/// within it. Each line changes one thing in a lease that is otherwise
/// its parent's equal.
#[test]
fn a_lease_with_more_of_any_one_thing_is_not_within_its_parent() {
    let mut parent = lease("l_parent", Level::Supervised);
    parent.grant.budget = Budget {
        usd_nanos: Some(2_000_000_000),
        tokens: Some(1_000_000),
        requests: Some(50),
        wall_clock_ms: Some(600_000),
    };
    parent.grant.limits = HardLimits {
        merge_to_main: false,
        team_size: Some(4),
    };
    parent.grant.rules = vec![rule(Effect::Write, "secrets/**", RuleAction::Deny)];
    parent.grant.models = vec!["model-a".to_owned().into()];
    let equal = {
        let mut child = parent.clone();
        child.id = LeaseId::new("l_child");
        child
    };
    assert!(within(&equal, &parent));

    type Change = fn(&mut Lease);
    let more: [(&str, Change); 14] = [
        ("more money", |l| {
            l.grant.budget.usd_nanos = Some(2_000_000_001)
        }),
        ("no limit on money", |l| l.grant.budget.usd_nanos = None),
        ("more tokens", |l| l.grant.budget.tokens = Some(1_000_001)),
        ("more requests", |l| l.grant.budget.requests = Some(51)),
        ("more time", |l| {
            l.grant.budget.wall_clock_ms = Some(600_001)
        }),
        ("a merge to main", |l| l.grant.limits.merge_to_main = true),
        ("a larger team", |l| l.grant.limits.team_size = Some(5)),
        ("no limit on the team", |l| l.grant.limits.team_size = None),
        ("a higher level", |l| l.grant.level = Level::Autonomous),
        ("the network", |l| {
            l.grant.sandbox.network = NetworkAccess::Full
        }),
        ("another writable root", |l| {
            l.grant.sandbox.write.roots.push("/srv/other".to_owned());
        }),
        ("a deny rule dropped", |l| l.grant.rules.clear()),
        ("an allow rule of its own", |l| {
            l.grant
                .rules
                .push(rule(Effect::Exec, "make *", RuleAction::Allow));
        }),
        ("another model", |l| {
            l.grant.models.push("model-b".to_owned().into())
        }),
    ];
    for (what, change) in more {
        let mut child = equal.clone();
        change(&mut child);
        assert!(!within(&child, &parent), "{what}");
        // And the other way round it is: the parent holds no more than a
        // lease that holds more than it.
        if what != "a deny rule dropped" {
            assert!(
                within(&parent, &child) || what == "an allow rule of its own",
                "{what}"
            );
        }
    }
}

#[test]
fn stop_conditions_are_those_of_both() {
    let failed = StopCondition::VerificationFailed(VerificationFailedStop { rounds: 3 });
    let scope = StopCondition::OutOfScope(OutOfScopeStop {});
    let spent = StopCondition::Spending(SpendingStop {
        usd_nanos: 1_000_000_000,
    });
    let mut parent = lease("l_parent", Level::Supervised);
    parent.stop_when = vec![failed.clone(), scope.clone()];
    let mut requested = lease("l_child", Level::Supervised);
    requested.stop_when = vec![scope.clone(), spent.clone()];
    let child = narrow(&parent, &requested);
    assert_eq!(child.stop_when, [failed, scope, spent]);
    assert!(within(&child, &parent) && within(&child, &requested));
    assert!(!within(&requested, &parent), "it lacks a stop condition");
}

#[test]
fn a_child_ends_no_later_than_either() {
    let early = "2026-11-13T10:00:00Z";
    let late = "2026-11-13T12:00:00Z";
    let with = |expires: Expiry| {
        let mut lease = lease("l", Level::Supervised);
        lease.expires = expires;
        lease
    };
    let time_stop = |text: &str| {
        StopCondition::Time(TimeStop {
            at: Timestamp::new(text),
        })
    };
    let cases: Vec<(Expiry, Expiry, Expiry, Vec<StopCondition>)> = vec![
        (
            ends(ExpiryKind::Never),
            ends(ExpiryKind::Never),
            ends(ExpiryKind::Never),
            vec![],
        ),
        (ends(ExpiryKind::Never), at(late), at(late), vec![]),
        (at(late), at(early), at(early), vec![]),
        (at(early), at(late), at(early), vec![]),
        (
            ends(ExpiryKind::EndOfSession),
            ends(ExpiryKind::Never),
            ends(ExpiryKind::EndOfSession),
            vec![],
        ),
        (
            ends(ExpiryKind::EndOfSession),
            ends(ExpiryKind::EndOfTask),
            ends(ExpiryKind::EndOfTask),
            vec![],
        ),
        // An event and a moment cannot be ordered: the event is the
        // expiry, and the moment becomes a stop condition.
        (
            ends(ExpiryKind::EndOfSession),
            at(late),
            ends(ExpiryKind::EndOfSession),
            vec![time_stop(late)],
        ),
        (
            at(early),
            ends(ExpiryKind::EndOfTask),
            ends(ExpiryKind::EndOfTask),
            vec![time_stop(early)],
        ),
    ];
    for (ours, theirs, expected, stops) in cases {
        let (parent, requested) = (with(ours.clone()), with(theirs.clone()));
        let child = narrow(&parent, &requested);
        assert_eq!(child.expires, expected, "{ours:?} and {theirs:?}");
        assert_eq!(child.stop_when, stops, "{ours:?} and {theirs:?}");
        assert!(within(&child, &parent), "{ours:?} and {theirs:?}");
        assert!(within(&child, &requested), "{ours:?} and {theirs:?}");
    }

    // Later is not within earlier, by expiry or by a time stop.
    assert!(!within(&with(at(late)), &with(at(early))));
    assert!(within(&with(at(early)), &with(at(late))));
    assert!(!within(&with(ends(ExpiryKind::Never)), &with(at(late))));
    assert!(!within(
        &with(ends(ExpiryKind::EndOfSession)),
        &with(ends(ExpiryKind::EndOfTask))
    ));
    let mut stopped = with(ends(ExpiryKind::EndOfSession));
    stopped.stop_when = vec![time_stop(late)];
    assert!(!within(&with(ends(ExpiryKind::EndOfSession)), &stopped));
    assert!(within(&stopped, &with(ends(ExpiryKind::EndOfSession))));
}

#[test]
fn a_time_nobody_can_read_has_already_passed() {
    let mut broken = lease("l_broken", Level::Supervised);
    broken.expires = at("next tuesday");
    let mut missing = lease("l_missing", Level::Supervised);
    missing.expires = Expiry {
        kind: ExpiryKind::At,
        at: None,
    };
    let mut real = lease("l_real", Level::Supervised);
    real.expires = at("2026-11-13T10:00:00Z");
    // Both end before any real time, so each is within a lease that ends
    // at one, and never the other way round.
    for unreadable in [&broken, &missing] {
        assert!(within(unreadable, &real));
        assert!(!within(&real, unreadable));
        assert_eq!(
            narrow(&real, unreadable).expires,
            unreadable_expiry(unreadable)
        );
    }
}

fn unreadable_expiry(lease: &Lease) -> Expiry {
    Expiry {
        kind: ExpiryKind::At,
        at: Some(
            lease
                .expires
                .at
                .clone()
                .unwrap_or_else(|| EARLIEST.to_timestamp()),
        ),
    }
}

#[test]
fn the_standing_lease_lasts_the_session_and_is_the_users() {
    let workspace = AbsPath::parse("/work/app", POSIX).unwrap();
    let terms = |level: Level| StandingTerms {
        level,
        rules: vec![rule(Effect::Exec, "cargo test *", RuleAction::Allow)],
        sandbox: default_sandbox(
            &workspace,
            &["/tmp/cortex-s1".to_owned()],
            &["/work/app/deploy".to_owned()],
        ),
        models: Vec::new(),
        budget: Budget {
            usd_nanos: Some(5_000_000_000),
            ..Budget::default()
        },
        limits: HardLimits::default(),
    };
    let off = SandboxState::NotEnforcing(NoSandbox::NotBuilt);
    let standing = standing_lease(LeaseId::new("l_1"), terms(Level::Supervised), off).unwrap();
    assert_eq!(standing.id, LeaseId::new("l_1"));
    assert_eq!(standing.grant.level, Level::Supervised);
    assert_eq!(standing.expires, ends(ExpiryKind::EndOfSession));
    assert_eq!(standing.granted_by.kind, GrantorKind::User);
    assert!(standing.stop_when.is_empty());
    assert_eq!(standing.grant.budget.usd_nanos, Some(5_000_000_000));

    // The default policy: read anywhere, write the workspace and the
    // session's temporary directory, no network, the protected names.
    let sandbox = &standing.grant.sandbox;
    assert!(sandbox.read.all);
    assert_eq!(sandbox.write, set(&["/work/app", "/tmp/cortex-s1"]));
    assert_eq!(sandbox.network, NetworkAccess::Off);
    for protected in [
        "/work/app/.git/hooks",
        "/work/app/.git/config",
        "/work/app/.cortex",
        "/work/app/.bashrc",
        "/work/app/deploy",
    ] {
        assert!(
            sandbox.protect.roots.contains(&protected.to_owned()),
            "{protected}"
        );
    }

    // It goes through the protocol's strict reader and back unchanged.
    let wire = serde_json::to_value(&standing).unwrap();
    assert_eq!(Lease::from_wire(wire).unwrap(), standing);

    // `autonomous` is refused where nothing confines commands.
    let error = standing_lease(LeaseId::new("l_2"), terms(Level::Autonomous), off).unwrap_err();
    assert!(matches!(error, LevelError::SandboxRequired { .. }));
    assert!(
        standing_lease(
            LeaseId::new("l_3"),
            terms(Level::Autonomous),
            SandboxState::Enforcing
        )
        .is_ok()
    );
    assert_eq!(
        standing_lease(
            LeaseId::new("l_4"),
            terms(Level::Guided),
            SandboxState::Enforcing
        ),
        Err(LevelError::GuidedNotAvailable)
    );
    assert!(standing_lease(LeaseId::new("l_5"), terms(Level::Unrestricted), off).is_ok());
}

// ------------------------------------------------------------- properties

const ROOTS: [&str; 7] = [
    "/work",
    "/work/app",
    "/work/app/src",
    "/work/app/tests",
    "/tmp/session",
    "/srv/data",
    "relative/root",
];

const TIMES: [&str; 4] = [
    "2026-11-13T09:00:00Z",
    "2026-11-13T10:00:00Z",
    "2026-11-13T12:30:00Z",
    "not a time",
];

fn any_path_set(rng: &mut SeededRng) -> PathSet {
    if rng.below(5) == 0 {
        return everything();
    }
    let roots: Vec<&str> = (0..rng.below(4))
        .map(|_| *cases::pick(rng, &ROOTS))
        .collect();
    set(&roots)
}

fn any_limit(rng: &mut SeededRng) -> Option<u64> {
    (rng.below(3) != 0).then(|| rng.below(6))
}

fn any_rules(rng: &mut SeededRng) -> Vec<Rule> {
    let effects = [Effect::Read, Effect::Write, Effect::Exec];
    let patterns = ["**/.env*", "cargo test *", "src/**", "git push *"];
    let actions = [RuleAction::Allow, RuleAction::Ask, RuleAction::Deny];
    (0..rng.below(5))
        .map(|_| {
            rule(
                cases::pick(rng, &effects).clone(),
                cases::pick(rng, &patterns),
                *cases::pick(rng, &actions),
            )
        })
        .collect()
}

fn any_stops(rng: &mut SeededRng) -> Vec<StopCondition> {
    (0..rng.below(3))
        .map(|_| match rng.below(4) {
            0 => StopCondition::VerificationFailed(VerificationFailedStop {
                rounds: 1 + rng.below(3) as u32,
            }),
            1 => StopCondition::OutOfScope(OutOfScopeStop {}),
            2 => StopCondition::Spending(SpendingStop {
                usd_nanos: rng.below(4),
            }),
            _ => StopCondition::Time(TimeStop {
                at: Timestamp::new(*cases::pick(rng, &TIMES)),
            }),
        })
        .collect()
}

fn any_expiry(rng: &mut SeededRng) -> Expiry {
    match rng.below(5) {
        0 => ends(ExpiryKind::Never),
        1 => ends(ExpiryKind::EndOfSession),
        2 => ends(ExpiryKind::EndOfTask),
        3 => Expiry {
            kind: ExpiryKind::At,
            at: None,
        },
        _ => at(cases::pick(rng, &TIMES)),
    }
}

fn any_lease(rng: &mut SeededRng, id: &str) -> Lease {
    let levels = [
        Level::ReadOnly,
        Level::Supervised,
        Level::Guided,
        Level::Autonomous,
        Level::Unrestricted,
        Level::Unknown("later".to_owned()),
    ];
    // Model lists overlap often, so that most pairs do narrow.
    let models: Vec<ModelId> = ["a", "b", "c"]
        .into_iter()
        .filter(|_| rng.below(3) != 0)
        .map(ModelId::new)
        .collect();
    Lease {
        id: LeaseId::new(id),
        grant: Grant {
            level: cases::pick(rng, &levels).clone(),
            rules: any_rules(rng),
            sandbox: SandboxPolicy {
                read: any_path_set(rng),
                write: any_path_set(rng),
                protect: any_path_set(rng),
                network: if rng.below(2) == 0 {
                    NetworkAccess::Off
                } else {
                    NetworkAccess::Full
                },
            },
            models: if rng.below(3) == 0 {
                Vec::new()
            } else {
                models
            },
            budget: Budget {
                usd_nanos: any_limit(rng),
                tokens: any_limit(rng),
                requests: any_limit(rng),
                wall_clock_ms: any_limit(rng),
            },
            limits: HardLimits {
                merge_to_main: rng.below(2) == 0,
                team_size: any_limit(rng).map(|limit| limit as u32),
            },
        },
        stop_when: any_stops(rng),
        expires: any_expiry(rng),
        granted_by: Grantor {
            kind: GrantorKind::User,
            lease_id: None,
        },
    }
}

/// The paths a property asks about: each root, something under it, and
/// places no root covers.
fn probes() -> Vec<AbsPath> {
    [
        "/",
        "/work",
        "/work/app",
        "/work/app/src",
        "/work/app/src/x.rs",
        "/work/app/tests/t.rs",
        "/work/other",
        "/tmp/session/a",
        "/tmp",
        "/srv/data/d",
        "/srv",
        "/etc/passwd",
    ]
    .into_iter()
    .map(|path| AbsPath::parse(path, POSIX).unwrap())
    .collect()
}

/// What a narrowed lease may not exceed, stated field by field and without
/// [`is_within_in`], so that the two cannot be wrong together.
fn never_wider(child: &Lease, parent: &Lease) -> Outcome {
    let (ours, theirs) = (&child.grant, &parent.grant);
    // Level.
    match (rank(&ours.level), rank(&theirs.level)) {
        (Some(child_rank), Some(parent_rank)) => {
            ensure!(child_rank <= parent_rank, "the level went up");
        }
        (Some(_), None) => return Err("a known level under an unknown one".into()),
        (None, _) => {}
    }
    ensure!(
        ours.level != Level::Unrestricted,
        "`unrestricted` was passed down"
    );
    // Rules.
    for rule in &theirs.rules {
        ensure!(
            rule.action == RuleAction::Allow || ours.rules.contains(rule),
            "the parent's {rule:?} was dropped"
        );
    }
    for rule in &ours.rules {
        ensure!(
            rule.action != RuleAction::Allow || theirs.rules.contains(rule),
            "{rule:?} is an allow the parent does not have"
        );
    }
    // Paths: nothing the child may read or write that the parent may not,
    // and nothing the parent protects that the child does not.
    for path in probes() {
        ensure!(
            !covers(&ours.sandbox.write, &path, POSIX)
                || covers(&theirs.sandbox.write, &path, POSIX),
            "{path} became writable"
        );
        ensure!(
            !covers(&ours.sandbox.read, &path, POSIX) || covers(&theirs.sandbox.read, &path, POSIX),
            "{path} became readable"
        );
        ensure!(
            !covers(&theirs.sandbox.protect, &path, POSIX)
                || covers(&ours.sandbox.protect, &path, POSIX),
            "{path} lost its protection"
        );
    }
    for root in &theirs.sandbox.protect.roots {
        let still_protected = ours.sandbox.protect.all
            || ours.sandbox.protect.roots.contains(root)
            || AbsPath::parse(root, POSIX)
                .is_ok_and(|path| covers(&ours.sandbox.protect, &path, POSIX));
        ensure!(still_protected, "the protected root {root} was dropped");
    }
    ensure!(
        ours.sandbox.network == NetworkAccess::Off || theirs.sandbox.network == NetworkAccess::Full,
        "the network came on"
    );
    // Models.
    if !theirs.models.is_empty() {
        ensure!(!ours.models.is_empty(), "the models became any");
        for model in &ours.models {
            ensure!(theirs.models.contains(model), "{model} is not the parent's");
        }
    }
    // Budget and hard limits.
    let budgets = [
        (ours.budget.usd_nanos, theirs.budget.usd_nanos, "money"),
        (ours.budget.tokens, theirs.budget.tokens, "tokens"),
        (ours.budget.requests, theirs.budget.requests, "requests"),
        (
            ours.budget.wall_clock_ms,
            theirs.budget.wall_clock_ms,
            "time",
        ),
        (
            ours.limits.team_size.map(u64::from),
            theirs.limits.team_size.map(u64::from),
            "team size",
        ),
    ];
    for (child_limit, parent_limit, what) in budgets {
        if let Some(parent_limit) = parent_limit {
            ensure!(
                child_limit.is_some_and(|limit| limit <= parent_limit),
                "the budget for {what} grew: {child_limit:?} over {parent_limit}"
            );
        }
    }
    ensure!(
        !ours.limits.merge_to_main || theirs.limits.merge_to_main,
        "merging to the main branch came on"
    );
    // Stop conditions and expiry.
    for condition in &parent.stop_when {
        ensure!(
            child.stop_when.contains(condition),
            "{condition:?} was dropped"
        );
    }
    ensure!(
        event_rank(child.expires.kind) >= event_rank(parent.expires.kind),
        "the child ends at a later event"
    );
    if let Some(parent_deadline) = deadline(parent) {
        ensure!(
            deadline(child).is_some_and(|child_deadline| child_deadline <= parent_deadline),
            "the child ends later: {:?} after {parent_deadline}",
            deadline(child)
        );
    }
    Ok(())
}

#[test]
fn narrow_is_always_within_both_inputs() {
    cases::check(
        "narrow_is_within_both",
        400,
        |rng| (any_lease(rng, "l_parent"), any_lease(rng, "l_child")),
        |(parent, requested)| {
            let Ok(child) = narrow_in(parent, requested, POSIX) else {
                // No lease inside both: the only reason is the models.
                ensure!(
                    !parent.grant.models.is_empty()
                        && !requested.grant.models.is_empty()
                        && requested
                            .grant
                            .models
                            .iter()
                            .all(|model| !parent.grant.models.contains(model)),
                    "refused, and the models have one in common"
                );
                return Ok(());
            };
            ensure!(
                is_within_in(&child, parent, POSIX),
                "not within its parent: {child:?}"
            );
            ensure!(
                is_within_in(&child, requested, POSIX),
                "not within the proposal: {child:?}"
            );
            never_wider(&child, parent)?;
            never_wider(&child, requested)?;
            Ok(())
        },
    );
}

#[test]
fn a_narrowed_lease_never_has_a_higher_level_a_later_expiry_or_a_larger_budget() {
    cases::check(
        "narrowed_is_never_more",
        3000,
        |rng| (any_lease(rng, "l_parent"), any_lease(rng, "l_child")),
        |(parent, requested)| {
            let Ok(child) = narrow_in(parent, requested, POSIX) else {
                return Ok(());
            };
            ensure!(
                level::no_higher_than(&child.grant.level, &parent.grant.level),
                "the level went up"
            );
            ensure!(ends_no_later(&child, parent), "it ends later");
            for (child_limit, parent_limit) in [
                (child.grant.budget.usd_nanos, parent.grant.budget.usd_nanos),
                (child.grant.budget.tokens, parent.grant.budget.tokens),
                (child.grant.budget.requests, parent.grant.budget.requests),
                (
                    child.grant.budget.wall_clock_ms,
                    parent.grant.budget.wall_clock_ms,
                ),
            ] {
                ensure!(no_more(child_limit, parent_limit), "a budget grew");
            }
            Ok(())
        },
    );
}

#[test]
fn no_sequence_of_narrowing_widens_a_lease() {
    cases::check(
        "narrowing_never_widens",
        250,
        |rng| {
            let root = any_lease(rng, "l_root");
            let proposals: Vec<Lease> = (0..1 + rng.below(4))
                .map(|step| any_lease(rng, &format!("l_{step}")))
                .collect();
            (root, proposals)
        },
        |(root, proposals)| {
            let mut current = root.clone();
            for proposal in proposals {
                let Ok(next) = narrow_in(&current, proposal, POSIX) else {
                    continue;
                };
                ensure!(
                    is_within_in(&next, &current, POSIX),
                    "wider than the one before"
                );
                ensure!(is_within_in(&next, root, POSIX), "wider than the first");
                never_wider(&next, &current)?;
                never_wider(&next, root)?;
                current = next;
            }
            Ok(())
        },
    );
}

#[test]
fn narrowing_a_lease_under_itself_gives_nothing_it_did_not_have() {
    cases::check(
        "narrow_under_itself",
        300,
        |rng| any_lease(rng, "l_same"),
        |lease| {
            let child = narrow_in(lease, lease, POSIX)?;
            never_wider(&child, lease)?;
            ensure!(is_within_in(&child, lease, POSIX), "not within itself");
            Ok(())
        },
    );
}

#[test]
fn within_is_transitive_and_agrees_with_the_field_by_field_check() {
    cases::check(
        "within_is_transitive",
        500,
        |rng| {
            let top = any_lease(rng, "l_top");
            // Leases made by narrowing are within each other often enough
            // to exercise the cases that matter.
            let middle =
                narrow_in(&top, &any_lease(rng, "l_mid"), POSIX).unwrap_or_else(|_| top.clone());
            let bottom = narrow_in(&middle, &any_lease(rng, "l_low"), POSIX)
                .unwrap_or_else(|_| middle.clone());
            // Sometimes break the chain, so that "not within" is seen too.
            let stray = any_lease(rng, "l_stray");
            match rng.below(4) {
                0 => (stray, middle, top),
                1 => (bottom, stray, top),
                _ => (bottom, middle, top),
            }
        },
        |(low, middle, top)| {
            if is_within_in(low, middle, POSIX) && is_within_in(middle, top, POSIX) {
                ensure!(is_within_in(low, top, POSIX), "within is not transitive");
            }
            for (child, parent) in [(low, middle), (middle, top), (low, top)] {
                if is_within_in(child, parent, POSIX) && child.grant.level != Level::Unrestricted {
                    never_wider(child, parent)?;
                }
            }
            Ok(())
        },
    );
}
