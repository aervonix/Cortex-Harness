//! Loading as a whole: files that cannot be used, the command line, the
//! environment, and what a loaded configuration can be asked.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use cortex_config::schema::StoreChoice;
use cortex_config::{Dirs, LayerKind, Override};
use cortex_protocol::{ModelRole, Speed};

use crate::{Machine, layer_of, value, write};

#[test]
fn with_no_file_anywhere_the_defaults_load() {
    let machine = Machine::new("load_nothing");
    let loaded = machine.load();
    assert_eq!(loaded.settings, cortex_config::Settings::default());
    assert!(loaded.diagnostics.is_empty() && loaded.withheld.is_empty());
    assert_eq!(loaded.profile, None);
    let files: Vec<(LayerKind, bool)> = loaded
        .files
        .iter()
        .map(|(layer, _, present)| (*layer, *present))
        .collect();
    assert_eq!(
        files,
        [
            (LayerKind::Managed, false),
            (LayerKind::User, false),
            (LayerKind::Project, false),
            (LayerKind::ProjectLocal, false),
        ]
    );
}

#[test]
fn with_no_workspace_there_are_no_project_layers() {
    let mut machine = Machine::new("load_no_workspace");
    machine.layer(LayerKind::Project, "[model]\nmain = \"theirs\"\n");
    machine.options.workspace = None;
    let loaded = machine.load();
    assert_eq!(loaded.settings.model.main, None);
    assert_eq!(loaded.files.len(), 2);
}

/// The Windows row of the coverage table: a file with a byte-order mark and
/// CRLF line endings parses, and its lines are counted the same.
#[test]
fn a_file_with_a_byte_order_mark_and_crlf_line_endings_loads() {
    let machine = Machine::new("load_bom_crlf");
    let text = "[model]\nmain = \"big\"\n\n[retry]\nattempts = \"x\"\n";
    let windows = format!("\u{feff}{}", text.replace('\n', "\r\n"));
    write(&machine.file(LayerKind::User), &windows);
    assert_eq!(
        machine.failure(),
        [
            "error: ~/cortex/config.toml:5: retry.attempts: expected a whole number that is not \
          negative"
        ]
    );
    write(
        &machine.file(LayerKind::User),
        &windows.replace("\"x\"", "4"),
    );
    let loaded = machine.load();
    assert_eq!(loaded.settings.model.main.as_deref(), Some("big"));
    assert_eq!(loaded.settings.retry.attempts, 4);
}

#[test]
fn every_error_in_every_layer_is_reported_in_one_go() {
    let machine = Machine::new("load_all_errors");
    machine
        .layer(LayerKind::Managed, "[retry]\nattempts = 99\n")
        .layer(LayerKind::User, "[model]\nmian = \"x\"\nmax_output = 0\n")
        .layer(LayerKind::Project, "[sandbox\n")
        .layer(LayerKind::ProjectLocal, "[limits.day]\nfiles = 3\n");
    let said = machine.failure();
    assert_eq!(said.len(), 5, "{said:#?}");
    assert_eq!(
        said[0],
        "error: ~/managed/config.toml:2: retry.attempts: 99 is out of range: expected 1 to 20"
    );
    // Within a file, in key order.
    assert_eq!(
        said[1],
        "error: ~/cortex/config.toml:3: model.max_output: 0 is out of range: expected 1 to \
         10000000"
    );
    assert_eq!(
        said[2],
        "warning: ~/cortex/config.toml:2: model.mian: unknown key, ignored. Did you mean `main`?"
    );
    assert!(
        said[3].starts_with("error: ~/work/app/.cortex/config.toml:1: is not valid TOML:"),
        "{}",
        said[3]
    );
    assert!(
        said[4].starts_with(
            "error: ~/work/app/.cortex/config.local.toml:2: limits.day.files: unknown key in a \
             table that is enforced."
        ),
        "{}",
        said[4]
    );
}

#[test]
fn strict_checking_turns_an_unknown_key_into_an_error() {
    let mut machine = Machine::new("load_strict");
    machine.layer(LayerKind::User, "[model]\nmian = \"x\"\n");
    let loaded = machine.load();
    assert_eq!(machine.said(&loaded).len(), 1);
    machine.options.strict = true;
    assert_eq!(
        machine.failure(),
        ["error: ~/cortex/config.toml:2: model.mian: unknown key. Did you mean `main`?"]
    );
}

#[test]
fn a_file_that_cannot_be_text_is_an_error_not_a_layer_to_skip() {
    let machine = Machine::new("load_unreadable");
    let user = machine.file(LayerKind::User);

    fs::create_dir_all(user.parent().unwrap()).unwrap();
    fs::write(&user, [0x66, 0x6f, 0xff, 0xfe, 0x6f]).unwrap();
    assert_eq!(
        machine.failure(),
        ["error: ~/cortex/config.toml: is not UTF-8 text"]
    );

    fs::write(&user, vec![b'#'; 1024 * 1024 + 1]).unwrap();
    assert_eq!(
        machine.failure(),
        ["error: ~/cortex/config.toml: is larger than 1024 KiB, which no configuration file is"]
    );

    // A directory where the file should be. Skipping it would be skipping a
    // layer that exists.
    fs::remove_file(&user).unwrap();
    fs::create_dir_all(&user).unwrap();
    let said = machine.failure();
    assert_eq!(said.len(), 1);
    assert!(
        said[0].starts_with("error: ~/cortex/config.toml: could not be read:"),
        "{said:?}"
    );
}

/// A project's files are found through the path resolver, which follows
/// each link itself. A link to a file of the user's is read like the file;
/// a link to nothing is an error, not a layer to skip. Before, a link to
/// nothing was taken for a missing file.
#[test]
fn a_projects_files_are_found_through_the_path_resolver() {
    let machine = Machine::new("load_project_links");
    let project = machine.file(LayerKind::Project);
    let local = machine.file(LayerKind::ProjectLocal);
    let elsewhere = machine.home().join("shared").join("project.toml");
    write(&elsewhere, "[model]\nreasoning = \"none\"\n");
    fs::create_dir_all(project.parent().unwrap()).unwrap();
    if !crate::make_link(&elsewhere, &project, false) {
        return;
    }
    let loaded = machine.load();
    assert_eq!(value(&loaded, "model.reasoning"), "\"none\"");
    // Named as the project's file, wherever it was read from.
    assert_eq!(
        crate::layer_of(&loaded, "model.reasoning"),
        "project"
    );

    assert!(crate::make_link(
        &machine.home().join("gone.toml"),
        &local,
        false
    ));
    let said = machine.failure();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].starts_with("error: ~/work/app/.cortex/config.local.toml: is not opened:")
            && said[0].contains("symbolic link to something that does not exist"),
        "{said:?}"
    );
}

/// A link to a network share in a project is refused before the network is
/// asked: following it would send the account's credentials to the server
/// it names (`docs/security.md`, section 7). The share does not exist, so
/// a load that followed the link would wait for its name to fail.
#[cfg(windows)]
#[test]
fn a_link_in_a_project_to_a_network_share_is_refused_unopened() {
    let machine = Machine::new("load_project_share");
    let project = machine.file(LayerKind::Project);
    let folder = project.parent().unwrap();
    // The whole `.cortex` folder, and one file in a real folder.
    if !crate::make_link(
        &std::path::Path::new(crate::NO_SUCH_SHARE).join("cortex"),
        folder,
        true,
    ) {
        return;
    }
    let said = machine.failure();
    assert_eq!(said.len(), 2, "both project files are refused: {said:?}");
    for line in &said {
        assert!(
            line.contains("is not opened:")
                && line.contains(r"network share `\\cortex-no-such-host.invalid\share`"),
            "{said:?}"
        );
    }

    fs::remove_dir(folder).unwrap();
    fs::create_dir_all(folder).unwrap();
    assert!(crate::make_link(
        &std::path::Path::new(crate::NO_SUCH_SHARE).join("config.toml"),
        &project,
        false
    ));
    let said = machine.failure();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].starts_with(r"error: ~/work/app/.cortex/config.toml: is not opened:"),
        "{said:?}"
    );
}

/// A project can put a link where its configuration file goes, to any
/// file of the user's. What a parse error says about such a file must not
/// repeat any of it. Before, a number too large for TOML was repeated
/// whole.
#[test]
fn a_parse_error_never_repeats_the_file() {
    const KEY: &str = "sk-test-leak-0001";
    for (name, text) in [
        ("environ", format!("ANTHROPIC_API_KEY={KEY}\0PATH=/usr/bin\0")),
        (
            "aws",
            format!("[default]\naws_access_key_id = AKIATESTLEAK0001\naws_secret_access_key = {KEY}\n"),
        ),
        (
            "pem",
            format!("-----BEGIN OPENSSH PRIVATE KEY-----\n{KEY}\n-----END OPENSSH PRIVATE KEY-----\n"),
        ),
        ("netrc", format!("machine example.com login me password {KEY}\n")),
        ("number", format!("key = 1{KEY}\n")),
        ("string", format!("key = \"{KEY}\nnext = 1\n")),
        ("dotted", format!("{KEY} = = 1\n")),
        ("duplicate", format!("a = \"{KEY}\"\na = \"{KEY}\"\n")),
        ("large", format!("a = 99999999999999999999999 # {KEY}\n")),
        ("larger", "a = 1234567890123456789012345678901234567890\n".to_owned()),
    ] {
        let machine = Machine::new(&format!("load_parse_{name}"));
        machine.layer(LayerKind::Project, &text);
        let failure = machine.try_load().expect_err("not TOML");
        let shown = format!("{} / {failure} / {failure:?}", failure.render(machine.env()));
        for secret in [KEY, "AKIATESTLEAK", "1234567890123", "99999999"] {
            assert!(!shown.contains(secret), "{name}: the error repeats the file: {shown}");
        }
        assert!(shown.contains("is not valid TOML"), "{name}: {shown}");
    }
}

#[test]
fn a_command_line_setting_is_read_as_its_key_wants_it() {
    let mut machine = Machine::new("load_cli");
    machine.options.overrides = [
        // Text needs no quotes, and may have them.
        "model.main=claude-opus-5-5",
        "model.fast=\"claude-haiku-5-5\"",
        // A number, a truth value, a length of time, a list.
        "retry.attempts=3",
        "model.speed_fallback=false",
        "retry.max_wait=45s",
        "sandbox.protect=[\"a\", \"b\"]",
        // Text that happens to read as a number stays text where the key
        // takes text.
        "model.effort=1e3",
        "model.judge=007",
        "permissions.mode=read-only",
        // A reserved section takes anything.
        "tui.theme=dusk",
        // A quoted part in the key.
        "providers.\"my.server\".base_url=http://localhost:1/v1?a=b",
    ]
    .into_iter()
    .map(|argument| Override::parse(argument).unwrap())
    .collect();
    let loaded = machine.load();
    for (key, expected) in [
        ("model.main", "\"claude-opus-5-5\""),
        ("model.fast", "\"claude-haiku-5-5\""),
        ("retry.attempts", "3"),
        ("model.speed_fallback", "false"),
        ("retry.max_wait", "\"45s\""),
        ("sandbox.protect", "[\"a\", \"b\"]"),
        ("model.effort", "\"1e3\""),
        ("model.judge", "\"007\""),
        ("permissions.mode", "\"read-only\""),
        ("tui.theme", "\"dusk\""),
        (
            "providers.\"my.server\".base_url",
            "\"http://localhost:1/v1?a=b\"",
        ),
    ] {
        assert_eq!(value(&loaded, key), expected, "{key}");
    }
    assert_eq!(layer_of(&loaded, "model.main"), "command-line");
    assert!(loaded.diagnostics.is_empty());
}

#[test]
fn a_mistake_on_the_command_line_is_always_an_error() {
    let mut machine = Machine::new("load_cli_errors");
    for (argument, expected) in [
        (
            "model.mian=x",
            "error: command-line: model.mian: unknown key. Did you mean `main`?",
        ),
        ("nonsense=1", "error: command-line: nonsense: unknown key."),
        (
            "retry.attempts=many",
            "error: command-line: retry.attempts: expected a whole number that is not negative",
        ),
        (
            "retry.attempts=0",
            "error: command-line: retry.attempts: 0 is out of range: expected 1 to 20",
        ),
        (
            "permissions.mode=yolo",
            "error: command-line: permissions.mode: expected one of `read-only`, `supervised`, \
             `guided`, `autonomous` or `unrestricted`",
        ),
        (
            "model=big",
            "error: command-line: model: this is a table of settings; set one of its keys",
        ),
        (
            "providers.x.headers.Authorization=abc",
            "error: command-line: providers.x.headers.Authorization: `headers` is one setting \
             and is set whole",
        ),
        (
            "profile=review",
            "error: command-line: profile: this key describes a file and cannot be set on the \
             command line. To choose a profile, use `--profile`",
        ),
        (
            "managed.locked=[]",
            "error: command-line: managed.locked: this key describes a file and cannot be set \
             on the command line. To choose a profile, use `--profile`",
        ),
        (
            "model..main=x",
            "error: command-line: `model..main` is not a key: a part is empty or has a \
             character that needs quotes",
        ),
    ] {
        machine.options.overrides = vec![Override::parse(argument).unwrap()];
        assert_eq!(machine.failure(), [expected], "{argument}");
    }
}

#[test]
fn the_environment_layer_is_a_short_fixed_list() {
    let mut machine = Machine::new("load_env");
    let data = machine.home().join("big-disk");
    machine.options.env.set("CORTEX_LOG", "debug");
    machine
        .options
        .env
        .set("CORTEX_DATA_DIR", data.to_str().unwrap());
    // Not a general override mechanism: other names do nothing.
    machine.options.env.set("CORTEX_MODEL_MAIN", "sneaky");
    machine
        .options
        .env
        .set("CORTEX_PERMISSIONS_MODE", "unrestricted");
    let loaded = machine.load();
    assert_eq!(value(&loaded, "log.level"), "\"debug\"");
    assert_eq!(layer_of(&loaded, "session.data_dir"), "environment");
    assert_eq!(loaded.data_dir(), Some(data));
    assert_eq!(loaded.settings.model.main, None);
    assert_eq!(value(&loaded, "permissions.mode"), "\"supervised\"");

    machine.options.env.set("CORTEX_DATA_DIR", "relative/place");
    let loaded = machine.load();
    assert_eq!(
        machine.said(&loaded),
        [
            "warning: environment: CORTEX_DATA_DIR is `relative/place`, which is not an absolute \
          path; it is ignored"
        ]
    );
    assert_eq!(layer_of(&loaded, "session.data_dir"), "default");
}

/// `CORTEX_DATA_DIR` sets `session.data_dir` in the environment layer and
/// nowhere else, so a managed lock on the key holds against it. Before,
/// the platform's directory was read from the variable as well, and a
/// locked data directory left at its default moved with it.
#[test]
fn the_data_directory_variable_cannot_move_a_locked_data_directory() {
    let mut machine = Machine::new("load_data_dir_lock");
    machine.layer(
        LayerKind::Managed,
        "[managed]\nlocked = [\"session.data_dir\"]\n",
    );
    let before = machine.load().data_dir();
    let elsewhere = machine.home().join("elsewhere");
    machine
        .options
        .env
        .set("CORTEX_DATA_DIR", elsewhere.to_str().unwrap());
    // As the binary does: the directories from the environment as it is.
    let managed = machine.options.dirs.managed_config.clone();
    machine.options.dirs = Dirs::from_env(&machine.options.env);
    machine.options.dirs.managed_config = managed;
    let loaded = machine.load();
    assert_eq!(loaded.data_dir(), before);
    assert_eq!(
        machine.said(&loaded),
        ["note: environment: session.data_dir: locked by the managed configuration; this value \
          is not used"]
    );

    // Without the lock the variable moves it.
    machine.layer(LayerKind::Managed, "");
    assert_eq!(machine.load().data_dir(), Some(elsewhere));
}

#[test]
fn the_data_directory_comes_from_configuration_or_the_platform() {
    let machine = Machine::new("load_data_dir");
    let home = machine.home();
    let usual = machine.load().data_dir().unwrap();
    assert!(usual.starts_with(&home), "{}", usual.display());
    assert!(usual.ends_with("cortex-harness"), "{}", usual.display());

    machine.layer(LayerKind::User, "[session]\ndata_dir = \"~/sessions\"\n");
    assert_eq!(machine.load().data_dir(), Some(home.join("sessions")));
    machine.layer(LayerKind::User, "[session]\ndata_dir = \"sessions-here\"\n");
    assert_eq!(
        machine.load().data_dir(),
        Some(PathBuf::from("sessions-here"))
    );
}

#[test]
fn the_settings_later_steps_read_are_typed() {
    let machine = Machine::new("load_typed");
    machine.layer(
        LayerKind::User,
        r#"
        [model]
        main = "claude-opus-5-5"
        speed = "fast"
        speed_fallback = false
        speed_wait = "0s"
        speed_cooldown = "30m"

        [model.roles.fast]
        speed = "fast"

        [providers.local]
        first_event_timeout = "5m"

        [retry]
        first_event_timeout = "45s"

        [credentials]
        store = "file"

        [limits.session]
        cost = 2.5
        "#,
    );
    let loaded = machine.load();
    let settings = &loaded.settings;
    assert_eq!(settings.model.speed_for(&ModelRole::Main), Speed::Fast);
    assert_eq!(settings.model.speed_for(&ModelRole::Fast), Speed::Fast);
    assert_eq!(settings.model.speed_for(&ModelRole::Judge), Speed::Standard);
    assert!(!settings.model.speed_fallback);
    assert_eq!(settings.model.speed_wait, Duration::ZERO);
    assert_eq!(settings.model.speed_cooldown, Duration::from_secs(1800));
    assert_eq!(
        settings.retry_for("local").first_event_timeout,
        Duration::from_secs(300)
    );
    assert_eq!(
        settings.retry_for("anthropic").first_event_timeout,
        Duration::from_secs(45)
    );
    assert_eq!(settings.credentials.store, StoreChoice::File);
    assert_eq!(
        settings.limits.session.cost_usd_nanos(),
        Some(2_500_000_000)
    );
    // A reserved section is handed over as it was written.
    assert!(loaded.section("tui").is_none());
    assert!(loaded.section("model").is_some());
}
