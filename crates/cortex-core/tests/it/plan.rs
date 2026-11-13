//! The plan: the model's and the user's changes, what front ends are
//! told, and where the plan is kept between runs
//! (`docs/architecture/05-tools.md`, "`plan`").

use cortex_core::ClientInfo;
use cortex_protocol::event::{Actor, Event};
use cortex_protocol::ids::PlanItemId;
use cortex_protocol::item::PlanStatus;
use cortex_protocol::methods::{Op, OpResult, PlanUpdateParams, SessionRef, SessionRewindParams};
use cortex_protocol::project::{PlanAdd, PlanEdit, PlanItemRef};
use cortex_protocol::{Item, Plan};
use serde_json::{Value, json};

use crate::{Front, World, calls, says};

fn plan_call(id: &str, items: &[(&str, &str, &str)]) -> cortex_testkit::Response {
    let items: Vec<Value> = items
        .iter()
        .map(|(id, text, status)| json!({"id": id, "text": text, "status": status}))
        .collect();
    calls(&[(id, "plan", json!({ "items": items }))])
}

async fn get(front: &Front) -> Plan {
    let result = front
        .op(Op::PlanGet(SessionRef {
            session_id: front.session.clone(),
        }))
        .await
        .unwrap();
    let OpResult::PlanGet(result) = result else {
        panic!("not a plan");
    };
    result.plan
}

async fn update(front: &Front, edits: Vec<PlanEdit>) -> Plan {
    let result = front
        .op(Op::PlanUpdate(PlanUpdateParams {
            session_id: front.session.clone(),
            edits,
        }))
        .await
        .unwrap();
    let OpResult::PlanUpdate(result) = result else {
        panic!("not a plan");
    };
    result.plan
}

fn texts(plan: &Plan) -> Vec<(&str, &PlanStatus)> {
    plan.items
        .iter()
        .map(|item| (item.text.as_str(), &item.status))
        .collect()
}

fn updates(front: &Front) -> Vec<Actor> {
    front.drain();
    front
        .events()
        .into_iter()
        .filter_map(|event| match event.event {
            Event::PlanUpdated(updated) => Some(updated.by),
            _ => None,
        })
        .collect()
}

#[test]
fn the_models_plan_and_the_users_edits_reach_each_other_and_every_front_end() {
    let world = World::new(
        "plan",
        [
            plan_call(
                "c1",
                &[
                    ("a", "Read the code", "active"),
                    ("b", "Fix the bug", "pending"),
                ],
            ),
            says("Planned."),
            // After the user's edit: the model, which has been shown the
            // edit, writes the plan again.
            plan_call(
                "c2",
                &[
                    ("a", "Read the code", "done"),
                    ("b", "Fix the bug", "skipped"),
                    ("c", "Write a test first", "active"),
                ],
            ),
            says("Carrying on."),
        ],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("plan it").await;
        assert_eq!(updates(&front), [Actor::Agent]);
        let plan = get(&front).await;
        assert_eq!(
            texts(&plan),
            [
                ("Read the code", &PlanStatus::Active),
                ("Fix the bug", &PlanStatus::Pending)
            ]
        );

        // The user skips one and adds one, while the session is idle.
        let edited = update(
            &front,
            vec![
                PlanEdit::Skip(PlanItemRef {
                    id: PlanItemId::new("b"),
                }),
                PlanEdit::Add(PlanAdd {
                    text: "Write a test first".to_owned(),
                    after: None,
                }),
            ],
        )
        .await;
        assert_eq!(edited.items.len(), 3);
        assert_eq!(edited.items[1].status, PlanStatus::Skipped);
        assert_eq!(updates(&front), [Actor::Agent, Actor::User]);

        // The model is told at the start of the next turn, in a notice
        // ahead of the prompt, and its `plan` call then goes through.
        front.turn("carry on").await;
        let request = &world.provider.requests()[2];
        let said: String = request
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .filter_map(|part| match part {
                cortex_protocol::Part::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(said.contains("Write a test first"), "{said}");
        let replaced = front
            .items()
            .into_iter()
            .filter_map(|item| match item {
                Item::ToolResult(result) if result.tool == "plan" => result.payload,
                _ => None,
            })
            .map(|payload| payload["replaced"] == true)
            .collect::<Vec<_>>();
        assert_eq!(
            replaced,
            [true, true],
            "the notice was recorded, so the call went through"
        );
        assert_eq!(updates(&front), [Actor::Agent, Actor::User, Actor::Agent]);
        assert_eq!(get(&front).await.items[0].status, PlanStatus::Done);

        // A front end that only watches is told too.
        let observer = front.join(ClientInfo::observer()).await;
        update(
            &front,
            vec![PlanEdit::Remove(PlanItemRef {
                id: PlanItemId::new("a"),
            })],
        )
        .await;
        assert_eq!(updates(&observer), [Actor::User]);
        // And it cannot edit the plan itself.
        let refused = observer
            .op(Op::PlanUpdate(PlanUpdateParams {
                session_id: front.session.clone(),
                edits: Vec::new(),
            }))
            .await
            .unwrap_err();
        assert_eq!(refused.code, "role_not_allowed");
        front.close().await;
    });
    // Each change left the plan in the log.
    let plans = world
        .log()
        .lines()
        .filter(|line| line.contains("\"kind\":\"plan\""))
        .count();
    assert_eq!(plans, 4);
}

#[test]
fn a_resumed_session_has_its_plan_back_with_the_users_last_edit() {
    let world = World::new(
        "plan_resume",
        [
            plan_call("c1", &[("a", "Read the code", "active")]),
            says("Planned."),
        ],
    );
    let core = world.core();
    let (session, before) = world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("plan it").await;
        // An edit after the model's last call, which no tool result holds.
        update(
            &front,
            vec![PlanEdit::Add(PlanAdd {
                text: "Ask before deleting anything".to_owned(),
                after: None,
            })],
        )
        .await;
        let plan = get(&front).await;
        front.close().await;
        (front.session.clone(), plan)
    });
    assert_eq!(before.items.len(), 2);

    let core = world.core();
    world.run(async {
        let client = core.connect(ClientInfo::user());
        core.handle(
            client.id,
            Op::SessionOpen(SessionRef {
                session_id: session.clone(),
            }),
        )
        .await
        .unwrap();
        let result = core
            .handle(
                client.id,
                Op::PlanGet(SessionRef {
                    session_id: session.clone(),
                }),
            )
            .await
            .unwrap();
        let OpResult::PlanGet(result) = result else {
            panic!("not a plan");
        };
        assert_eq!(result.plan, before, "the plan as the session left it");
        core.shutdown().await;
    });
}

#[test]
fn a_rewind_goes_back_to_the_plan_of_that_point() {
    let world = World::new(
        "plan_rewind",
        [
            plan_call("c1", &[("a", "First plan", "active")]),
            says("Planned."),
            plan_call(
                "c2",
                &[("a", "Second plan", "active"), ("b", "More", "pending")],
            ),
            says("Planned again."),
        ],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.turn("plan it").await;
        let keep = front
            .items()
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::AssistantMessage(message) => Some(message.id.clone()),
                _ => None,
            })
            .unwrap();
        front.turn("plan it again").await;
        let later = get(&front).await;
        assert_eq!(later.items.len(), 2);

        front
            .op(Op::SessionRewind(SessionRewindParams {
                session_id: front.session.clone(),
                to: keep,
                restore_files: false,
            }))
            .await
            .unwrap();
        let plan = get(&front).await;
        assert_eq!(texts(&plan), [("First plan", &PlanStatus::Active)]);
        // A front end can tell the plan it held is older: the revision
        // never goes back.
        assert!(plan.revision > later.revision);
        assert_eq!(updates(&front).last(), Some(&Actor::Harness));
        front.close().await;
    });
}
