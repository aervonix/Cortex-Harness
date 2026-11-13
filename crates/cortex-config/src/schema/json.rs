//! The JSON Schema of a configuration file, generated from the settings
//! (`docs/architecture/08-configuration.md`, "Validation"). Editors use it
//! to complete and check `config.toml`.
//!
//! It is a walk over the same schema tree that validation walks, so it
//! cannot describe a different file from the one the harness reads.

use cortex_protocol::lease::{Effect as RuleEffect, RuleAction};
use serde_json::{Map, Value as Json, json};

use super::{Child, Kind, Meta, Node, root};

/// The JSON Schema (draft 2020-12) of a configuration file, as text, ending
/// with a newline. The same input always gives the same bytes.
#[must_use]
pub fn json_schema() -> String {
    let mut schema = table(root(), false);
    if let Json::Object(object) = &mut schema {
        object.insert(
            "$schema".to_owned(),
            json!("https://json-schema.org/draft/2020-12/schema"),
        );
        object.insert("title".to_owned(), json!("Cortex-Harness configuration"));
        object.insert(
            "description".to_owned(),
            json!(
                "A Cortex-Harness configuration file: the user's `config.toml`, a project's \
                 `.cortex/config.toml` or `.cortex/config.local.toml`, or the managed file. \
                 Generated from the settings in `crates/cortex-config`; not edited by hand. \
                 `x-cortex-trust` says what a project's file may set, and `x-cortex-effect` \
                 when a change takes effect."
            ),
        );
    }
    let mut text = serde_json::to_string_pretty(&schema).unwrap_or_default();
    text.push('\n');
    text
}

fn table(node: &Node, enforced: bool) -> Json {
    let Node::Table(children) = node else {
        return json!({});
    };
    let properties: Map<String, Json> = children
        .iter()
        .map(|child| (child.name.to_owned(), entry(child, enforced)))
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        // An enforced table refuses a key it does not know. Elsewhere an
        // unknown key is a warning, which a schema cannot say, so it is
        // allowed.
        "additionalProperties": !enforced,
    })
}

fn entry(child: &Child, enforced: bool) -> Json {
    let mut schema = match child.meta {
        Meta::Leaf {
            trust,
            effect,
            range,
            joined,
        } => {
            let mut schema = match &child.node {
                Node::Leaf(kind) => kind_schema(kind),
                _ => json!({}),
            };
            if let Json::Object(object) = &mut schema {
                if let Some((low, high)) = range {
                    object.insert("minimum".to_owned(), number(low));
                    object.insert("maximum".to_owned(), number(high));
                }
                if let Some(default) = &child.default {
                    object.insert("default".to_owned(), from_toml(default));
                }
                object.insert("x-cortex-trust".to_owned(), json!(trust.describe()));
                object.insert("x-cortex-effect".to_owned(), json!(effect.as_str()));
                if joined {
                    object.insert("x-cortex-joined".to_owned(), json!(true));
                }
            }
            schema
        }
        Meta::Reserved { trust } => json!({
            "type": "object",
            "x-cortex-reserved": true,
            "x-cortex-trust": trust.describe(),
        }),
        Meta::Nested { enforced: here } => nested(&child.node, enforced || here),
    };
    if let Json::Object(object) = &mut schema {
        object.insert("description".to_owned(), json!(child.doc_text()));
    }
    schema
}

fn nested(node: &Node, enforced: bool) -> Json {
    match node {
        Node::Table(_) => table(node, enforced),
        Node::Map(inner) => json!({
            "type": "object",
            "additionalProperties": nested(inner, enforced),
        }),
        // A profile holds the keys of the file itself.
        Node::Profiles => json!({
            "type": "object",
            "additionalProperties": { "$ref": "#" },
        }),
        Node::Leaf(kind) => kind_schema(kind),
        Node::Open => json!({ "type": "object" }),
    }
}

fn kind_schema(kind: &Kind) -> Json {
    match kind {
        Kind::Bool => json!({ "type": "boolean" }),
        Kind::Integer => json!({ "type": "integer" }),
        Kind::Number => json!({ "type": "number" }),
        Kind::Text => json!({ "type": "string" }),
        Kind::Choice(words) => json!({ "enum": words }),
        Kind::Duration => json!({
            "type": "string",
            "pattern": "^([0-9]+(ms|s|m|h|d))+$",
        }),
        Kind::EnvText => json!({
            "anyOf": [
                { "type": "string" },
                {
                    "type": "object",
                    "properties": { "env": { "type": "string", "minLength": 1 } },
                    "required": ["env"],
                    "additionalProperties": false,
                },
            ],
        }),
        Kind::List(item) => json!({ "type": "array", "items": kind_schema(item) }),
        Kind::Map(item) => json!({
            "type": "object",
            "additionalProperties": kind_schema(item),
        }),
        Kind::Rule => json!({
            "type": "object",
            "properties": {
                "effect": { "enum": RuleEffect::KNOWN },
                "match": { "type": "string" },
                "action": { "enum": RuleAction::KNOWN },
            },
            "required": ["effect", "match", "action"],
            "additionalProperties": false,
        }),
    }
}

/// A whole number as a whole number, so that the schema says `20` and not
/// `20.0`.
fn number(value: f64) -> Json {
    if value.fract() == 0.0 && value.abs() < 9.0e15 {
        json!(value as i64)
    } else {
        json!(value)
    }
}

fn from_toml(value: &toml::Value) -> Json {
    match value {
        toml::Value::String(text) => json!(text),
        toml::Value::Integer(number) => json!(number),
        toml::Value::Float(number) => json!(number),
        toml::Value::Boolean(flag) => json!(flag),
        toml::Value::Datetime(time) => json!(time.to_string()),
        toml::Value::Array(items) => Json::Array(items.iter().map(from_toml).collect()),
        toml::Value::Table(table) => Json::Object(
            table
                .iter()
                .map(|(name, value)| (name.clone(), from_toml(value)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Json {
        serde_json::from_str(&json_schema()).unwrap()
    }

    #[test]
    fn the_schema_describes_a_setting_in_full() {
        let schema = schema();
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        let attempts = &schema["properties"]["retry"]["properties"]["attempts"];
        assert_eq!(attempts["type"], "integer");
        assert_eq!(attempts["default"], 5);
        assert_eq!(
            (&attempts["minimum"], &attempts["maximum"]),
            (&json!(1), &json!(20))
        );
        assert_eq!(attempts["x-cortex-trust"], "raises cost");
        assert_eq!(attempts["x-cortex-effect"], "at once");
        assert!(
            attempts["description"]
                .as_str()
                .unwrap()
                .contains("attempts")
        );

        let wait = &schema["properties"]["model"]["properties"]["speed_wait"];
        assert_eq!(wait["default"], "20s");
        assert!(wait["pattern"].is_string());
    }

    #[test]
    fn enforced_tables_take_no_unknown_key_and_the_others_do() {
        let schema = schema();
        let properties = &schema["properties"];
        for enforced in ["permissions", "sandbox", "limits"] {
            assert_eq!(
                properties[enforced]["additionalProperties"], false,
                "{enforced}"
            );
        }
        assert_eq!(
            properties["limits"]["properties"]["day"]["additionalProperties"], false,
            "and so do the tables inside one"
        );
        for tolerant in ["model", "retry", "log"] {
            assert_eq!(
                properties[tolerant]["additionalProperties"], true,
                "{tolerant}"
            );
        }
        assert_eq!(schema["additionalProperties"], true);
    }

    #[test]
    fn collections_rules_profiles_and_reserved_sections_have_their_shapes() {
        let schema = schema();
        let properties = &schema["properties"];
        let provider = &properties["providers"]["additionalProperties"];
        assert_eq!(provider["properties"]["base_url"]["type"], "string");
        assert!(provider["properties"]["headers"]["additionalProperties"]["anyOf"].is_array());

        let rules = &properties["permissions"]["properties"]["rules"];
        assert_eq!(rules["type"], "array");
        assert_eq!(rules["x-cortex-joined"], true);
        assert_eq!(rules["items"]["additionalProperties"], false);
        assert_eq!(
            rules["items"]["properties"]["action"]["enum"],
            json!(["allow", "ask", "deny"])
        );
        assert_eq!(
            properties["permissions"]["properties"]["mode"]["enum"],
            json!([
                "read-only",
                "supervised",
                "guided",
                "autonomous",
                "unrestricted"
            ])
        );

        assert_eq!(properties["profiles"]["additionalProperties"]["$ref"], "#");
        assert_eq!(properties["tui"]["x-cortex-reserved"], true);
        assert_eq!(properties["hooks"]["x-cortex-trust"], "runs code");
        assert_eq!(
            properties["model"]["properties"]["roles"]["additionalProperties"]["properties"]["speed"]
                ["type"],
            "string"
        );
    }

    #[test]
    fn the_same_schema_comes_out_every_time() {
        let first = json_schema();
        assert_eq!(first, json_schema());
        assert!(first.ends_with("}\n"));
        assert!(!first.contains('\r'));
    }
}
