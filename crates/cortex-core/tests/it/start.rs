//! What a core and a session start with, and what stops them starting
//! (`docs/architecture/08-configuration.md`,
//! `docs/architecture/06-permissions-and-sandbox.md`).

use cortex_core::{ClientInfo, Core};
use cortex_protocol::ErrorKind;
use cortex_protocol::event::{Event, Notice};
use cortex_protocol::methods::{Op, OpResult};
use cortex_protocol::model::ModelRole;
use cortex_protocol::session::SessionSettings;
use serde_json::json;

use crate::{Front, MODEL, World, calls, says};

const LOCAL_MODEL: &str = "[models.local]
provider = \"local\"
id = \"house-model\"
name = \"House model\"
protocol = \"anthropic-messages\"
context_window = 100000
max_output = 8000
input = [\"text\"]

[providers.local]
protocol = \"anthropic-messages\"
base_url = \"http://127.0.0.1:9\"
";

fn start_error(world: &World) -> Box<cortex_protocol::ErrorData> {
    match Core::start(world.options()) {
        Ok(_) => panic!("the core started"),
        Err(error) => error,
    }
}

async fn session_error(world: &World, core: &Core) -> Box<cortex_protocol::ErrorData> {
    match Front::start_as(world, core, ClientInfo::user(), SessionSettings::default()).await {
        Ok(_) => panic!("the session started"),
        Err(error) => error,
    }
}

#[test]
fn an_invalid_entry_of_the_users_catalog_stops_the_core_and_says_where_and_what() {
    let world = World::new("catalog_bad", []);
    // Its output limit is larger than its context window.
    world.user_config(&LOCAL_MODEL.replace("max_output = 8000", "max_output = 800000"));
    let error = start_error(&world);
    assert_eq!(error.kind, ErrorKind::Config);
    assert_eq!(error.code, "invalid_catalog");
    let message = &error.message;
    // The file and the line, the entry, and what is wrong with it.
    assert!(
        message.contains("~/.config/cortex-harness/config.toml:")
            || message.contains("cortex-harness/config.toml:")
            || message.contains("cortex-harness\\config.toml:"),
        "{message}"
    );
    assert!(message.contains("[models.local]"), "{message}");
    assert!(message.contains("local/house-model"), "{message}");
    assert!(message.contains("max_output"), "{message}");
    assert!(message.contains("not started"), "{message}");

    // A key the catalog does not know is refused the same way, by name.
    world.user_config(&LOCAL_MODEL.replace("input = [\"text\"]", "inputs = [\"text\"]"));
    let error = start_error(&world);
    assert_eq!(error.code, "invalid_catalog");
    assert!(error.message.contains("inputs"), "{}", error.message);
    assert!(
        error.message.contains("[models.local]"),
        "{}",
        error.message
    );

    // Something that is not an entry at all.
    world.user_config("[models]\nlocal = \"house-model\"\n");
    let error = start_error(&world);
    assert_eq!(error.code, "invalid_catalog");
    assert!(
        error.message.contains("[models.local]"),
        "{}",
        error.message
    );
}

#[test]
fn a_valid_entry_of_the_users_catalog_is_a_model_like_any_other() {
    let world = World::new("catalog_good", [says("From the house model.")]);
    world.raw_user_config(&format!("[model]\nmain = \"house-model\"\n\n{LOCAL_MODEL}"));
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        let main = &front.state.roles[&ModelRole::Main];
        assert_eq!(main.model.as_str(), "house-model");
        assert_eq!(main.provider.as_str(), "local");
        front.turn("hello").await;
        let request = &world.provider.requests()[0];
        assert_eq!(request.model.info.context_window, 100_000);
        // It is listed with the built-in ones.
        let listed = front
            .op(Op::ModelsList(cortex_protocol::methods::Empty {}))
            .await
            .unwrap();
        let OpResult::ModelsList(listed) = listed else {
            panic!("not models");
        };
        let ids: Vec<&str> = listed
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert!(
            ids.contains(&"house-model") && ids.contains(&MODEL),
            "{ids:?}"
        );
        front.close().await;
    });
}

#[test]
fn a_session_is_refused_when_a_protected_path_cannot_be_placed() {
    let world = World::new("protect_bad", []);
    // No path can hold a NUL: nothing can place this entry.
    world.user_config("[sandbox]\nprotect = [\"secrets\\u0000hidden\"]\n");
    let core = world.core();
    world.run(async {
        let error = session_error(&world, &core).await;
        assert_eq!(error.kind, ErrorKind::Config);
        assert!(error.message.contains("secrets"), "{}", error.message);
        assert!(
            error.message.contains("sandbox.protect"),
            "{}",
            error.message
        );
    });
    assert!(world.logs().is_empty());
}

#[test]
fn a_session_is_refused_at_a_level_that_cannot_be_held_and_told_when_there_is_no_sandbox() {
    let world = World::new("level_refused", [says("Hi.")]);
    world.user_config("[permissions]\nmode = \"autonomous\"\n");
    let core = world.core();
    world.run(async {
        let error = session_error(&world, &core).await;
        assert_eq!(error.code, "sandbox_required");
        assert_eq!(error.kind, ErrorKind::Permission);
    });

    // At a level that can be held, the session starts and says that
    // nothing confines its commands.
    world.user_config("");
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("hi").await;
        let sandbox: Vec<String> = front
            .events()
            .into_iter()
            .filter_map(|event| match event.event {
                Event::Notice(Notice::Sandbox(notice)) => Some(notice.message),
                _ => None,
            })
            .collect();
        assert_eq!(sandbox.len(), 1, "{sandbox:?}");
        assert!(sandbox[0].starts_with("sandbox: off."), "{}", sandbox[0]);
        front.close().await;
    });
}

#[test]
fn a_session_needs_a_model_and_a_key() {
    let world = World::new("no_model", []);
    world.raw_user_config("");
    let core = world.core();
    world.run(async {
        let error = session_error(&world, &core).await;
        assert_eq!(
            (error.kind.clone(), error.code.as_str()),
            (ErrorKind::Config, "no_model")
        );
    });

    // With a model and the real provider layer, and no key anywhere.
    world.user_config("");
    let core = world.core_with(|options| options.connector = None);
    world.run(async {
        let error = session_error(&world, &core).await;
        assert_eq!(error.kind, ErrorKind::Auth);
        assert_eq!(error.code, "no_credentials");
        assert!(
            error.message.contains("ANTHROPIC_API_KEY"),
            "{}",
            error.message
        );
    });

    // A provider whose protocol this build does not speak.
    world.raw_user_config(&format!(
        "[model]\nmain = \"house-model\"\n\n{}",
        LOCAL_MODEL.replace(
            "[providers.local]\nprotocol = \"anthropic-messages\"",
            "[providers.local]\nprotocol = \"gopher\""
        )
    ));
    let core = world.core_with(|options| options.connector = None);
    world.run(async {
        let error = session_error(&world, &core).await;
        assert_eq!(error.code, "unknown_protocol");
        assert!(error.message.contains("gopher"), "{}", error.message);
    });
}

#[test]
fn the_harnesss_own_configuration_is_closed_to_the_tools() {
    let world = World::new(
        "protected_own",
        [
            calls(&[(
                "c1",
                "read",
                json!({"path": world_config_path_placeholder()}),
            )]),
            says("Could not."),
        ],
    );
    // The call names the real file.
    let config = world.dirs().user_config.unwrap();
    world.provider.push(calls(&[(
        "c2",
        "read",
        json!({"path": config.to_str().unwrap()}),
    )]));
    world.provider.push(says("Could not either."));
    let core = world.core();
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .unwrap();
        front.turn("read a file that is not there").await;
        front.turn("read your own configuration").await;
        let results = front.results();
        let own = results.last().unwrap();
        assert_ne!(own.1, "success", "the configuration was read: {}", own.2);
        assert!(!own.2.contains(MODEL), "{}", own.2);
        front.close().await;
    });
}

fn world_config_path_placeholder() -> &'static str {
    "no-such-file.txt"
}

/// The configuration directory reached through a link is protected as
/// where it really is.
#[cfg(unix)]
#[test]
fn a_configuration_directory_behind_a_link_is_protected_where_it_really_is() {
    let world = World::new("protected_link", []);
    // Move the configuration to another place, and leave a link.
    let config_dir = world.dirs().config.unwrap();
    let real = world.dir.join("elsewhere").join("real-config");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::rename(&config_dir, &real).unwrap();
    std::os::unix::fs::symlink(&real, &config_dir).unwrap();

    let real_file = real.join("config.toml");
    world.provider.push(calls(&[(
        "c1",
        "read",
        json!({"path": real_file.to_str().unwrap()}),
    )]));
    world.provider.push(says("Could not."));
    let core = world.core();
    world.run(async {
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::headless(),
            SessionSettings::default(),
        )
        .await
        .unwrap();
        assert_eq!(front.state.roles[&ModelRole::Main].model.as_str(), MODEL);
        front.turn("read the real file").await;
        let result = &front.results()[0];
        assert_ne!(
            result.1, "success",
            "read through its real path: {}",
            result.2
        );
        assert!(!result.2.contains(MODEL), "{}", result.2);
        front.close().await;
    });
}
