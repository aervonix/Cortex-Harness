//! Reloading without a restart, and the rule that an invalid file never
//! takes the configuration away
//! (`docs/architecture/08-configuration.md`, "Reloading without a restart").
//!
//! Time is the fake clock's, and each file's modification time is set, not
//! waited for: a file is given a time well before the clock's "now", so
//! that it counts as settled, and a different one on every write, so that
//! the write is seen.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use cortex_base::FakeClock;
use cortex_config::{Effect, KeyPath, LayerKind, LiveConfig, ProjectTrust, Reload, Severity};

use crate::{Machine, value};

/// The fake clock's time when a test starts.
const NOW: &str = "2026-11-12T12:00:00Z";

/// Writes a layer's file and gives it a modification time `minutes` before
/// the clock's start.
fn write_at(machine: &Machine, layer: LayerKind, text: &str, minutes: u64) {
    machine.layer(layer, text);
    set_time(&machine.file(layer), minutes);
}

fn set_time(file: &Path, minutes: u64) {
    let now = cortex_base::UtcTime::parse(NOW).unwrap();
    let start = SystemTime::UNIX_EPOCH + Duration::from_secs(now.unix_secs().unsigned_abs());
    fs::File::options()
        .write(true)
        .open(file)
        .unwrap()
        .set_modified(start - Duration::from_secs(60 * minutes))
        .unwrap();
}

fn start(machine: &Machine) -> (LiveConfig, Arc<FakeClock>) {
    let clock = Arc::new(FakeClock::starting_at(NOW).unwrap());
    let live = LiveConfig::start(machine.options.clone(), clock.clone())
        .unwrap_or_else(|failure| panic!("{}", failure.render(machine.env())));
    (live, clock)
}

#[test]
fn a_reload_with_an_invalid_file_keeps_the_old_configuration() {
    let machine = Machine::new("reload_invalid");
    write_at(
        &machine,
        LayerKind::User,
        "[model]\nmain = \"first\"\n[retry]\nattempts = 3\n",
        60,
    );
    let (mut live, _clock) = start(&machine);
    let mut before = live.current();
    assert_eq!(value(&before, "model.main"), "\"first\"");
    assert!(live.check().is_none(), "nothing has changed yet");

    // Each kind of broken file: not TOML, a wrong type, an unknown key in an
    // enforced table, a newer version.
    for (broken, expected) in [
        ("[model\nmain = \"second\"\n", "is not valid TOML"),
        (
            "[model]\nmain = \"second\"\n[retry]\nattempts = \"many\"\n",
            "retry.attempts: expected a whole number",
        ),
        (
            "[model]\nmain = \"second\"\n[permissions]\nallow_everything = true\n",
            "is refused",
        ),
        (
            "config_version = 9\n[model]\nmain = \"second\"\n",
            "written for configuration version 9",
        ),
    ] {
        write_at(&machine, LayerKind::User, broken, 50);
        match live.check() {
            Some(Reload::Rejected(failure)) => {
                let text = failure.render(machine.env());
                assert!(text.contains(expected), "{expected:?} not in:\n{text}");
                assert!(failure.errors().count() >= 1);
                assert!(
                    failure
                        .errors()
                        .all(|error| error.severity == Severity::Error)
                );
            }
            other => panic!("expected a rejected reload for {broken:?}, got {other:?}"),
        }
        // The configuration in force is the one from before, the very same
        // one, and it is complete.
        let current = live.current();
        assert!(Arc::ptr_eq(&current, &before), "{broken:?}");
        assert_eq!(value(&current, "model.main"), "\"first\"");
        assert_eq!(current.settings.retry.attempts, 3);

        // The same broken file is not reported again on the next check,
        // and not on a forced reload either.
        assert!(live.check().is_none());
        assert_eq!(live.reload(), Reload::Unchanged);
        assert!(Arc::ptr_eq(&live.current(), &before));

        write_at(
            &machine,
            LayerKind::User,
            "[model]\nmain = \"first\"\n[retry]\nattempts = 3\n",
            40,
        );
        assert_eq!(
            live.check(),
            Some(Reload::Unchanged),
            "fixed, and the same as before"
        );
        // The fixed file is the one in force now, with the same settings.
        assert_eq!(live.current().settings, before.settings);
        before = live.current();
    }
}

#[test]
fn a_valid_change_is_applied_and_says_when_each_part_takes_effect() {
    let machine = Machine::new("reload_applied");
    write_at(&machine, LayerKind::User, "[model]\nmain = \"first\"\n", 60);
    let (mut live, _clock) = start(&machine);
    let before = live.current();

    write_at(
        &machine,
        LayerKind::User,
        "[model]\nmain = \"second\"\nspeed = \"fast\"\n[retry]\nattempts = 2\n\
         [session]\ndata_dir = \"~/elsewhere\"\n[tui]\ntheme = \"dusk\"\n",
        30,
    );
    let Some(Reload::Applied { previous, changes }) = live.check() else {
        panic!("expected the change to be applied");
    };
    assert!(Arc::ptr_eq(&previous, &before));
    let changes: Vec<(String, Effect)> = changes
        .into_iter()
        .map(|change| (change.key.to_string(), change.effect))
        .collect();
    assert_eq!(
        changes,
        [
            ("model.main".to_owned(), Effect::NextEpoch),
            ("model.speed".to_owned(), Effect::Now),
            ("retry.attempts".to_owned(), Effect::Now),
            ("session.data_dir".to_owned(), Effect::Restart),
            // A section whose keys are not declared yet: the cautious answer.
            ("tui.theme".to_owned(), Effect::NextEpoch),
        ]
    );
    let current = live.current();
    assert_eq!(value(&current, "model.main"), "\"second\"");
    assert_eq!(current.settings.retry.attempts, 2);
    assert!(live.check().is_none(), "the change is reported once");

    // A key that goes back to its default is a change too.
    write_at(
        &machine,
        LayerKind::User,
        "[model]\nmain = \"second\"\n",
        20,
    );
    let Some(Reload::Applied { changes, .. }) = live.check() else {
        panic!("expected the change to be applied");
    };
    assert_eq!(changes.len(), 4);
    assert_eq!(live.current().settings.retry.attempts, 5);
}

#[test]
fn a_file_that_appears_or_goes_is_a_change() {
    let machine = Machine::new("reload_appear");
    let (mut live, _clock) = start(&machine);
    assert_eq!(live.current().settings.model.main, None);
    assert!(live.current().files.iter().all(|(_, _, present)| !present));

    // The project's file is created after start-up.
    write_at(
        &machine,
        LayerKind::Project,
        "[model]\nmain = \"theirs\"\n",
        30,
    );
    assert!(matches!(live.check(), Some(Reload::Applied { .. })));
    assert_eq!(
        live.current().settings.model.main.as_deref(),
        Some("theirs")
    );

    fs::remove_file(machine.file(LayerKind::Project)).unwrap();
    assert!(matches!(live.check(), Some(Reload::Applied { .. })));
    assert_eq!(live.current().settings.model.main, None);
}

#[test]
fn a_file_saved_without_a_change_is_not_announced() {
    let machine = Machine::new("reload_same");
    write_at(&machine, LayerKind::User, "[model]\nmain = \"first\"\n", 60);
    let (mut live, _clock) = start(&machine);
    let before = live.current();

    // Rewritten with a comment and a different time: a change on disk, and
    // no change in what is in effect.
    write_at(
        &machine,
        LayerKind::User,
        "# a note\n[model]\nmain = \"first\"\n",
        30,
    );
    assert_eq!(live.check(), Some(Reload::Unchanged));
    // The new one is in force all the same, so that origins are right.
    let after = live.current();
    assert!(!Arc::ptr_eq(&after, &before));
    assert_eq!(
        after.origin(&KeyPath::parse("model.main").unwrap()).line,
        Some(3)
    );
}

#[test]
fn a_change_of_options_reloads() {
    let machine = Machine::new("reload_options");
    write_at(
        &machine,
        LayerKind::User,
        "[profiles.quick]\nmodel.main = \"small\"\n[model]\nmain = \"big\"\n",
        60,
    );
    write_at(
        &machine,
        LayerKind::Project,
        "[sandbox]\nnetwork = \"full\"\n",
        60,
    );
    let (mut live, _clock) = start(&machine);
    assert_eq!(live.current().withheld.len(), 1);

    // `/profile quick`.
    let reload = live.update(|options| options.profile = Some("quick".to_owned()));
    assert!(matches!(reload, Reload::Applied { .. }));
    assert_eq!(live.current().settings.model.main.as_deref(), Some("small"));
    assert_eq!(live.options().profile.as_deref(), Some("quick"));

    // The user trusts the project.
    let reload = live.update(|options| options.trust = ProjectTrust::Trusted);
    let Reload::Applied { changes, .. } = reload else {
        panic!("expected the change to be applied");
    };
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].key.to_string(), "sandbox.network");
    assert!(live.current().withheld.is_empty());

    // A profile that does not exist is refused like any other error, and
    // the configuration stays.
    let reload = live.update(|options| options.profile = Some("missing".to_owned()));
    assert!(matches!(reload, Reload::Rejected(_)));
    assert_eq!(live.current().settings.model.main.as_deref(), Some("small"));
}

#[test]
fn the_check_on_a_key_press_runs_at_most_once_a_second() {
    let machine = Machine::new("reload_interval");
    write_at(&machine, LayerKind::User, "[model]\nmain = \"first\"\n", 60);
    let (mut live, clock) = start(&machine);
    let second = Duration::from_secs(1);
    assert!(live.check_at_most_every(second).is_none());

    write_at(
        &machine,
        LayerKind::User,
        "[model]\nmain = \"second\"\n",
        30,
    );
    assert!(
        live.check_at_most_every(second).is_none(),
        "a check ran less than a second ago, so nothing is looked at"
    );
    assert_eq!(live.current().settings.model.main.as_deref(), Some("first"));

    clock.advance(Duration::from_millis(1500));
    assert!(matches!(
        live.check_at_most_every(second),
        Some(Reload::Applied { .. })
    ));
    assert_eq!(
        live.current().settings.model.main.as_deref(),
        Some("second")
    );
    assert!(clock.sleeps().is_empty(), "nothing waited");
}

#[test]
fn starting_with_an_invalid_file_is_an_error_with_nothing_to_fall_back_to() {
    let machine = Machine::new("reload_start_invalid");
    write_at(&machine, LayerKind::User, "[retry]\nattempts = 0\n", 60);
    let clock = Arc::new(FakeClock::starting_at(NOW).unwrap());
    let failure = LiveConfig::start(machine.options.clone(), clock).unwrap_err();
    assert_eq!(
        failure.render(machine.env()),
        "error: ~/cortex/config.toml:2: retry.attempts: 0 is out of range: expected 1 to 20"
    );
}
