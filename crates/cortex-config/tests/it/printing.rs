//! No printed form of anything this crate makes holds a key
//! (`docs/security.md`, "Secrets in logs and session files").
//!
//! One made-up key, the canary, is put everywhere a key can be: in the
//! environment, in a provider's literal header in the user's file, the
//! project's file and on the command line, in the credential store and on
//! the command line as a flag. Then every public type that can hold a key
//! or the environment is printed every way a log line could print it. The
//! whole values are printed, not the fields known to hold a key today, so a
//! field added later is checked by the same test.

use std::sync::Arc;

use cortex_base::{FakeClock, Secret, SharedClock};
use cortex_config::credentials::{self, Lookup};
use cortex_config::{
    CredentialStore, FileStore, KeyPath, LayerKind, LiveConfig, Override, schema::EnvText,
};
use cortex_testkit::{Printed, assert_never_printed};

use crate::{Machine, write};

const KEY: &str = "sk-test-canary-config-0001";

fn header(name: &str) -> String {
    format!(
        "[providers.{name}]\nheaders = {{ api-key = \"{KEY}\", X-Team = {{ env = \"TEAM\" }} }}\n"
    )
}

/// Where else a user's file can hold a key: in a web address, and in a
/// reserved section, whose keys a later step declares (an MCP server's
/// environment, a hook's key, a proxy's password). It goes on from the
/// provider's table that [`header`] opens.
fn elsewhere() -> String {
    format!(
        "base_url = \"https://me:{KEY}@gateway.example.test/v1\"\n\
         [mcp.servers.helper]\ncommand = \"helper-server\"\n\
         [mcp.servers.helper.env]\nHELPER_TOKEN = \"{KEY}\"\n\
         [hooks.audit]\ncommand = \"audit\"\napi_key = \"{KEY}\"\n\
         [http]\nproxy = \"http://me:{KEY}@proxy.example.test:8080\"\n\
         [tui]\ntheme = \"dusk\"\n"
    )
}

#[test]
fn no_debug_or_display_form_holds_a_key() {
    let mut machine = Machine::new("printing_canary");
    machine.options.env.set("ANTHROPIC_API_KEY", KEY);
    machine.options.overrides.push(
        Override::parse(&format!("providers.cli.headers={{ api-key = \"{KEY}\" }}")).unwrap(),
    );
    machine
        .layer(
            LayerKind::User,
            &format!("{}{}", header("azure"), elsewhere()),
        )
        .layer(
            LayerKind::Project,
            &format!(
                "{}[mcp.servers.theirs.env]\nTHEIR_TOKEN = \"{KEY}\"\n",
                header("gateway")
            ),
        );
    let env = machine.env().clone();
    let loaded = machine.load();

    // The key is really in the configuration, where the code that sends a
    // request will look for it.
    let literal = |provider: &str| match &loaded.settings.providers[provider].headers.0["api-key"] {
        EnvText::Literal(secret) => secret.expose().to_owned(),
        EnvText::Env(name) => panic!("a literal was read as the variable {name}"),
    };
    assert_eq!(literal("azure"), KEY);
    assert_eq!(literal("cli"), KEY);
    assert!(
        !loaded.settings.providers.contains_key("gateway"),
        "a project may not set a provider's headers"
    );
    assert_eq!(
        loaded.section("mcp").unwrap()["servers"]["helper"]["env"]["HELPER_TOKEN"].as_str(),
        Some(KEY),
        "the step that reads a reserved section gets what was written"
    );
    assert_eq!(loaded.withheld.len(), 1, "the project's MCP server waits");
    // And it is redacted where the configuration is shown, with the name of
    // a variable, and what is not a secret, left in view.
    let shown = loaded.show(&env, true);
    for line in [
        "providers.azure.headers = { X-Team = { env = \"TEAM\" }, api-key = \"[redacted]\" }",
        "providers.azure.base_url = \"https://[redacted]@gateway.example.test/v1\"",
        "mcp.servers.helper.command = \"helper-server\"",
        "mcp.servers.helper.env.HELPER_TOKEN = \"[redacted]\"",
        "hooks.audit.api_key = \"[redacted]\"",
        "http.proxy = \"http://[redacted]@proxy.example.test:8080\"",
        "tui.theme = \"dusk\"",
    ] {
        assert!(shown.contains(line), "{line:?} not in:\n{shown}");
    }

    // A live configuration, and a reload whose previous configuration held
    // the key.
    let clock: SharedClock = Arc::new(FakeClock::default());
    let mut live = LiveConfig::start(machine.options.clone(), Arc::clone(&clock)).unwrap();
    let reload = live.update(|options| {
        options
            .overrides
            .push(Override::parse("model.speed_fallback=false").unwrap());
    });
    assert!(matches!(reload, cortex_config::Reload::Applied { .. }));

    // The credential store holds it, a flag gives it, and the environment.
    let store = FileStore::new(
        machine.home().join("cortex").join("credentials.toml"),
        Arc::clone(&clock),
    );
    store.set("example", &Secret::new(KEY)).unwrap();
    let flag = Secret::new(KEY);
    let lookup = Lookup {
        provider: "anthropic",
        flag: Some(&flag),
        variable: Some("ANTHROPIC_API_KEY"),
        env: &env,
        store: &store,
    };
    let from_flag = credentials::resolve(lookup).unwrap().unwrap();
    let from_env = credentials::resolve(Lookup {
        flag: None,
        ..lookup
    })
    .unwrap()
    .unwrap();
    let from_store = credentials::resolve(Lookup {
        provider: "example",
        flag: None,
        ..lookup
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        [
            from_flag.secret.expose(),
            from_env.secret.expose(),
            from_store.secret.expose()
        ],
        [KEY; 3]
    );
    let status = credentials::status(lookup).unwrap();

    // A credentials file with the key on a line that does not parse.
    let broken_path = machine.home().join("broken").join("credentials.toml");
    write(
        &broken_path,
        &format!("[providers.example]\nkey = \"{KEY}\n"),
    );
    let broken = FileStore::new(&broken_path, clock);
    let error = broken.get("example").unwrap_err();

    // A configuration file with the key on a line that does not parse.
    machine.layer(
        LayerKind::User,
        &format!("[providers.azure]\nheaders = {{ api-key = \"{KEY}\" \n"),
    );
    let failure = machine.try_load().unwrap_err();

    let headers = KeyPath::parse("providers.azure.headers").unwrap();
    let mut printed = vec![
        Printed::debug("Env", &env),
        Printed::debug("LoadOptions", &machine.options),
        Printed::debug("Override", &machine.options.overrides[0]),
        Printed::debug("Loaded", &loaded),
        Printed::debug("Settings", &loaded.settings),
        Printed::debug("ProviderSettings", &loaded.settings.providers["azure"]),
        Printed::debug("Headers", &loaded.settings.providers["azure"].headers),
        Printed::debug("Withheld", &loaded.withheld),
        Printed::debug("Loaded::value", &loaded.value(&headers)),
        Printed::debug("Loaded::effective", &loaded.effective()),
        Printed::display("config show", &loaded.show(&env, false)),
        Printed::display("config show --origin", &shown),
        Printed::debug("LiveConfig", &live),
        Printed::debug("Reload", &reload),
        Printed::debug("Lookup", &lookup),
        Printed::debug("FileStore", &store),
        Printed::debug("Credential", &from_store),
        Printed::debug("CredentialStatus", &status),
        Printed::both("CredentialSource", &from_env.source),
        Printed::both("CredentialError", &error),
        Printed::both("LoadFailure", &failure),
        Printed::display("LoadFailure::render", &failure.render(&env)),
    ];
    for layer in LayerKind::ALL {
        printed.push(Printed::display(
            format!("config show --layer {layer}"),
            &loaded.show_layer(layer, &env),
        ));
    }
    assert_never_printed(KEY, &printed);
}
