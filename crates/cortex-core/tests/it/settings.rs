//! Settings: at session start and changed while a session runs
//! (`docs/architecture/11-rpc-protocol.md`, "Settings and state";
//! `docs/decisions/0027-speed-tiers.md`).

use cortex_config::Override;
use cortex_core::ClientInfo;
use cortex_protocol::event::TurnOutcome;
use cortex_protocol::lease::Level;
use cortex_protocol::methods::{ClientRole, Op, OpResult, SessionSetParams};
use cortex_protocol::model::ModelRole;
use cortex_protocol::session::{RoleSettings, SessionSettings, SessionState};
use cortex_protocol::{ModelId, Speed};
use serde_json::json;

use crate::{Front, MODEL, OTHER_MODEL, World, calls, says};

const UNCACHED: &str = "without the provider's cached prefix";

async fn set(
    front: &Front,
    settings: SessionSettings,
) -> Result<SessionState, cortex_core::OpError> {
    let result = front
        .op(Op::SessionSet(SessionSetParams {
            session_id: front.session.clone(),
            settings,
        }))
        .await?;
    let OpResult::SessionSet(result) = result else {
        panic!("not the result of session/set");
    };
    Ok(result.session)
}

fn role(settings: RoleSettings) -> SessionSettings {
    SessionSettings {
        roles: [(ModelRole::Main, settings)].into_iter().collect(),
        ..SessionSettings::default()
    }
}

fn warned(front: &Front) -> usize {
    front.drain();
    front
        .notices()
        .iter()
        .filter(|notice| notice.contains(UNCACHED))
        .count()
}

#[test]
fn a_session_can_be_started_at_a_speed_from_its_settings_or_from_the_command_line() {
    let world = World::new("speed_start", [says("One."), says("Two.")]);
    world.run(async {
        // As `session/new` gives it.
        let core = world.core();
        let front = Front::start_as(
            &world,
            &core,
            ClientInfo::user(),
            SessionSettings {
                speed: Some(Speed::Fast),
                ..SessionSettings::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(front.state.speed, Speed::Fast);
        let main = &front.state.roles[&ModelRole::Main];
        assert_eq!(
            (&main.speed, &main.speed_in_effect),
            (&Speed::Fast, &Speed::Fast)
        );
        front.turn("go").await;
        assert_eq!(world.provider.requests()[0].speed, Speed::Fast);
        front.close().await;

        // As `--speed fast` gives it: an override of the user's own, so it
        // waits for nothing.
        let core = world.core_with(|options| {
            options.overrides.push(Override {
                key: "model.speed".to_owned(),
                value: "fast".to_owned(),
                flag: Some("--speed".to_owned()),
            });
        });
        let front = Front::start(&world, &core).await;
        assert_eq!(front.state.speed, Speed::Fast);
        front.turn("go").await;
        assert_eq!(world.provider.requests()[1].speed, Speed::Fast);
        front.close().await;
    });
}

#[test]
fn the_speed_can_be_changed_and_the_state_says_what_is_set_and_what_is_in_effect() {
    let world = World::new("speed_set", [says("One."), says("Two."), says("Three.")]);
    world.user_config(&format!("fast = \"{OTHER_MODEL}\"\n"));
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        assert_eq!(front.state.speed, Speed::Standard);
        // Before anything was sent there is no cache to lose: no warning.
        let state = set(
            &front,
            SessionSettings {
                speed: Some(Speed::Fast),
                ..SessionSettings::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(state.speed, Speed::Fast);
        assert_eq!(state.roles[&ModelRole::Main].speed_in_effect, Speed::Fast);
        assert_eq!(warned(&front), 0);
        // A utility role does not follow the session's speed.
        assert_eq!(state.roles[&ModelRole::Fast].speed, Speed::Standard);

        front.turn("one").await;
        assert_eq!(world.provider.requests()[0].speed, Speed::Fast);

        // Back to standard, with history: the user is told what it costs.
        let state = set(
            &front,
            SessionSettings {
                speed: Some(Speed::Standard),
                ..SessionSettings::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(state.speed, Speed::Standard);
        assert_eq!(warned(&front), 1);
        front.turn("two").await;
        assert_eq!(world.provider.requests()[1].speed, Speed::Standard);

        // A role's own speed: the utility role's model has no fast tier,
        // so the state shows fast as set and standard as in effect.
        let state = set(
            &front,
            SessionSettings {
                roles: [(
                    ModelRole::Fast,
                    RoleSettings {
                        speed: Some(Speed::Fast),
                        ..RoleSettings::default()
                    },
                )]
                .into_iter()
                .collect(),
                ..SessionSettings::default()
            },
        )
        .await
        .unwrap();
        let utility = &state.roles[&ModelRole::Fast];
        assert_eq!(utility.speed, Speed::Fast);
        assert_eq!(utility.speed_in_effect, Speed::Standard);
        // The main role's own speed wins over the session's.
        let state = set(
            &front,
            role(RoleSettings {
                speed: Some(Speed::Fast),
                ..RoleSettings::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(state.speed, Speed::Standard);
        assert_eq!(state.roles[&ModelRole::Main].speed, Speed::Fast);
        front.turn("three").await;
        assert_eq!(world.provider.requests()[2].speed, Speed::Fast);
        front.close().await;
    });
}

#[test]
fn a_roles_model_and_reasoning_level_can_be_changed_in_a_running_session() {
    let world = World::new(
        "model_set",
        [
            says("From the first."),
            says("From the second."),
            says("Third."),
        ],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("one").await;
        assert_eq!(world.provider.requests()[0].model.info.id.as_str(), MODEL);
        assert_eq!(world.provider.requests()[0].reasoning.level, None);

        // Another model, and a level in the portable names.
        let state = set(
            &front,
            role(RoleSettings {
                model: Some(ModelId::new(OTHER_MODEL)),
                effort: Some("minimal".to_owned()),
                ..RoleSettings::default()
            }),
        )
        .await
        .unwrap();
        let main = &state.roles[&ModelRole::Main];
        assert_eq!(main.model.as_str(), OTHER_MODEL);
        assert_eq!(main.effort.as_deref(), Some("minimal"));
        // Mapped onto the model's own scale, whose weakest level is `low`.
        assert_eq!(main.effort_in_effect.as_deref(), Some("low"));
        assert_eq!(
            warned(&front),
            1,
            "the change re-sends the context uncached"
        );

        front.turn("two").await;
        let request = &world.provider.requests()[1];
        assert_eq!(request.model.info.id.as_str(), OTHER_MODEL);
        assert_eq!(request.reasoning.level.as_deref(), Some("low"));
        // The conversation went with it, and the first model's message
        // still says who wrote it: what is replayed of its reasoning is the
        // provider layer's rule.
        assert_eq!(request.messages.len(), 3);
        assert_eq!(
            request.messages[1]
                .origin
                .as_ref()
                .map(|origin| origin.model.as_str()),
            Some(MODEL)
        );

        // The level alone: the model stays.
        let state = set(
            &front,
            role(RoleSettings {
                effort: Some("max".to_owned()),
                ..RoleSettings::default()
            }),
        )
        .await
        .unwrap();
        let main = &state.roles[&ModelRole::Main];
        assert_eq!(main.model.as_str(), OTHER_MODEL);
        assert_eq!(main.effort_in_effect.as_deref(), Some("max"));
        front.turn("three").await;
        assert_eq!(
            world.provider.requests()[2].reasoning.level.as_deref(),
            Some("max")
        );
        front.close().await;
    });
}

#[test]
fn a_model_or_a_level_that_does_not_exist_is_refused_and_changes_nothing() {
    let world = World::new("model_bad", [says("Still the first.")]);
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        let refused = set(
            &front,
            role(RoleSettings {
                model: Some(ModelId::new("no-such-model")),
                ..RoleSettings::default()
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.code, "unknown_model");
        let refused = set(
            &front,
            role(RoleSettings {
                effort: Some("ludicrous".to_owned()),
                ..RoleSettings::default()
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.code, "unknown_effort");
        assert!(refused.message.contains("xhigh"), "{}", refused.message);
        front.turn("one").await;
        let request = &world.provider.requests()[0];
        assert_eq!(request.model.info.id.as_str(), MODEL);
        assert_eq!(request.reasoning.level, None);
        front.close().await;
    });
}

#[test]
fn the_level_can_be_lowered_by_whoever_drives_and_raised_only_by_a_user() {
    let world = World::new(
        "level_set",
        [
            calls(&[("c1", "write", json!({"path": "a.txt", "content": "x\n"}))]),
            says("Refused."),
            calls(&[("c2", "write", json!({"path": "a.txt", "content": "x\n"}))]),
            says("Written."),
        ],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        let supervisor = front
            .join(ClientInfo {
                role: ClientRole::Supervisor,
                ..ClientInfo::user()
            })
            .await;
        let level = |level: Level| SessionSettings {
            level: Some(level),
            ..SessionSettings::default()
        };
        // A supervisor lowers it.
        let state = set(&supervisor, level(Level::ReadOnly)).await.unwrap();
        assert_eq!(state.level, Level::ReadOnly);
        // The engine is at the new level: an edit is refused outright.
        let end = front.turn("write it").await;
        assert_eq!(end.outcome, TurnOutcome::Completed);
        assert_eq!(front.results()[0].1, "denied");
        assert_eq!(world.read("a.txt"), None);

        // It may not raise it again.
        let refused = set(&supervisor, level(Level::Supervised))
            .await
            .unwrap_err();
        assert_eq!(refused.code, "role_not_allowed");
        assert_eq!(refused.kind, cortex_protocol::ErrorKind::Permission);
        // A user may, and the edit then goes through.
        let state = set(&front, level(Level::Supervised)).await.unwrap();
        assert_eq!(state.level, Level::Supervised);
        assert_ne!(state.lease_id, front.state.lease_id, "a new standing lease");
        front.turn("write it now").await;
        assert_eq!(world.read("a.txt").as_deref(), Some("x\n"));

        // A level that cannot be held here is refused, and nothing changes:
        // there is no sandbox to confine an autonomous session.
        let refused = set(&front, level(Level::Autonomous)).await.unwrap_err();
        assert_eq!(refused.code, "sandbox_required");
        let refused = set(&front, level(Level::from_wire("godlike")))
            .await
            .unwrap_err();
        assert_eq!(refused.code, "unknown_value");

        // Each standing lease is in the log: granted, ended, granted, …
        let leases: Vec<(String, String)> = front
            .log(&world)
            .iter()
            .filter(|line| line["kind"] == "lease")
            .map(|line| {
                (
                    line["event"].as_str().unwrap().to_owned(),
                    line["lease"]["grant"]["level"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        let expected: Vec<(String, String)> = [
            ("granted", "supervised"),
            ("ended", "supervised"),
            ("granted", "read-only"),
            ("ended", "read-only"),
            ("granted", "supervised"),
        ]
        .iter()
        .map(|(event, level)| ((*event).to_owned(), (*level).to_owned()))
        .collect();
        assert_eq!(leases, expected);
        front.close().await;
    });
}
