//! Tool schemas reduced to what a provider's strict mode takes
//! (`docs/architecture/04-providers.md`, "Tools").
//!
//! A tool's schema says more than a provider's strict mode will take: how
//! long a string may be, how many items a list may hold, the range of a
//! number. The harness checks every call's arguments against the whole
//! schema before the tool runs (`cortex-tools`), so nothing is lost by
//! leaving those keywords out of what is sent. What each one said is added
//! to the description of the value it was on, in words, so the model still
//! reads it.
//!
//! A schema that cannot be put in the subset without changing what it
//! means (an object whose properties are not all required, under rules
//! that require them all) is sent whole, and the tool without `strict`.
//!
//! **Anthropic also limits a request as a whole:** at most 20 strict tools,
//! 24 optional parameters and 16 parameters of more than one type, over
//! all its strict tools together. A [`Budget`] counts them in the order the
//! tools are sent, and a tool that would go over is sent without `strict`.
//! The order is the toolbox's, so the same tools are strict every time.
//!
//! **The result is the same bytes every time** for the same schema: the
//! keywords are looked at in a fixed order, the sentences are fixed, and
//! the objects are written in key order. The request's cached prefix holds
//! the tools, and a schema that changed from one request to the next would
//! cost the whole cache.

use std::borrow::Cow;

use serde_json::{Map, Value};

/// Whose strict mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rules {
    /// Anthropic's `strict` tools. Every object must close its properties
    /// (`additionalProperties: false`). It takes no numeric constraint, no
    /// length of a string, no item count but a `minItems` of 0 or 1, and
    /// no count of an object's entries.
    Anthropic,
    /// OpenAI's `strict` functions, on the `openai-chat` protocol. Every
    /// object must close its properties and require every one of them. It
    /// takes the numeric constraints and the item counts, and not the
    /// length of a string or the count of an object's entries.
    OpenAi,
}

/// The keywords a set of rules may leave out, in the order they are looked
/// at, with what each one says in words: the words before the number, and
/// the unit after it for one and for more.
const KEYWORDS: &[(&str, &str, &str, &str)] = &[
    ("minimum", "At least", "", ""),
    ("exclusiveMinimum", "More than", "", ""),
    ("maximum", "At most", "", ""),
    ("exclusiveMaximum", "Less than", "", ""),
    ("multipleOf", "A multiple of", "", ""),
    ("minLength", "At least", " character", " characters"),
    ("maxLength", "At most", " character", " characters"),
    ("minItems", "At least", " item", " items"),
    ("maxItems", "At most", " item", " items"),
    ("uniqueItems", "No item twice", "", ""),
    ("minProperties", "At least", " entry", " entries"),
    ("maxProperties", "At most", " entry", " entries"),
];

impl Rules {
    /// Whether these rules refuse `keyword` with this value.
    fn refuses(self, keyword: &str, value: &Value) -> bool {
        match self {
            Self::Anthropic => match keyword {
                "minItems" => value.as_u64().is_none_or(|count| count > 1),
                _ => true,
            },
            Self::OpenAi => matches!(
                keyword,
                "minLength" | "maxLength" | "uniqueItems" | "minProperties" | "maxProperties"
            ),
        }
    }

    /// Whether these rules require every property of an object.
    fn require_all(self) -> bool {
        self == Self::OpenAi
    }
}

/// What a request's strict tools may add up to, under rules that limit it.
#[derive(Debug, Clone, Copy)]
struct Limits {
    tools: usize,
    optional: usize,
    unions: usize,
}

impl Rules {
    /// The limits on a whole request: Anthropic's, as its documentation of
    /// strict tool use states them. OpenAI's are on one schema, and the
    /// built-in tools are far inside them.
    fn limits(self) -> Option<Limits> {
        match self {
            Self::Anthropic => Some(Limits {
                tools: 20,
                optional: 24,
                unions: 16,
            }),
            Self::OpenAi => None,
        }
    }
}

/// What one request's strict tools have used of its limits.
#[derive(Debug)]
pub(crate) struct Budget {
    rules: Rules,
    tools: usize,
    optional: usize,
    unions: usize,
}

impl Budget {
    /// Nothing used yet.
    pub(crate) fn new(rules: Rules) -> Self {
        Self {
            rules,
            tools: 0,
            optional: 0,
            unions: 0,
        }
    }

    /// The schema to send for the next tool, and whether to send it as
    /// strict: when it fits the rules, and the request's limits still have
    /// room for it.
    pub(crate) fn reduce<'a>(&mut self, schema: &'a Value) -> (Cow<'a, Value>, bool) {
        let (reduced, strict) = reduce(schema, self.rules);
        if !strict {
            return (reduced, false);
        }
        if let Some(limits) = self.rules.limits() {
            let (optional, unions) = count(schema);
            if self.tools + 1 > limits.tools
                || self.optional + optional > limits.optional
                || self.unions + unions > limits.unions
            {
                return (Cow::Borrowed(schema), false);
            }
            self.tools += 1;
            self.optional += optional;
            self.unions += unions;
        }
        (reduced, true)
    }
}

/// How many optional parameters, and how many parameters of more than one
/// type, a schema has, its nested objects included.
fn count(schema: &Value) -> (usize, usize) {
    let Value::Object(object) = schema else {
        return (0, 0);
    };
    let mut optional = 0;
    let mut unions = 0;
    if let Some(Value::Object(properties)) = object.get("properties") {
        let required: Vec<&str> = object
            .get("required")
            .and_then(Value::as_array)
            .map(|names| names.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        for (name, property) in properties {
            if !required.contains(&name.as_str()) {
                optional += 1;
            }
            let several = property.get("anyOf").is_some()
                || property.get("oneOf").is_some()
                || property.get("type").is_some_and(Value::is_array);
            if several {
                unions += 1;
            }
        }
    }
    for child in children(object) {
        let (more_optional, more_unions) = count(child);
        optional += more_optional;
        unions += more_unions;
    }
    (optional, unions)
}

/// The schema to send for a tool under `rules`, and whether it can be sent
/// as strict, the request's limits aside.
fn reduce(schema: &Value, rules: Rules) -> (Cow<'_, Value>, bool) {
    if !fits(schema, rules) {
        return (Cow::Borrowed(schema), false);
    }
    let mut reduced = schema.clone();
    walk(&mut reduced, rules);
    (Cow::Owned(reduced), true)
}

/// Whether every object of the schema is closed, and, where the rules ask
/// it, requires every property. An object is a schema of type `object`, or
/// one that lists properties. One that takes any key (no
/// `additionalProperties: false`) cannot be closed without changing what
/// it takes.
fn fits(schema: &Value, rules: Rules) -> bool {
    let Value::Object(object) = schema else {
        return true;
    };
    let typed_object = match object.get("type") {
        Some(Value::String(name)) => name == "object",
        Some(Value::Array(names)) => names.iter().any(|name| name == "object"),
        _ => false,
    };
    if (typed_object || object.contains_key("properties"))
        && object.get("additionalProperties") != Some(&Value::Bool(false))
    {
        return false;
    }
    if let Some(Value::Object(properties)) = object.get("properties")
        && rules.require_all()
    {
        {
            let required: Vec<&str> = object
                .get("required")
                .and_then(Value::as_array)
                .map(|names| names.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if properties
                .keys()
                .any(|name| !required.contains(&name.as_str()))
            {
                return false;
            }
        }
    }
    children(object).all(|child| fits(child, rules))
}

/// The schemas directly inside an object: its properties, its items, the
/// branches of its unions and its definitions.
fn children(object: &Map<String, Value>) -> impl Iterator<Item = &Value> {
    let maps = ["properties", "$defs", "definitions"]
        .into_iter()
        .filter_map(|key| object.get(key).and_then(Value::as_object))
        .flat_map(Map::values);
    let lists = ["anyOf", "oneOf", "allOf", "prefixItems"]
        .into_iter()
        .filter_map(|key| object.get(key).and_then(Value::as_array))
        .flatten();
    let single = ["items", "additionalProperties"]
        .into_iter()
        .filter_map(|key| object.get(key))
        .filter(|value| value.is_object());
    maps.chain(lists).chain(single)
}

fn walk(schema: &mut Value, rules: Rules) {
    let Value::Object(object) = schema else {
        return;
    };
    let mut said: Vec<String> = Vec::new();
    for (keyword, words, one, more) in KEYWORDS {
        let Some(value) = object.get(*keyword) else {
            continue;
        };
        if !rules.refuses(keyword, value) {
            continue;
        }
        if let Some(value) = object.remove(*keyword) {
            match value {
                Value::Bool(true) => said.push(format!("{words}.")),
                Value::Bool(false) => {}
                other => {
                    let unit = if other.as_u64() == Some(1) { one } else { more };
                    said.push(format!("{words} {other}{unit}."));
                }
            }
        }
    }
    if !said.is_empty() {
        let added = said.join(" ");
        let description = match object.get("description").and_then(Value::as_str) {
            Some(text) if !text.is_empty() => format!("{} {added}", text.trim_end()),
            _ => added,
        };
        object.insert("description".to_owned(), Value::String(description));
    }
    for key in ["properties", "$defs", "definitions"] {
        if let Some(Value::Object(map)) = object.get_mut(key) {
            for child in map.values_mut() {
                walk(child, rules);
            }
        }
    }
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(Value::Array(list)) = object.get_mut(key) {
            for child in list {
                walk(child, rules);
            }
        }
    }
    for key in ["items", "additionalProperties"] {
        if let Some(child) = object.get_mut(key)
            && child.is_object()
        {
            walk(child, rules);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "minLength": 1, "description": "The file."},
                "edits": {
                    "type": "array", "minItems": 1, "maxItems": 50,
                    "items": {
                        "type": "object",
                        "properties": {"old": {"type": "string", "maxLength": 9}},
                        "required": ["old"],
                        "additionalProperties": false
                    }
                },
                "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 600_000}
            },
            "required": ["path", "edits", "timeout_ms"],
            "additionalProperties": false
        })
    }

    #[test]
    fn anthropic_gets_no_constraint_and_the_words_for_each() {
        let original = schema();
        let (reduced, strict) = reduce(&original, Rules::Anthropic);
        assert!(strict);
        assert_eq!(
            *reduced,
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "The file. At least 1 character."},
                    "edits": {
                        "type": "array", "minItems": 1, "description": "At most 50 items.",
                        "items": {
                            "type": "object",
                            "properties": {"old": {"type": "string",
                                "description": "At most 9 characters."}},
                            "required": ["old"],
                            "additionalProperties": false
                        }
                    },
                    "timeout_ms": {"type": "integer", "description": "At least 1. At most 600000."}
                },
                "required": ["path", "edits", "timeout_ms"],
                "additionalProperties": false
            })
        );
        // The original is not touched: the tool validates against it.
        assert_eq!(original, schema());
    }

    #[test]
    fn openai_keeps_the_counts_and_the_ranges() {
        let original = schema();
        let (reduced, strict) = reduce(&original, Rules::OpenAi);
        assert!(strict);
        assert_eq!(reduced["properties"]["edits"]["maxItems"], 50);
        assert_eq!(reduced["properties"]["timeout_ms"]["maximum"], 600_000);
        assert!(reduced["properties"]["path"].get("minLength").is_none());
    }

    #[test]
    fn a_schema_the_rules_cannot_take_is_sent_whole_and_not_strict() {
        let optional = json!({
            "type": "object",
            "properties": {"command": {"type": "string"}, "cwd": {"type": "string"}},
            "required": ["command"],
            "additionalProperties": false
        });
        let (sent, strict) = reduce(&optional, Rules::OpenAi);
        assert!(!strict);
        assert!(matches!(sent, Cow::Borrowed(_)));
        // Anthropic does not require every property.
        assert!(reduce(&optional, Rules::Anthropic).1);
        // An object left open fits neither, with properties or without.
        let open = json!({"type": "object", "properties": {"a": {"type": "string"}}});
        assert!(!reduce(&open, Rules::Anthropic).1);
        let any = json!({"type": "object", "properties": {"a": {"type": "object"}},
            "required": ["a"], "additionalProperties": false});
        assert!(!reduce(&any, Rules::Anthropic).1);
        assert!(!reduce(&any, Rules::OpenAi).1);
    }

    #[test]
    fn anthropic_tools_stay_strict_while_the_request_has_room() {
        // Ten optional parameters each: two fit in 24, the third does not.
        let properties: Map<String, Value> = (0..10)
            .map(|index| (format!("p{index}"), json!({"type": "string"})))
            .collect();
        let tool = json!({"type": "object", "properties": properties,
            "additionalProperties": false});
        let mut budget = Budget::new(Rules::Anthropic);
        let strict: Vec<bool> = (0..4).map(|_| budget.reduce(&tool).1).collect();
        assert_eq!(strict, [true, true, false, false]);
        // A smaller tool after them still fits.
        let small = json!({"type": "object", "properties": {"a": {"type": "string"}},
            "required": ["a"], "additionalProperties": false});
        assert!(budget.reduce(&small).1);
        // Unions and the number of tools count too.
        let union = json!({"type": "object", "properties": {
            "a": {"type": ["string", "null"]}}, "required": ["a"],
            "additionalProperties": false});
        let mut budget = Budget::new(Rules::Anthropic);
        let strict = (0..20).filter(|_| budget.reduce(&union).1).count();
        assert_eq!(strict, 16);
        let mut budget = Budget::new(Rules::Anthropic);
        let strict = (0..25).filter(|_| budget.reduce(&small).1).count();
        assert_eq!(strict, 20);
        // OpenAI's limits are per schema: no budget.
        let mut budget = Budget::new(Rules::OpenAi);
        let closed = json!({"type": "object", "properties": properties.clone(),
            "required": properties.keys().cloned().collect::<Vec<_>>(),
            "additionalProperties": false});
        assert!((0..5).all(|_| budget.reduce(&closed).1));
    }

    #[test]
    fn the_same_schema_is_the_same_bytes_every_time() {
        let mut out = Vec::new();
        for _ in 0..3 {
            let original = schema();
            let (reduced, _) = reduce(&original, Rules::Anthropic);
            let mut text = String::new();
            crate::json::value(&mut text, &reduced);
            out.push(text);
        }
        assert!(out.windows(2).all(|pair| pair[0] == pair[1]), "{out:?}");
    }
}
