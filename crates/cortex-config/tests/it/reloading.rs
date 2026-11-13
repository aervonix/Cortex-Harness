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
    // the configuration and the options stay.
    let reload = live.update(|options| options.profile = Some("missing".to_owned()));
    assert!(matches!(reload, Reload::Rejected(_)));
    assert_eq!(live.current().settings.model.main.as_deref(), Some("small"));
    assert_eq!(live.options().profile.as_deref(), Some("quick"));
}

/// A change of options that does not load is undone, and says so every
/// time. Before, the options kept the change that failed, and a second
/// attempt that failed the same way said `Unchanged`, as if it had worked.
#[test]
fn a_change_of_options_that_does_not_load_is_undone_and_refused_every_time() {
    let machine = Machine::new("reload_options_refused");
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
    let before = live.current();

    for _ in 0..2 {
        let reload = live.update(|options| options.profile = Some("missing".to_owned()));
        assert!(matches!(reload, Reload::Rejected(_)), "{reload:?}");
        assert_eq!(live.options().profile, None);
        assert!(Arc::ptr_eq(&live.current(), &before));
    }

    // While a file is broken, every change is refused with the error, not
    // only the first, and none of them is kept.
    write_at(&machine, LayerKind::User, "[retry]\nattempts = 0\n", 50);
    let Some(Reload::Rejected(broken)) = live.check() else {
        panic!("expected the broken file to be reported");
    };
    for _ in 0..2 {
        assert_eq!(
            live.update(|options| options.trust = ProjectTrust::Trusted),
            Reload::Rejected(broken.clone())
        );
        assert_eq!(live.options().trust, ProjectTrust::Untrusted);
        // The broken file is the one that defines the profile, so its
        // absence is a second error.
        let Reload::Rejected(failure) =
            live.update(|options| options.profile = Some("quick".to_owned()))
        else {
            panic!("expected the profile to be refused");
        };
        assert_eq!(failure.diagnostics[0], broken.diagnostics[0]);
        assert_eq!(failure.errors().count(), 2);
        assert_eq!(live.options().profile, None);
    }
    assert!(Arc::ptr_eq(&live.current(), &before));
    assert_eq!(live.current().withheld.len(), 1, "the network stays held back");

    // The file is still watched under the options that stayed: fixing it
    // is noticed, and then the change goes through.
    write_at(
        &machine,
        LayerKind::User,
        "[profiles.quick]\nmodel.main = \"small\"\n[model]\nmain = \"big\"\n",
        40,
    );
    assert_eq!(live.check(), Some(Reload::Unchanged));
    assert!(matches!(
        live.update(|options| options.trust = ProjectTrust::Trusted),
        Reload::Applied { .. }
    ));
    assert_eq!(live.current().settings.sandbox.network.as_str(), "full");
}

/// Taking trust away always takes effect, even while a file has an error,
/// whichever file it is. The project's layers leave the configuration in
/// force and the error is reported. Before, the trusted project's allow
/// rule stayed in force, and a second attempt said `Unchanged`.
#[test]
fn lowering_trust_takes_effect_while_a_file_is_broken() {
    const USER: &str = "[model]\nmain = \"big\"\n";
    const PROJECT: &str = "[sandbox]\nprotect = [\"vendor\"]\n\
                           [[permissions.rules]]\neffect = \"exec\"\nmatch = \"curl *\"\n\
                           action = \"allow\"\n";
    for (broken, fixed) in [(LayerKind::User, USER), (LayerKind::Project, PROJECT)] {
        let mut machine = Machine::new(&format!("reload_revoke_{broken}"));
        machine.trusted();
        write_at(&machine, LayerKind::User, USER, 60);
        write_at(&machine, LayerKind::Project, PROJECT, 60);
        let (mut live, _clock) = start(&machine);
        assert_eq!(live.current().rules().len(), 1, "trusted: the allow rule applies");

        write_at(&machine, broken, "[retry]\nattempts = 0\n", 50);
        assert!(matches!(live.check(), Some(Reload::Rejected(_))));

        let reload = live.update(|options| options.trust = ProjectTrust::Untrusted);
        let Reload::ProjectDropped {
            previous,
            changes,
            failure,
        } = reload
        else {
            panic!("{broken}: expected the project to be dropped, got {reload:?}");
        };
        assert!(
            failure
                .render(machine.env())
                .contains("retry.attempts: 0 is out of range"),
            "{broken}: the error is reported"
        );
        assert_eq!(previous.rules().len(), 1);
        let changed: Vec<String> = changes.iter().map(|c| c.key.to_string()).collect();
        assert_eq!(changed, ["permissions.rules", "sandbox.protect"], "{broken}");

        // Trust is lowered, nothing of the project is in force, and the
        // user's own configuration, as it was in force, stays.
        let current = live.current();
        assert_eq!(live.options().trust, ProjectTrust::Untrusted);
        assert!(current.rules().is_empty(), "{broken}");
        assert_eq!(value(&current, "sandbox.protect"), "[]", "{broken}");
        assert_eq!(value(&current, "model.main"), "\"big\"", "{broken}");
        assert!(
            current
                .files
                .iter()
                .all(|(kind, ..)| !kind.is_project())
        );

        // The same error is not reported again until a file changes.
        assert!(live.check().is_none());
        assert_eq!(live.reload(), Reload::Unchanged);

        // Once the file is fixed, the project is back, held to the trust
        // rules: its protected path applies and its allow rule waits.
        write_at(&machine, broken, fixed, 40);
        assert!(matches!(live.check(), Some(Reload::Applied { .. })));
        let current = live.current();
        assert!(current.rules().is_empty(), "{broken}");
        assert_eq!(value(&current, "sandbox.protect"), "[\"vendor\"]");
        assert_eq!(current.withheld.len(), 1);
    }
}

/// The user selected a profile that only the project defines, and takes
/// trust away while a file is broken: the project and its profile go.
#[test]
fn lowering_trust_drops_a_profile_only_the_project_defines() {
    let mut machine = Machine::new("reload_revoke_profile");
    machine.trusted();
    write_at(&machine, LayerKind::User, "profile = \"theirs\"\n", 60);
    write_at(
        &machine,
        LayerKind::Project,
        "[profiles.theirs]\nsandbox.network = \"full\"\n",
        60,
    );
    let (mut live, _clock) = start(&machine);
    assert_eq!(live.current().profile.as_deref(), Some("theirs"));
    assert_eq!(value(&live.current(), "sandbox.network"), "\"full\"");

    write_at(&machine, LayerKind::Project, "[retry\n", 50);
    let reload = live.update(|options| options.trust = ProjectTrust::Untrusted);
    assert!(matches!(reload, Reload::ProjectDropped { .. }), "{reload:?}");
    let current = live.current();
    assert_eq!(current.profile, None);
    assert_eq!(value(&current, "sandbox.network"), "\"off\"");
    let said = machine.said(&current);
    assert!(
        said.iter()
            .any(|line| line.contains("`theirs` was defined by the project alone")),
        "{said:?}"
    );
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

/// A link that appears in the project after the load is not followed by
/// the next check: the project's files are found again through the path
/// resolver first, and the configuration is reloaded through it. The check
/// asks the file system about each file, and that follows links, which on
/// Windows sends the account's credentials to a share a link names.
///
/// Here the link leads to nothing, which every OS can show: followed, it
/// looks like the missing file it stands for, and the check says nothing.
/// Found again, it is refused and reported.
#[test]
fn a_link_that_appears_in_the_project_is_found_before_it_is_followed() {
    let machine = Machine::new("reload_link_appears");
    write_at(&machine, LayerKind::User, "[model]\nmain = \"first\"\n", 60);
    let (mut live, clock) = start(&machine);
    let folder = machine
        .file(LayerKind::Project)
        .parent()
        .unwrap()
        .to_owned();
    assert!(!folder.exists(), "no `.cortex` at the load");

    if !crate::make_link(&machine.home().join("nowhere"), &folder, true) {
        return;
    }
    let Some(Reload::Rejected(failure)) = live.check() else {
        panic!("expected the link to be found and refused");
    };
    let said = failure.render(machine.env());
    assert!(
        said.contains("is not opened:") && said.contains("does not exist"),
        "{said}"
    );
    // Reported once, and the configuration stays.
    assert!(live.check().is_none());
    assert_eq!(value(&live.current(), "model.main"), "\"first\"");

    // The link goes and a real folder comes: noticed, and loaded.
    remove_link(&folder);
    write_at(
        &machine,
        LayerKind::Project,
        "[model]\nreasoning = \"none\"\n",
        30,
    );
    assert!(matches!(live.check(), Some(Reload::Applied { .. })));
    assert_eq!(value(&live.current(), "model.reasoning"), "\"none\"");

    // The same on the check on a key press.
    fs::remove_dir_all(&folder).unwrap();
    assert!(crate::make_link(&machine.home().join("nowhere"), &folder, true));
    clock.advance(Duration::from_secs(2));
    assert!(matches!(
        live.check_at_most_every(Duration::from_secs(1)),
        Some(Reload::Rejected(_))
    ));
}

/// Removes a link to a folder: a file on Unix, a folder on Windows.
fn remove_link(link: &Path) {
    fs::remove_file(link)
        .or_else(|_| fs::remove_dir(link))
        .unwrap();
}

/// The same with a link to a network share, on Windows, where it matters.
/// The share does not exist: a check that followed the link would wait for
/// its name to fail, and then see nothing new.
#[cfg(windows)]
#[test]
fn a_link_to_a_share_that_appears_after_the_load_is_never_followed() {
    let machine = Machine::new("reload_share_appears");
    write_at(&machine, LayerKind::User, "[model]\nmain = \"first\"\n", 60);
    let (mut live, _clock) = start(&machine);
    let folder = machine
        .file(LayerKind::Project)
        .parent()
        .unwrap()
        .to_owned();
    if !crate::make_link(
        &Path::new(crate::NO_SUCH_SHARE).join("cortex"),
        &folder,
        true,
    ) {
        return;
    }
    let Some(Reload::Rejected(failure)) = live.check() else {
        panic!("expected the link to be found and refused");
    };
    let said = failure.render(machine.env());
    assert!(
        said.contains(r"network share `\\cortex-no-such-host.invalid\share`"),
        "{said}"
    );
    assert!(live.check().is_none(), "reported once");
}
