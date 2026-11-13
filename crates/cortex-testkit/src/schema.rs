//! A validator for the part of JSON Schema that the protocol's generated
//! schema uses (`docs/protocol/schema/cortex-protocol.schema.json`).
//!
//! It checks that a value really has the wire form the schema describes:
//! the golden transcript in `xtask`'s schema tests, and what the `cortex`
//! binary writes in headless mode, in its system tests. It knows the
//! keywords the generator writes (`$ref` into `$defs`, `const`, `enum`,
//! `not`, `type`, `minimum`, `required`, `properties`,
//! `additionalProperties`, `items`, `allOf`, `anyOf`, `oneOf`) and passes
//! over any other.

use serde_json::{Value, json};

/// Checks `instance` against the definition `name` in the schema document
/// `root`.
///
/// # Errors
///
/// What does not fit, and where: `value.items[2].kind: …`.
pub fn check(root: &Value, name: &str, instance: &Value) -> Result<(), String> {
    valid(
        &json!({"$ref": format!("#/$defs/{name}")}),
        instance,
        root,
        name,
    )
}

/// Checks `instance` against `schema`, a part of the document `root`, and
/// names a failure from `at`.
///
/// # Errors
///
/// What does not fit, and where.
pub fn valid(schema: &Value, instance: &Value, root: &Value, at: &str) -> Result<(), String> {
    walk(schema, instance, root, at, &mut |_| {})
}

/// As [`valid`], and tells `seen` the name of every definition the value
/// was found to fit: how a test shows that every type has a value.
///
/// # Errors
///
/// What does not fit, and where.
pub fn valid_seen(
    schema: &Value,
    instance: &Value,
    root: &Value,
    at: &str,
    seen: &mut dyn FnMut(&str),
) -> Result<(), String> {
    walk(schema, instance, root, at, seen)
}

fn walk(
    schema: &Value,
    instance: &Value,
    root: &Value,
    at: &str,
    seen: &mut dyn FnMut(&str),
) -> Result<(), String> {
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
        walk(target, instance, root, at, seen)?;
        seen(name);
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
        && walk(not, instance, root, at, seen).is_ok()
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
                Some(property) => walk(property, value, root, &format!("{at}.{name}"), seen)?,
                None => {
                    if let Some(additional) = object.get("additionalProperties") {
                        walk(additional, value, root, &format!("{at}.{name}"), seen)?;
                    }
                }
            }
        }
    }
    if let (Some(items), Some(elements)) = (object.get("items"), instance.as_array()) {
        for (index, element) in elements.iter().enumerate() {
            walk(items, element, root, &format!("{at}[{index}]"), seen)?;
        }
    }
    for part in object
        .get("allOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        walk(part, instance, root, at, seen)?;
    }
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = object.get(key).and_then(Value::as_array) {
            let failures: Vec<String> = branches
                .iter()
                .filter_map(|branch| walk(branch, instance, root, at, seen).err())
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
