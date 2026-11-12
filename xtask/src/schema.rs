//! `cargo xtask schema`: the protocol's JSON Schema and TypeScript
//! declarations, generated from the Rust types in `cortex-protocol` and
//! checked in under `docs/protocol/` (`docs/architecture/11-rpc-protocol.md`,
//! "Schema and generated types").
//!
//! The schema comes from the types. The TypeScript declarations come from
//! the schema, by the small translator in this file, so there is one source
//! and the two files cannot disagree. The translator knows exactly the
//! schema keywords the protocol uses and fails on any other: a new shape
//! must be translated on purpose, never guessed.

use std::fmt::Write as _;
use std::fs;

use cortex_protocol::schema::{METHODS_KEY, OPEN_KEY, bundle};
use serde_json::{Map, Value};

use crate::Result;
use crate::workspace;

/// The generated schema, relative to the repository root.
pub(crate) const SCHEMA_FILE: &str = "docs/protocol/schema/cortex-protocol.schema.json";
/// The generated declarations, relative to the repository root.
pub(crate) const TYPESCRIPT_FILE: &str = "docs/protocol/ts/cortex-protocol.d.ts";

/// The two files, as they should be.
pub(crate) struct Generated {
    pub(crate) schema: String,
    pub(crate) typescript: String,
}

/// Generates both files in memory.
pub(crate) fn generate() -> Result<Generated> {
    let document = bundle();
    let mut schema = serde_json::to_string_pretty(&document)
        .map_err(|error| format!("could not write the schema as JSON: {error}"))?;
    schema.push('\n');
    let typescript = typescript(&document)?;
    Ok(Generated { schema, typescript })
}

/// `cargo xtask schema [--check]`
pub(crate) fn run(arguments: &[String]) -> Result<()> {
    match arguments {
        [] => write(),
        [flag] if flag == "--check" => check(),
        _ => Err("usage: cargo xtask schema [--check]".to_owned()),
    }
}

/// Writes both files.
pub(crate) fn write() -> Result<()> {
    let generated = generate()?;
    let root = workspace::root();
    for (file, content) in [
        (SCHEMA_FILE, &generated.schema),
        (TYPESCRIPT_FILE, &generated.typescript),
    ] {
        let path = root.join(file);
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)
                .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
        }
        let unchanged = fs::read_to_string(&path).is_ok_and(|current| current == *content);
        if unchanged {
            println!("unchanged  {file}");
        } else {
            fs::write(&path, content)
                .map_err(|error| format!("could not write {file}: {error}"))?;
            println!("wrote      {file}");
        }
    }
    Ok(())
}

/// Fails when a checked-in file differs from what the types generate.
pub(crate) fn check() -> Result<()> {
    let generated = generate()?;
    let root = workspace::root();
    let mut stale = Vec::new();
    for (file, content) in [
        (SCHEMA_FILE, &generated.schema),
        (TYPESCRIPT_FILE, &generated.typescript),
    ] {
        let current = fs::read_to_string(root.join(file)).unwrap_or_default();
        // A checkout on Windows may have CRLF line endings despite
        // .gitattributes (a git older than the file, or an archive).
        if current.replace("\r\n", "\n") != *content {
            stale.push(file);
        }
    }
    if stale.is_empty() {
        println!("The generated protocol files are current.");
        Ok(())
    } else {
        Err(format!(
            "stale: {}. The types in crates/cortex-protocol changed and the generated files \
             did not.\n    Run `cargo xtask schema` and commit the result. The diff is the \
             protocol change, and belongs in the review.",
            stale.join(", ")
        ))
    }
}

// ------------------------------------------------------------- TypeScript

/// Keywords that say nothing a TypeScript type can express, or that the
/// translator reads at the place it handles their neighbours.
const IGNORED: [&str; 12] = [
    "$schema",
    "default",
    "description",
    "format",
    "maximum",
    "minimum",
    "not",
    "propertyNames",
    "required",
    "title",
    "uniqueItems",
    OPEN_KEY,
];

fn typescript(document: &Value) -> Result<String> {
    let definitions = document
        .get("$defs")
        .and_then(Value::as_object)
        .ok_or("the schema has no $defs")?;
    let version = document
        .get("x-cortex-protocol-version")
        .and_then(Value::as_u64)
        .ok_or("the schema has no protocol version")?;

    let mut out = String::new();
    out.push_str(
        "// The Cortex-Harness protocol, as TypeScript declarations.\n\
         //\n\
         // Generated from the Rust types in crates/cortex-protocol by `cargo xtask schema`.\n\
         // Do not edit: change the types and run the command again.\n\
         //\n\
         // Every enumeration and every tagged union is open. A string enumeration\n\
         // lists the values this version knows and also accepts any other string.\n\
         // A tagged union lists the variants this version knows; a receiver must\n\
         // also tolerate an object whose `type` or `kind` is not listed, and skip\n\
         // it or show it generically. Objects may carry fields that are not listed.\n\n",
    );
    let _ = writeln!(
        out,
        "/** The protocol version these declarations describe. */\nexport type ProtocolVersion = {version};\n"
    );

    let methods = document
        .get(METHODS_KEY)
        .and_then(Value::as_object)
        .ok_or("the schema has no method table")?;
    for (key, name, doc) in [
        (
            "client",
            "ClientMethods",
            "Requests a client sends to the server, by method name.",
        ),
        (
            "server",
            "ServerMethods",
            "Requests the server sends to a client, by method name. Each pauses the turn until it is answered or times out.",
        ),
    ] {
        let table = methods
            .get(key)
            .and_then(Value::as_object)
            .ok_or_else(|| format!("the method table has no `{key}`"))?;
        let _ = writeln!(out, "{}export interface {name} {{", comment(doc, ""));
        for (method, entry) in table {
            let description = entry
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            let params = type_of(entry.get("params").unwrap_or(&Value::Bool(true)), method)?;
            let result = type_of(entry.get("result").unwrap_or(&Value::Bool(true)), method)?;
            let _ = writeln!(
                out,
                "{}  {}: {{ params: {params}; result: {result} }};",
                comment(description, "  "),
                property_name(method),
            );
        }
        out.push_str("}\n\n");
    }
    let notifications = methods
        .get("notifications")
        .and_then(Value::as_object)
        .ok_or("the method table has no `notifications`")?;
    out.push_str(&comment(
        "Notifications the server sends, by method name, with their parameters. A client ignores a method it does not know.",
        "",
    ));
    out.push_str("export interface ServerNotifications {\n");
    for (method, entry) in notifications {
        let description = entry
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        let params = type_of(entry.get("params").unwrap_or(&Value::Bool(true)), method)?;
        let _ = writeln!(
            out,
            "{}  {}: {params};",
            comment(description, "  "),
            property_name(method),
        );
    }
    out.push_str("}\n");

    for (name, schema) in definitions {
        out.push('\n');
        let description = schema
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        out.push_str(&comment(description, ""));
        match plain_object(schema) {
            Some(object) => {
                let _ = writeln!(
                    out,
                    "export interface {name} {}",
                    object_type(object, name, "")?
                );
            }
            None => {
                let _ = writeln!(out, "export type {name} = {};", type_of(schema, name)?);
            }
        }
    }
    Ok(out)
}

/// A JSDoc comment, or nothing for an empty description.
fn comment(text: &str, indent: &str) -> String {
    // Doc comments arrive with their line breaks. One line reads better
    // in an editor's hover, and cannot break the comment's indentation.
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return String::new();
    }
    format!("{indent}/** {} */\n", text.replace("*/", "*\\/"))
}

fn property_name(name: &str) -> String {
    let plain = name
        .chars()
        .enumerate()
        .all(|(index, c)| c == '_' || c.is_ascii_alphabetic() || (index > 0 && c.is_ascii_digit()));
    if plain && !name.is_empty() {
        name.to_owned()
    } else {
        Value::String(name.to_owned()).to_string()
    }
}

/// The schema as an object type with named properties and nothing else, if
/// that is what it is. Such a definition becomes an `interface`.
fn plain_object(schema: &Value) -> Option<&Map<String, Value>> {
    let object = schema.as_object()?;
    let is_object = object.get("type").and_then(Value::as_str) == Some("object");
    let combined = [
        "anyOf",
        "oneOf",
        "allOf",
        "$ref",
        "enum",
        "const",
        "additionalProperties",
    ]
    .iter()
    .any(|key| object.contains_key(*key));
    (is_object && !combined).then_some(object)
}

/// `{ name: type; … }` for an object schema with named properties.
fn object_type(object: &Map<String, Value>, at: &str, indent: &str) -> Result<String> {
    let empty = Map::new();
    let properties = object
        .get("properties")
        .map(|properties| {
            properties
                .as_object()
                .ok_or_else(|| format!("{at}: `properties` is not an object"))
        })
        .transpose()?
        .unwrap_or(&empty);
    let required: Vec<&str> = object
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if properties.is_empty() {
        // An object with no fields of its own: anything may be added later.
        return Ok("{\n".to_owned() + indent + "  [key: string]: unknown;\n" + indent + "}");
    }
    let mut out = String::from("{\n");
    for (name, property) in properties {
        let description = property
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        let inner = format!("{indent}  ");
        out.push_str(&comment(description, &inner));
        let optional = if required.contains(&name.as_str()) {
            ""
        } else {
            "?"
        };
        let _ = writeln!(
            out,
            "{inner}{}{optional}: {};",
            property_name(name),
            type_of(property, &format!("{at}.{name}"))?
        );
    }
    out.push_str(indent);
    out.push('}');
    Ok(out)
}

/// The TypeScript type of a schema. `at` names the place, for errors.
fn type_of(schema: &Value, at: &str) -> Result<String> {
    let object = match schema {
        Value::Bool(true) => return Ok("unknown".to_owned()),
        Value::Bool(false) => return Ok("never".to_owned()),
        Value::Object(object) => object,
        other => return Err(format!("{at}: a schema cannot be `{other}`")),
    };
    const KNOWN: [&str; 11] = [
        "$ref",
        "additionalProperties",
        "allOf",
        "anyOf",
        "const",
        "enum",
        "items",
        "oneOf",
        "properties",
        "type",
        "$defs",
    ];
    if let Some(strange) = object
        .keys()
        .find(|key| !KNOWN.contains(&key.as_str()) && !IGNORED.contains(&key.as_str()))
    {
        return Err(format!(
            "{at}: the schema keyword `{strange}` has no translation to TypeScript yet. Add one \
             to xtask/src/schema.rs."
        ));
    }

    if let Some(reference) = object.get("$ref") {
        let name = reference
            .as_str()
            .and_then(|reference| reference.strip_prefix("#/$defs/"))
            .ok_or_else(|| format!("{at}: `$ref` must point into #/$defs"))?;
        return Ok(name.to_owned());
    }
    if let Some(value) = object.get("const") {
        return Ok(value.to_string());
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        return Ok(values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(" | "));
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = object.get(key).and_then(Value::as_array) {
            let mut types = Vec::new();
            for (index, branch) in branches.iter().enumerate() {
                let open = branch.get(OPEN_KEY).and_then(Value::as_bool) == Some(true);
                let is_string = branch.get("type").and_then(Value::as_str) == Some("string");
                if open && is_string {
                    // Any other string, written so that editors still offer
                    // the known values.
                    types.push("(string & {})".to_owned());
                } else if !open {
                    // The open branch of a union is left out: see the note
                    // at the top of the generated file.
                    types.push(parenthesised(&type_of(
                        branch,
                        &format!("{at}.{key}[{index}]"),
                    )?));
                }
            }
            return Ok(types.join(" | "));
        }
    }
    if let Some(parts) = object.get("allOf").and_then(Value::as_array) {
        let mut types = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            types.push(parenthesised(&type_of(
                part,
                &format!("{at}.allOf[{index}]"),
            )?));
        }
        return Ok(types.join(" & "));
    }

    let names: Vec<&str> = match object.get("type") {
        Some(Value::String(name)) => vec![name.as_str()],
        Some(Value::Array(names)) => names.iter().filter_map(Value::as_str).collect(),
        Some(other) => return Err(format!("{at}: `type` cannot be `{other}`")),
        None => return Ok("unknown".to_owned()),
    };
    let mut types = Vec::new();
    for name in names {
        types.push(match name {
            "string" => "string".to_owned(),
            "integer" | "number" => "number".to_owned(),
            "boolean" => "boolean".to_owned(),
            "null" => "null".to_owned(),
            "array" => {
                let items = object.get("items").unwrap_or(&Value::Bool(true));
                format!("Array<{}>", type_of(items, &format!("{at}[]"))?)
            }
            "object" => match object.get("additionalProperties") {
                Some(Value::Bool(false)) | None => object_type(object, at, "  ")?,
                Some(values) => {
                    if object.contains_key("properties") {
                        return Err(format!(
                            "{at}: an object with both named and additional properties has no \
                             translation to TypeScript yet"
                        ));
                    }
                    format!(
                        "{{ [key: string]: {} }}",
                        type_of(values, &format!("{at}{{}}"))?
                    )
                }
            },
            other => return Err(format!("{at}: unknown type `{other}`")),
        });
    }
    Ok(types.join(" | "))
}

/// Wraps a union or an intersection so that it can sit inside another.
fn parenthesised(ty: &str) -> String {
    let mut depth = 0_i32;
    let mut compound = false;
    let mut previous = ' ';
    for c in ty.chars() {
        match c {
            '(' | '{' | '<' | '[' => depth += 1,
            ')' | '}' | '>' | ']' => depth -= 1,
            '|' | '&' if depth == 0 && previous == ' ' => compound = true,
            _ => {}
        }
        previous = c;
    }
    if compound {
        format!("({ty})")
    } else {
        ty.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;

    use cortex_protocol::schema::{METHODS_KEY, bundle};
    use serde_json::{Value, json};

    use super::{SCHEMA_FILE, TYPESCRIPT_FILE, generate, type_of, typescript};
    use crate::workspace;

    const TRANSCRIPT: &str = "crates/cortex-protocol/tests/golden/v1-session.jsonl";
    const VALUES: &str = "crates/cortex-protocol/tests/golden/v1-values.jsonl";

    thread_local! {
        /// The definitions some value has been valid against, on this thread.
        static SEEN: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
    }

    /// The done criterion of step 3: the test fails when a checked-in file
    /// is stale. `cargo xtask schema` puts it right.
    #[test]
    fn the_checked_in_protocol_files_are_current() {
        let generated = generate().unwrap();
        let root = workspace::root();
        for (file, content) in [
            (SCHEMA_FILE, &generated.schema),
            (TYPESCRIPT_FILE, &generated.typescript),
        ] {
            let current = fs::read_to_string(root.join(file))
                .unwrap_or_else(|error| panic!("{file}: {error}. Run `cargo xtask schema`."))
                .replace("\r\n", "\n");
            assert!(
                current == *content,
                "{file} is stale: the types in crates/cortex-protocol changed and the generated \
                 file did not. Run `cargo xtask schema` and commit the result."
            );
        }
    }

    #[test]
    fn a_changed_type_makes_the_generated_files_differ() {
        // What the freshness test compares is sensitive to a one-word change
        // in one description, so a stale file cannot pass by accident.
        let mut document = bundle();
        let before = serde_json::to_string_pretty(&document).unwrap();
        let before_ts = typescript(&document).unwrap();
        document["$defs"]["TurnStartParams"]["properties"]["delivery"]["description"] =
            json!("Changed.");
        assert_ne!(serde_json::to_string_pretty(&document).unwrap(), before);
        assert_ne!(typescript(&document).unwrap(), before_ts);
    }

    #[test]
    fn generation_is_deterministic() {
        let (first, second) = (generate().unwrap(), generate().unwrap());
        assert_eq!(first.schema, second.schema);
        assert_eq!(first.typescript, second.typescript);
    }

    #[test]
    fn the_translator_covers_the_shapes_the_protocol_uses() {
        let cases = [
            (json!(true), "unknown"),
            (json!({"$ref": "#/$defs/Item"}), "Item"),
            (json!({"const": "text"}), "\"text\""),
            (json!({"type": ["string", "null"]}), "string | null"),
            (
                json!({"type": "integer", "format": "uint64", "minimum": 0}),
                "number",
            ),
            (
                json!({"type": "array", "items": {"$ref": "#/$defs/Part"}}),
                "Array<Part>",
            ),
            (
                json!({"type": "object", "additionalProperties": {"$ref": "#/$defs/RoleState"}}),
                "{ [key: string]: RoleState }",
            ),
            (
                json!({"anyOf": [{"$ref": "#/$defs/Cost"}, {"type": "null"}]}),
                "Cost | null",
            ),
            (
                json!({"anyOf": [
                    {"const": "end"}, {"const": "error"},
                    {"type": "string", "x-cortex-open": true},
                ]}),
                "\"end\" | \"error\" | (string & {})",
            ),
            (
                json!({"allOf": [
                    {"type": "object", "properties": {"kind": {"const": "text"}}, "required": ["kind"]},
                    {"$ref": "#/$defs/TextPart"},
                ]}),
                "{\n    kind: \"text\";\n  } & TextPart",
            ),
        ];
        for (schema, expected) in cases {
            assert_eq!(type_of(&schema, "test").unwrap(), expected, "{schema}");
        }

        // The open branch of a tagged union is not a TypeScript variant.
        let union = json!({"anyOf": [
            {"allOf": [{"$ref": "#/$defs/A"}, {"$ref": "#/$defs/B"}]},
            {"type": "object", "properties": {"kind": {"type": "string"}}, "x-cortex-open": true},
        ]});
        assert_eq!(type_of(&union, "test").unwrap(), "(A & B)");

        // A keyword nobody translated is an error, not a silent `unknown`.
        let error =
            type_of(&json!({"type": "string", "pattern": "^a"}), "Thing.field").unwrap_err();
        assert!(error.contains("Thing.field"), "{error}");
        assert!(error.contains("`pattern`"), "{error}");
    }

    // ---- A validator for the subset of JSON Schema the protocol uses, so
    // that the hand-written golden transcript can be checked against the
    // generated schema. It is the proof that the schema describes the wire
    // form the Rust types really have, including the parts of the schema
    // that are written by hand (the open enumerations and unions).

    fn valid(schema: &Value, instance: &Value, root: &Value, at: &str) -> Result<(), String> {
        let object = match schema {
            Value::Bool(true) => return Ok(()),
            Value::Bool(false) => return Err(format!("{at}: nothing is allowed here")),
            Value::Object(object) => object,
            other => return Err(format!("{at}: bad schema {other}")),
        };
        if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
            let name = reference.strip_prefix("#/$defs/").ok_or("bad $ref")?;
            let target = root["$defs"]
                .get(name)
                .ok_or(format!("no definition {name}"))?;
            valid(target, instance, root, at)?;
            SEEN.with(|seen| seen.borrow_mut().insert(name.to_owned()));
        }
        if let Some(expected) = object.get("const")
            && expected != instance
        {
            return Err(format!("{at}: expected {expected}, found {instance}"));
        }
        if let Some(allowed) = object.get("enum").and_then(Value::as_array)
            && !allowed.contains(instance)
        {
            return Err(format!("{at}: {instance} is not one of {allowed:?}"));
        }
        if let Some(not) = object.get("not")
            && valid(not, instance, root, at).is_ok()
        {
            return Err(format!("{at}: {instance} is excluded here"));
        }
        if let Some(type_names) = object.get("type") {
            let names: Vec<&str> = match type_names {
                Value::String(name) => vec![name.as_str()],
                Value::Array(names) => names.iter().filter_map(Value::as_str).collect(),
                _ => return Err(format!("{at}: bad `type`")),
            };
            let fits = names.iter().any(|name| match *name {
                "string" => instance.is_string(),
                "integer" => instance.is_i64() || instance.is_u64(),
                "number" => instance.is_number(),
                "boolean" => instance.is_boolean(),
                "null" => instance.is_null(),
                "array" => instance.is_array(),
                "object" => instance.is_object(),
                _ => false,
            });
            if !fits {
                return Err(format!("{at}: {instance} is not of type {type_names}"));
            }
        }
        if let Some(minimum) = object.get("minimum").and_then(Value::as_f64)
            && instance.as_f64().is_some_and(|number| number < minimum)
        {
            return Err(format!("{at}: {instance} is below {minimum}"));
        }
        if let Some(fields) = instance.as_object() {
            for name in object
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let name = name.as_str().unwrap_or_default();
                if !fields.contains_key(name) {
                    return Err(format!("{at}: missing `{name}`"));
                }
            }
            let properties = object.get("properties").and_then(Value::as_object);
            for (name, value) in fields {
                match properties.and_then(|properties| properties.get(name)) {
                    Some(property) => valid(property, value, root, &format!("{at}.{name}"))?,
                    None => {
                        if let Some(additional) = object.get("additionalProperties") {
                            valid(additional, value, root, &format!("{at}.{name}"))?;
                        }
                    }
                }
            }
        }
        if let (Some(items), Some(elements)) = (object.get("items"), instance.as_array()) {
            for (index, element) in elements.iter().enumerate() {
                valid(items, element, root, &format!("{at}[{index}]"))?;
            }
        }
        for part in object
            .get("allOf")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            valid(part, instance, root, at)?;
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(branches) = object.get(key).and_then(Value::as_array) {
                let failures: Vec<String> = branches
                    .iter()
                    .filter_map(|branch| valid(branch, instance, root, at).err())
                    .collect();
                if failures.len() == branches.len() {
                    return Err(format!(
                        "{at}: no branch fits:\n      {}",
                        failures.join("\n      ")
                    ));
                }
            }
        }
        Ok(())
    }

    /// Checks one line of a transcript: the envelope against the root, and
    /// the contents against the type its method names.
    fn check_line(
        line: &Value,
        root: &Value,
        asked: &mut BTreeMap<String, (String, String)>,
    ) -> Result<(), String> {
        valid(root, line, root, "message")?;
        let methods = &root[METHODS_KEY];
        let id = line.get("id").map(Value::to_string);
        if let Some(method) = line.get("method").and_then(Value::as_str) {
            let params = line.get("params").unwrap_or(&Value::Null);
            match id {
                Some(id) => {
                    let side = if methods["client"].get(method).is_some() {
                        "client"
                    } else {
                        "server"
                    };
                    let entry = methods[side]
                        .get(method)
                        .ok_or(format!("unknown method {method}"))?;
                    asked.insert(id, (side.to_owned(), method.to_owned()));
                    valid(&entry["params"], params, root, &format!("{method} params"))
                }
                None => {
                    let entry = methods["notifications"]
                        .get(method)
                        .ok_or(format!("unknown notification {method}"))?;
                    valid(&entry["params"], params, root, method)
                }
            }
        } else if let Some(result) = line.get("result") {
            let (side, method) = asked
                .remove(&id.unwrap_or_default())
                .ok_or("a response with no request")?;
            valid(
                &methods[side.as_str()][method.as_str()]["result"],
                result,
                root,
                &format!("{method} result"),
            )
        } else {
            let data = &line["error"]["data"];
            valid(
                &json!({"$ref": "#/$defs/ErrorData"}),
                data,
                root,
                "error.data",
            )
        }
    }

    fn transcript() -> Vec<Value> {
        fs::read_to_string(workspace::root().join(TRANSCRIPT))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn the_golden_transcript_is_valid_against_the_generated_schema() {
        let root = bundle();
        let mut asked = BTreeMap::new();
        for line in transcript() {
            if let Err(error) = check_line(&line, &root, &mut asked) {
                panic!("{error}\n  in {line}");
            }
        }
    }

    /// "Round-trip tests for every type" is only true if every type is in a
    /// golden file. This is the check: a type added to the protocol and to
    /// no golden file fails here, by name.
    #[test]
    fn every_type_of_the_protocol_is_in_a_golden_file() {
        let root = bundle();
        let mut asked = BTreeMap::new();
        for line in transcript() {
            check_line(&line, &root, &mut asked)
                .unwrap_or_else(|error| panic!("{error}\n  in {line}"));
        }
        let values = fs::read_to_string(workspace::root().join(VALUES)).unwrap();
        for line in values.lines() {
            let entry: Value = serde_json::from_str(line).unwrap();
            let schema = json!({"$ref": format!("#/$defs/{}", entry["type"].as_str().unwrap())});
            valid(&schema, &entry["value"], &root, "value")
                .unwrap_or_else(|error| panic!("{error}\n  in {line}"));
        }

        let seen = SEEN.with(|seen| seen.borrow().clone());
        let missing: Vec<&String> = root["$defs"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|name| !seen.contains(*name))
            .collect();
        assert!(
            missing.is_empty(),
            "no golden file has a value of: {missing:?}. Add one to a file in \
             crates/cortex-protocol/tests/golden/ (a new file for an addition to a released \
             version), so that its wire form is pinned."
        );
    }

    #[test]
    fn no_two_types_share_a_name() {
        // The schema generator gives the second of two types with one name
        // a digit at the end. `Message2` in a client's code would be ours to
        // keep for ever.
        let root = bundle();
        for name in root["$defs"].as_object().unwrap().keys() {
            assert!(
                !name.ends_with(|c: char| c.is_ascii_digit()),
                "`{name}`: two protocol types have the same schema name"
            );
        }
    }

    #[test]
    fn the_schema_refuses_what_the_types_refuse() {
        let root = bundle();
        let event =
            |params: Value| json!({"jsonrpc": "2.0", "method": "session/event", "params": params});
        let refused = |line: Value| check_line(&line, &root, &mut BTreeMap::new()).unwrap_err();

        // A known event with a field missing does not pass as an unknown one.
        let error = refused(event(
            json!({"session_id": "s", "seq": 1, "type": "item.delta", "item_id": "i"}),
        ));
        assert!(error.contains("missing `delta`"), "{error}");
        // A wrong type deep inside.
        let error = refused(event(json!({
            "session_id": "s", "seq": 1, "type": "item.delta", "item_id": "i",
            "delta": {"kind": "text", "text": 5},
        })));
        assert!(error.contains("is not of type"), "{error}");
        // The envelope.
        let error = refused(event(json!({"seq": 1, "type": "session.idle"})));
        assert!(error.contains("missing `session_id`"), "{error}");
        let error = refused(
            json!({"jsonrpc": "2.0", "id": 1, "method": "turn/start", "params": {"input": []}}),
        );
        assert!(error.contains("missing `session_id`"), "{error}");

        // What the types accept, the schema accepts: an event type, an item
        // kind and an enumeration value added later.
        for later in [
            json!({"session_id": "s", "seq": 1, "type": "team.updated", "members": 3}),
            json!({"session_id": "s", "seq": 1, "type": "item.completed", "item": {"kind": "escalation", "id": "i"}}),
            json!({"session_id": "s", "seq": 1, "type": "item.started", "item_id": "i", "kind": "escalation"}),
            json!({"session_id": "s", "seq": 1, "type": "notice", "kind": "lease_expiring", "message": "Soon."}),
        ] {
            check_line(&event(later.clone()), &root, &mut BTreeMap::new())
                .unwrap_or_else(|error| panic!("{error}\n  in {later}"));
        }
    }
}
