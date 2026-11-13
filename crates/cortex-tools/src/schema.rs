//! Checking a tool call's arguments against the tool's JSON Schema.
//!
//! Arguments from a model are untrusted. They are validated before a tool
//! sees them, and a mismatch becomes an error result that tells the model
//! what to change (`docs/architecture/03-agent-loop.md`, "Streaming").
//!
//! This is a validator for the part of JSON Schema that tool definitions
//! use, written here because a full implementation is a large dependency
//! for what is needed. The keywords it knows:
//!
//! `type`, `properties`, `required`, `additionalProperties`, `items`,
//! `enum`, `const`, `minimum`, `maximum`, `exclusiveMinimum`,
//! `exclusiveMaximum`, `minLength`, `maxLength`, `minItems`, `maxItems`,
//! `anyOf`, `oneOf`, `allOf`, `not`, and `$ref` to a place inside the same
//! schema.
//!
//! **A keyword it does not know is ignored.** The schemas of MCP and
//! extension tools are written by others, and refusing a valid call because
//! of a keyword this file has not heard of would make the tool unusable; the
//! tool checks its own arguments in any case. What is known is enforced.

use std::fmt::Write as _;

use crate::contract::Json;

/// How deep a schema may send the validator, through nesting and through
/// `$ref`. A schema that refers to itself without end stops here, and what
/// lies deeper passes unchecked.
const MAX_DEPTH: usize = 48;

/// How many problems one report lists.
const MAX_PROBLEMS: usize = 6;

/// What is wrong with a value, as a list of problems. `Display` writes them
/// one per line, each naming the place in the arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaMismatch {
    problems: Vec<String>,
}

impl SchemaMismatch {
    /// The problems, in the order they were found.
    #[must_use]
    pub fn problems(&self) -> &[String] {
        &self.problems
    }
}

impl std::fmt::Display for SchemaMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, problem) in self.problems.iter().enumerate() {
            if index > 0 {
                f.write_str("\n")?;
            }
            f.write_str(problem)?;
        }
        Ok(())
    }
}

impl std::error::Error for SchemaMismatch {}

/// Checks `value` against `schema`.
pub fn validate(schema: &Json, value: &Json) -> Result<(), SchemaMismatch> {
    let mut check = Check {
        root: schema,
        problems: Vec::new(),
    };
    check.value(schema, value, &mut Place::default(), 0);
    if check.problems.is_empty() {
        Ok(())
    } else {
        Err(SchemaMismatch {
            problems: check.problems,
        })
    }
}

/// Where in the arguments the validator is: `edits[0].old`.
#[derive(Default)]
struct Place(String);

impl Place {
    fn name(&self) -> String {
        if self.0.is_empty() {
            "the arguments".to_owned()
        } else {
            format!("`{}`", self.0)
        }
    }

    fn with<T>(&mut self, step: Step<'_>, inside: impl FnOnce(&mut Self) -> T) -> T {
        let length = self.0.len();
        match step {
            Step::Key(key) => {
                if !self.0.is_empty() {
                    self.0.push('.');
                }
                self.0.push_str(key);
            }
            Step::Index(index) => {
                // Writing to a string cannot fail.
                let _ = write!(self.0, "[{index}]");
            }
        }
        let out = inside(self);
        self.0.truncate(length);
        out
    }
}

enum Step<'a> {
    Key(&'a str),
    Index(usize),
}

struct Check<'a> {
    root: &'a Json,
    problems: Vec<String>,
}

impl Check<'_> {
    fn problem(&mut self, text: String) {
        if self.problems.len() < MAX_PROBLEMS {
            self.problems.push(text);
        }
    }

    /// Whether `value` passes `schema`, without recording why not: for the
    /// branches of `anyOf`, `oneOf` and `not`.
    fn passes(&self, schema: &Json, value: &Json, depth: usize) -> bool {
        let mut probe = Check {
            root: self.root,
            problems: Vec::new(),
        };
        probe.value(schema, value, &mut Place::default(), depth);
        probe.problems.is_empty()
    }

    fn value(&mut self, schema: &Json, value: &Json, place: &mut Place, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        let schema = match schema {
            Json::Bool(true) => return,
            Json::Bool(false) => {
                self.problem(format!("{} is not allowed", place.name()));
                return;
            }
            Json::Object(schema) => schema,
            // Not a schema at all: nothing to enforce.
            _ => return,
        };

        if let Some(Json::String(reference)) = schema.get("$ref")
            && let Some(target) = resolve(self.root, reference)
        {
            self.value(target, value, place, depth + 1);
        }

        if let Some(expected) = schema.get("type") {
            let allowed: Vec<&str> = match expected {
                Json::String(name) => vec![name.as_str()],
                Json::Array(names) => names.iter().filter_map(Json::as_str).collect(),
                _ => Vec::new(),
            };
            if !allowed.is_empty() && !allowed.iter().any(|name| is_type(value, name)) {
                self.problem(format!(
                    "{} must be {}, and is {}",
                    place.name(),
                    one_of(&allowed),
                    describe(value)
                ));
                // The other keywords assume the type, and would only repeat
                // the complaint.
                return;
            }
        }

        if let Some(Json::Array(options)) = schema.get("enum")
            && !options.contains(value)
        {
            let listed: Vec<String> = options.iter().map(Json::to_string).collect();
            self.problem(format!(
                "{} must be one of {}, and is {}",
                place.name(),
                listed.join(", "),
                describe(value)
            ));
        }
        if let Some(constant) = schema.get("const")
            && constant != value
        {
            self.problem(format!(
                "{} must be {constant}, and is {}",
                place.name(),
                describe(value)
            ));
        }

        match value {
            Json::Number(number) => self.number(schema, number, place),
            Json::String(text) => self.string(schema, text, place),
            Json::Array(items) => self.array(schema, items, place, depth),
            Json::Object(fields) => self.object(schema, fields, place, depth),
            Json::Null | Json::Bool(_) => {}
        }

        if let Some(Json::Array(all)) = schema.get("allOf") {
            for branch in all {
                self.value(branch, value, place, depth + 1);
            }
        }
        if let Some(Json::Array(any)) = schema.get("anyOf")
            && !any.is_empty()
            && !any
                .iter()
                .any(|branch| self.passes(branch, value, depth + 1))
        {
            self.problem(format!(
                "{} fits none of the forms its schema allows",
                place.name()
            ));
        }
        if let Some(Json::Array(one)) = schema.get("oneOf") {
            let fitting = one
                .iter()
                .filter(|branch| self.passes(branch, value, depth + 1))
                .count();
            if !one.is_empty() && fitting != 1 {
                self.problem(format!(
                    "{} must fit exactly one of the forms its schema allows, and fits {fitting}",
                    place.name()
                ));
            }
        }
        if let Some(not) = schema.get("not")
            && self.passes(not, value, depth + 1)
        {
            self.problem(format!("{} has a form its schema rules out", place.name()));
        }
    }

    fn number(
        &mut self,
        schema: &serde_json::Map<String, Json>,
        number: &serde_json::Number,
        place: &Place,
    ) {
        let Some(value) = number.as_f64() else {
            return;
        };
        let bound = |keyword: &str| schema.get(keyword).and_then(Json::as_f64);
        if let Some(minimum) = bound("minimum")
            && value < minimum
        {
            self.problem(format!(
                "{} must be {} or more, and is {number}",
                place.name(),
                plain(minimum)
            ));
        }
        if let Some(maximum) = bound("maximum")
            && value > maximum
        {
            self.problem(format!(
                "{} must be {} or less, and is {number}",
                place.name(),
                plain(maximum)
            ));
        }
        if let Some(minimum) = bound("exclusiveMinimum")
            && value <= minimum
        {
            self.problem(format!(
                "{} must be more than {}, and is {number}",
                place.name(),
                plain(minimum)
            ));
        }
        if let Some(maximum) = bound("exclusiveMaximum")
            && value >= maximum
        {
            self.problem(format!(
                "{} must be less than {}, and is {number}",
                place.name(),
                plain(maximum)
            ));
        }
    }

    fn string(&mut self, schema: &serde_json::Map<String, Json>, text: &str, place: &Place) {
        let length = text.chars().count() as u64;
        if let Some(minimum) = schema.get("minLength").and_then(Json::as_u64)
            && length < minimum
        {
            self.problem(if minimum == 1 {
                format!("{} must not be empty", place.name())
            } else {
                format!(
                    "{} must have at least {minimum} characters, and has {length}",
                    place.name()
                )
            });
        }
        if let Some(maximum) = schema.get("maxLength").and_then(Json::as_u64)
            && length > maximum
        {
            self.problem(format!(
                "{} must have at most {maximum} characters, and has {length}",
                place.name()
            ));
        }
    }

    fn array(
        &mut self,
        schema: &serde_json::Map<String, Json>,
        items: &[Json],
        place: &mut Place,
        depth: usize,
    ) {
        let length = items.len() as u64;
        if let Some(minimum) = schema.get("minItems").and_then(Json::as_u64)
            && length < minimum
        {
            self.problem(format!(
                "{} must have at least {minimum} items, and has {length}",
                place.name()
            ));
        }
        if let Some(maximum) = schema.get("maxItems").and_then(Json::as_u64)
            && length > maximum
        {
            self.problem(format!(
                "{} must have at most {maximum} items, and has {length}",
                place.name()
            ));
        }
        if let Some(item_schema) = schema.get("items") {
            for (index, item) in items.iter().enumerate() {
                place.with(Step::Index(index), |place| {
                    self.value(item_schema, item, place, depth + 1);
                });
            }
        }
    }

    fn object(
        &mut self,
        schema: &serde_json::Map<String, Json>,
        fields: &serde_json::Map<String, Json>,
        place: &mut Place,
        depth: usize,
    ) {
        let properties = schema.get("properties").and_then(Json::as_object);
        if let Some(Json::Array(required)) = schema.get("required") {
            for name in required.iter().filter_map(Json::as_str) {
                if !fields.contains_key(name) {
                    let missing = place.with(Step::Key(name), |place| place.name());
                    self.problem(format!("{missing} is required"));
                }
            }
        }
        for (name, field) in fields {
            if let Some(property) = properties.and_then(|properties| properties.get(name)) {
                place.with(Step::Key(name), |place| {
                    self.value(property, field, place, depth + 1);
                });
                continue;
            }
            match schema.get("additionalProperties") {
                Some(Json::Bool(false)) => {
                    let unknown = place.with(Step::Key(name), |place| place.name());
                    let known: Vec<&str> = properties
                        .map(|properties| properties.keys().map(String::as_str).collect())
                        .unwrap_or_default();
                    self.problem(if known.is_empty() {
                        format!("{unknown} is not an argument this takes")
                    } else {
                        format!(
                            "{unknown} is not an argument this takes: the arguments are {}",
                            known
                                .iter()
                                .map(|name| format!("`{name}`"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    });
                }
                Some(extra @ Json::Object(_)) => {
                    place.with(Step::Key(name), |place| {
                        self.value(extra, field, place, depth + 1);
                    });
                }
                _ => {}
            }
        }
    }
}

/// Follows a `$ref` that points inside the same schema: `#`, or
/// `#/$defs/name`. Anything else (another document, an anchor) is not
/// followed.
fn resolve<'a>(root: &'a Json, reference: &str) -> Option<&'a Json> {
    let pointer = reference.strip_prefix('#')?;
    if pointer.is_empty() {
        return Some(root);
    }
    root.pointer(pointer)
}

fn is_type(value: &Json, name: &str) -> bool {
    match name {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "number" => value.is_number(),
        // `3.0` is an integer to JSON Schema.
        "integer" => match value {
            Json::Number(number) => {
                number.is_i64()
                    || number.is_u64()
                    || number.as_f64().is_some_and(|float| float.fract() == 0.0)
            }
            _ => false,
        },
        // A type name this does not know is not enforced.
        _ => true,
    }
}

/// `a string`, `an integer or null`.
fn one_of(names: &[&str]) -> String {
    let with_article = |name: &str| match name {
        "integer" | "array" | "object" => format!("an {name}"),
        "null" => "null".to_owned(),
        _ => format!("a {name}"),
    };
    match names {
        [] => String::new(),
        [only] => with_article(only),
        [rest @ .., last] => {
            let rest: Vec<String> = rest.iter().map(|name| with_article(name)).collect();
            format!("{} or {}", rest.join(", "), with_article(last))
        }
    }
}

/// What a value is, with the value itself when it is short.
fn describe(value: &Json) -> String {
    let kind = match value {
        Json::Null => return "null".to_owned(),
        Json::Bool(_) => "a boolean",
        Json::Number(_) => "a number",
        Json::String(_) => "a string",
        Json::Array(_) => return "an array".to_owned(),
        Json::Object(_) => return "an object".to_owned(),
    };
    let text = value.to_string();
    if text.chars().count() <= 40 {
        format!("{kind} ({text})")
    } else {
        kind.to_owned()
    }
}

/// A bound as it was written: `1`, not `1.0`.
fn plain(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        format!("{number:.0}")
    } else {
        number.to_string()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn problems(schema: &Json, value: &Json) -> Vec<String> {
        match validate(schema, value) {
            Ok(()) => Vec::new(),
            Err(mismatch) => mismatch.problems().to_vec(),
        }
    }

    fn read_schema() -> Json {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "minLength": 1},
                "offset": {"type": "integer", "minimum": 1},
                "limit": {"type": "integer", "minimum": 1},
                "fresh": {"type": "boolean"}
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    #[test]
    fn arguments_that_fit_pass() {
        let schema = read_schema();
        assert_eq!(validate(&schema, &json!({"path": "a.rs"})), Ok(()));
        assert_eq!(
            validate(
                &schema,
                &json!({"path": "a.rs", "offset": 10, "limit": 5, "fresh": true})
            ),
            Ok(())
        );
        // A float with no fraction is an integer.
        assert_eq!(
            validate(&schema, &json!({"path": "a.rs", "offset": 3.0})),
            Ok(())
        );
    }

    #[test]
    fn each_problem_names_the_argument_and_says_what_it_should_be() {
        let schema = read_schema();
        assert_eq!(problems(&schema, &json!({})), ["`path` is required"]);
        assert_eq!(
            problems(&schema, &json!({"path": 7})),
            ["`path` must be a string, and is a number (7)"]
        );
        assert_eq!(
            problems(&schema, &json!({"path": ""})),
            ["`path` must not be empty"]
        );
        assert_eq!(
            problems(&schema, &json!({"path": "a", "offset": 0})),
            ["`offset` must be 1 or more, and is 0"]
        );
        assert_eq!(
            problems(&schema, &json!({"path": "a", "offset": "ten"})),
            ["`offset` must be an integer, and is a string (\"ten\")"]
        );
        assert_eq!(
            problems(&schema, &json!({"path": "a", "offset": 1.5})),
            ["`offset` must be an integer, and is a number (1.5)"]
        );
        assert_eq!(
            problems(&schema, &json!({"path": "a", "lines": 3})),
            [
                "`lines` is not an argument this takes: the arguments are `fresh`, `limit`, \
                 `offset`, `path`"
            ]
        );
        assert_eq!(
            problems(&schema, &json!("a.rs")),
            ["the arguments must be an object, and is a string (\"a.rs\")"]
        );
    }

    #[test]
    fn several_problems_are_reported_together_up_to_a_limit() {
        let schema = read_schema();
        let found = problems(&schema, &json!({"offset": 0, "fresh": "yes"}));
        assert_eq!(found.len(), 3, "{found:?}");
        let mismatch = validate(&schema, &json!({"offset": 0, "fresh": "yes"})).unwrap_err();
        assert_eq!(mismatch.to_string().lines().count(), 3);

        let many = json!({"type": "array", "items": {"type": "string"}});
        let value = json!([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(problems(&many, &value).len(), MAX_PROBLEMS);
    }

    #[test]
    fn nested_values_are_named_by_their_place() {
        let schema = json!({
            "type": "object",
            "properties": {
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "old": {"type": "string"},
                            "count": {"type": "integer", "minimum": 1, "maximum": 100}
                        },
                        "required": ["old"]
                    }
                }
            }
        });
        assert_eq!(
            problems(&schema, &json!({"edits": [{"old": "x"}, {"count": 0}]})),
            [
                "`edits[1].old` is required",
                "`edits[1].count` must be 1 or more, and is 0"
            ]
        );
        assert_eq!(
            problems(&schema, &json!({"edits": []})),
            ["`edits` must have at least 1 items, and has 0"]
        );
        assert_eq!(
            problems(&schema, &json!({"edits": [{"old": "x", "count": 101}]})),
            ["`edits[0].count` must be 100 or less, and is 101"]
        );
    }

    #[test]
    fn enumerations_constants_and_lengths_are_enforced() {
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": {"enum": ["lines", "files", "count"]},
                "version": {"const": 2},
                "name": {"type": "string", "minLength": 2, "maxLength": 4},
                "tags": {"type": "array", "maxItems": 2},
                "ratio": {"type": "number", "exclusiveMinimum": 0, "exclusiveMaximum": 1}
            }
        });
        assert_eq!(
            problems(&schema, &json!({"mode": "words"})),
            ["`mode` must be one of \"lines\", \"files\", \"count\", and is a string (\"words\")"]
        );
        assert_eq!(
            problems(&schema, &json!({"version": 3})),
            ["`version` must be 2, and is a number (3)"]
        );
        assert_eq!(
            problems(&schema, &json!({"name": "a"})),
            ["`name` must have at least 2 characters, and has 1"]
        );
        assert_eq!(
            problems(&schema, &json!({"name": "abcdé"})),
            ["`name` must have at most 4 characters, and has 5"]
        );
        assert_eq!(
            problems(&schema, &json!({"tags": [1, 2, 3]})),
            ["`tags` must have at most 2 items, and has 3"]
        );
        assert_eq!(
            problems(&schema, &json!({"ratio": 0})),
            ["`ratio` must be more than 0, and is 0"]
        );
        assert_eq!(
            problems(&schema, &json!({"ratio": 1.0})),
            ["`ratio` must be less than 1, and is 1.0"]
        );
        assert_eq!(
            validate(&schema, &json!({"ratio": 0.5, "mode": "files"})),
            Ok(())
        );
    }

    #[test]
    fn several_types_and_combined_schemas_work() {
        let schema = json!({
            "type": "object",
            "properties": {
                "limit": {"type": ["integer", "null"]},
                "target": {"anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]},
                "one": {"oneOf": [{"type": "integer"}, {"type": "number", "minimum": 10}]},
                "all": {"allOf": [{"type": "integer"}, {"minimum": 5}]},
                "odd": {"not": {"type": "string"}}
            }
        });
        assert_eq!(
            validate(
                &schema,
                &json!({"limit": null, "target": ["a"], "one": 3, "all": 5, "odd": 1})
            ),
            Ok(())
        );
        assert_eq!(
            problems(&schema, &json!({"limit": "x"})),
            ["`limit` must be an integer or null, and is a string (\"x\")"]
        );
        assert_eq!(
            problems(&schema, &json!({"target": [1]})),
            ["`target` fits none of the forms its schema allows"]
        );
        assert_eq!(
            problems(&schema, &json!({"one": 12})),
            ["`one` must fit exactly one of the forms its schema allows, and fits 2"]
        );
        assert_eq!(
            problems(&schema, &json!({"all": 4})),
            ["`all` must be 5 or more, and is 4"]
        );
        assert_eq!(
            problems(&schema, &json!({"odd": "text"})),
            ["`odd` has a form its schema rules out"]
        );
    }

    #[test]
    fn references_inside_the_schema_are_followed() {
        let schema = json!({
            "type": "object",
            "properties": {
                "root": {"$ref": "#/$defs/node"}
            },
            "$defs": {
                "node": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "children": {"type": "array", "items": {"$ref": "#/$defs/node"}}
                    },
                    "required": ["name"]
                }
            }
        });
        assert_eq!(
            validate(
                &schema,
                &json!({"root": {"name": "a", "children": [{"name": "b"}]}})
            ),
            Ok(())
        );
        assert_eq!(
            problems(
                &schema,
                &json!({"root": {"name": "a", "children": [{"name": 1}]}})
            ),
            ["`root.children[0].name` must be a string, and is a number (1)"]
        );
    }

    #[test]
    fn a_schema_that_refers_to_itself_without_end_stops() {
        let schema = json!({"$ref": "#"});
        assert_eq!(validate(&schema, &json!({"anything": 1})), Ok(()));
        let twins = json!({
            "$ref": "#/$defs/a",
            "$defs": {"a": {"$ref": "#/$defs/b"}, "b": {"$ref": "#/$defs/a"}}
        });
        assert_eq!(validate(&twins, &json!(1)), Ok(()));
    }

    #[test]
    fn what_is_not_known_is_not_enforced() {
        let schema = json!({
            "type": "object",
            "properties": {
                "when": {"type": "string", "format": "date-time", "pattern": "^[0-9]"},
                "what": {"type": "money"},
                "other": {"$ref": "https://example.com/schema.json"}
            },
            "patternProperties": {"^x-": {"type": "string"}},
            "dependentRequired": {"when": ["what"]}
        });
        assert_eq!(
            validate(
                &schema,
                &json!({"when": "tomorrow", "what": 3, "other": [], "x-a": 1})
            ),
            Ok(())
        );
        // Boolean schemas, and things that are not schemas.
        assert_eq!(validate(&json!(true), &json!(1)), Ok(()));
        assert_eq!(
            problems(&json!(false), &json!(1)),
            ["the arguments is not allowed"]
        );
        assert_eq!(validate(&json!("nonsense"), &json!(1)), Ok(()));
        assert_eq!(
            validate(
                &json!({"additionalProperties": {"type": "integer"}}),
                &json!({"a": 1})
            ),
            Ok(())
        );
        assert_eq!(
            problems(
                &json!({"additionalProperties": {"type": "integer"}}),
                &json!({"a": "x"})
            ),
            ["`a` must be an integer, and is a string (\"x\")"]
        );
    }

    #[test]
    fn a_long_value_is_described_without_being_quoted() {
        let long = "x".repeat(100);
        assert_eq!(
            problems(&json!({"type": "integer"}), &json!(long)),
            ["the arguments must be an integer, and is a string"]
        );
        assert_eq!(
            problems(&json!({"type": "string"}), &json!([1])),
            ["the arguments must be a string, and is an array"]
        );
        assert_eq!(
            problems(&json!({"type": "string"}), &json!(null)),
            ["the arguments must be a string, and is null"]
        );
    }
}
