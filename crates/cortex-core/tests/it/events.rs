//! Events on their way to a front end: a slow client, and what an item in
//! progress holds (`docs/architecture/02-runtime.md`, "Event
//! backpressure"; `docs/architecture/11-rpc-protocol.md`, "Reconnecting
//! and several clients").

use cortex_core::{ClientInfo, Outgoing};
use cortex_protocol::event::{Delta, Event};
use cortex_protocol::methods::{Op, OpResult, SessionReadParams};
use cortex_protocol::{Item, ItemId, Part, StopReason};
use cortex_testkit::{Gate, Response};

use crate::{Front, World, settle};

async fn read(
    front: &Front,
    item: Option<ItemId>,
    limit: Option<u32>,
    cursor: Option<String>,
) -> cortex_protocol::methods::SessionReadResult {
    let result = front
        .op(Op::SessionRead(SessionReadParams {
            session_id: front.session.clone(),
            item_id: item,
            limit,
            cursor,
        }))
        .await
        .unwrap();
    let OpResult::SessionRead(result) = result else {
        panic!("not items");
    };
    result
}

fn text_of(item: &Item) -> String {
    match item {
        Item::AssistantMessage(message) => message
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

fn text_deltas(front: &Front) -> Vec<String> {
    front
        .events()
        .into_iter()
        .filter_map(|event| match event.event {
            Event::ItemDelta(delta) => match delta.delta {
                Delta::Text(text) => Some(text.text),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn an_item_in_progress_can_be_read_with_what_it_has_so_far() {
    let gate = Gate::new();
    let world = World::new(
        "in_progress",
        [Response::new()
            .text_in(&["The build ", "fails because"])
            .wait(&gate)
            .text(" of a missing file.")
            .end(StopReason::End)],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        front.prompt("why does the build fail?").await;
        front
            .until(|seen| {
                seen.iter().any(|outgoing| {
                    matches!(outgoing, Outgoing::Event(event)
                        if matches!(&event.event, Event::ItemDelta(delta)
                            if matches!(&delta.delta, Delta::Text(text) if text.text.contains("because"))))
                })
            })
            .await;

        // A client that connects now reads the session: the prompt, and
        // the answer as far as it has come, marked as not complete.
        let late = front.join(ClientInfo::user()).await;
        let result = read(&late, None, None, None).await;
        assert_eq!(result.items.len(), 2);
        assert!(matches!(result.items[0], Item::UserMessage(_)));
        assert_eq!(text_of(&result.items[1]), "The build fails because");
        let Item::AssistantMessage(partial) = &result.items[1] else {
            panic!("not an assistant message");
        };
        assert_eq!(partial.stop_reason, None);
        assert_eq!(result.incomplete, std::slice::from_ref(&partial.id));
        assert!(result.seq > 0);
        // That one item, by its id.
        let one = read(&late, Some(partial.id.clone()), None, None).await;
        assert_eq!(one.items.len(), 1);
        assert_eq!(one.incomplete.len(), 1);

        gate.open();
        front.turns(1).await;
        let result = read(&late, None, None, None).await;
        assert!(result.incomplete.is_empty());
        assert_eq!(
            text_of(&result.items[1]),
            "The build fails because of a missing file."
        );
        // Paged: one item at a time, with a cursor to the next.
        let first = read(&late, None, Some(1), None).await;
        assert_eq!(first.items.len(), 1);
        let second = read(&late, None, Some(1), first.next_cursor.clone()).await;
        assert_eq!(text_of(&second.items[0]), text_of(&result.items[1]));
        assert_eq!(second.next_cursor, None);
        front.close().await;
    });
}

#[test]
fn a_client_that_falls_behind_gets_fewer_deltas_one_resync_and_every_whole_event() {
    let gate = Gate::new();
    let pieces: Vec<String> = (0..200).map(|n| format!("word{n} ")).collect();
    let before: Vec<&str> = pieces[..100].iter().map(String::as_str).collect();
    let after: Vec<&str> = pieces[100..].iter().map(String::as_str).collect();
    let world = World::new(
        "slow_client",
        [Response::new()
            .text_in(&before)
            .wait(&gate)
            .text_in(&after)
            .end(StopReason::End)],
    );
    let core = world.core();
    world.run(async {
        let front = Front::start(&world, &core).await;
        // A second client with a short queue, which reads nothing for now.
        let slow = front
            .join(ClientInfo {
                queue: 4,
                ..ClientInfo::user()
            })
            .await;
        front.prompt("say a lot").await;
        // The client that keeps up sees the text arrive.
        front
            .until(|seen| {
                seen.iter().any(|outgoing| {
                    matches!(outgoing, Outgoing::Event(event)
                        if matches!(&event.event, Event::ItemDelta(delta)
                            if matches!(&delta.delta, Delta::Text(text) if text.text.contains("word99"))))
                })
            })
            .await;
        settle().await;
        // The slow one's queue did not grow with the deltas: a hundred
        // pieces arrived, and it holds a handful of events.
        assert!(slow.client.queue.len() <= 8, "{}", slow.client.queue.len());

        // It catches up: what it is owed is one resync for the item, and
        // the item's content so far is there to be read.
        slow.drain();
        let resyncs: Vec<ItemId> = slow
            .events()
            .into_iter()
            .filter_map(|event| match event.event {
                Event::ItemResync(resync) => Some(resync.item_id),
                _ => None,
            })
            .collect();
        assert_eq!(resyncs.len(), 1, "{:?}", slow.events());
        let partial = read(&slow, Some(resyncs[0].clone()), None, None).await;
        assert!(text_of(&partial.items[0]).ends_with("word99 "));
        assert_eq!(partial.incomplete, resyncs);

        gate.open();
        front.turns(1).await;
        slow.turns(1).await;
        // No delta of the item after its resync, and its completion whole.
        let late: Vec<String> = text_deltas(&slow)
            .into_iter()
            .filter(|text| text.contains("word150"))
            .collect();
        assert!(late.is_empty(), "deltas after the resync: {late:?}");
        let whole = pieces.concat();
        assert!(slow.items().iter().any(|item| text_of(item) == whole));
        // Nothing but deltas was lost: every other event the client that
        // kept up was sent, the slow one was sent too.
        let whole_events = |front: &Front| -> Vec<u64> {
            front
                .events()
                .iter()
                .filter(|event| {
                    !matches!(event.event, Event::ItemDelta(_) | Event::ItemResync(_))
                })
                .map(|event| event.seq)
                .collect()
        };
        let kept_up = whole_events(&front);
        let fell_behind = whole_events(&slow);
        let from = fell_behind.first().copied().unwrap_or(0);
        let expected: Vec<u64> = kept_up.into_iter().filter(|seq| *seq >= from).collect();
        assert_eq!(fell_behind, expected);

        // The client that kept up got all of the text in its deltas.
        assert_eq!(text_deltas(&front).concat(), whole);
        front.close().await;
        slow.close().await;
    });
}
