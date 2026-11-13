//! The JSON Schema of the whole protocol, as one document. Built only with
//! the `schema` feature, for `cargo xtask schema` and the schema test.
//!
//! The document has every type under `$defs`, and a table of methods under
//! `x-cortex-methods` that says, for each method name, which types its
//! parameters and its result are. Its root accepts any JSON-RPC message.

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

use crate::methods::{MethodSchema, Op, PROTOCOL_VERSION, SESSION_EVENT, ServerRequest};

/// The key of the method table in the document.
pub const METHODS_KEY: &str = "x-cortex-methods";

/// The key that marks the "anything else" branch of an open enumeration or
/// an open union.
pub const OPEN_KEY: &str = "x-cortex-open";

fn method_table(methods: Vec<MethodSchema>) -> Value {
    let table: Map<String, Value> = methods
        .into_iter()
        .map(|method| {
            (
                method.name.to_owned(),
                json!({
                    "description": method.doc,
                    "params": method.params,
                    "result": method.result,
                }),
            )
        })
        .collect();
    Value::Object(table)
}

/// The schema document.
#[must_use]
pub fn bundle() -> Value {
    let mut generator = SchemaSettings::draft2020_12().into_generator();

    let message = generator.subschema_for::<crate::jsonrpc::Message>();
    let client = Op::schemas(&mut generator);
    let server = ServerRequest::schemas(&mut generator);
    let session_event = generator.subschema_for::<crate::SessionEvent>();
    // Types that no method reaches, and that are part of the contract all
    // the same: the session log and the provider layer use them.
    let _ = generator.subschema_for::<crate::message::Message>();
    let _ = generator.subschema_for::<crate::lease::Lease>();
    // What headless mode writes beside the events (step 13b).
    let _ = generator.subschema_for::<crate::headless::HeadlessRecord>();

    let definitions = generator.take_definitions(true);

    let mut document = Map::new();
    document.insert(
        "$schema".to_owned(),
        json!("https://json-schema.org/draft/2020-12/schema"),
    );
    document.insert("title".to_owned(), json!("Cortex-Harness protocol"));
    document.insert(
        "description".to_owned(),
        json!(
            "Generated from the Rust types in crates/cortex-protocol by `cargo xtask schema`. Do not edit. \
             The root accepts any JSON-RPC message; the typed contents of each method are in x-cortex-methods."
        ),
    );
    document.insert(
        "x-cortex-protocol-version".to_owned(),
        json!(PROTOCOL_VERSION),
    );
    document.insert(
        METHODS_KEY.to_owned(),
        json!({
            "client": method_table(client),
            "server": method_table(server),
            "notifications": {
                SESSION_EVENT: {
                    "description": "One event of a session. The only notification the server sends.",
                    "params": session_event,
                },
            },
        }),
    );
    if let Value::Object(message) = serde_json::to_value(&message).unwrap_or(Value::Null) {
        document.extend(message);
    }
    document.insert("$defs".to_owned(), Value::Object(definitions));
    Value::Object(document)
}
