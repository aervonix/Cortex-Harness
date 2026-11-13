//! A small writer of JSON text, for request bodies whose bytes matter.
//!
//! The body of a request is written by hand and not through a JSON value,
//! for two reasons:
//!
//! - **The order of keys is the order of the code.** The parts of a request
//!   that stay the same for a context epoch come first, in the same bytes
//!   every time, whatever a map type would have done with them.
//! - **A raw block goes in untouched.** A block the provider produced is
//!   replayed as the text that was stored, with no parser in between that
//!   could reorder its keys, change its escapes or round its numbers
//!   ([`Object::raw`]).
//!
//! Strings are escaped the shortest way JSON allows: `"` and `\`, the
//! control characters, and nothing else. Text that is not ASCII is written
//! as it is.

use std::fmt::Write as _;

use serde::Deserialize;
use serde_json::Value;
use serde_json::value::RawValue;

/// Appends `text` as a JSON string, with its quotes.
pub(crate) fn string(out: &mut String, text: &str) {
    out.reserve(text.len() + 2);
    out.push('"');
    let mut plain_from = 0;
    for (at, byte) in text.bytes().enumerate() {
        let escape = match byte {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            b'\n' => "\\n",
            b'\r' => "\\r",
            b'\t' => "\\t",
            0x00..=0x1f => "",
            _ => continue,
        };
        // Every byte matched above is ASCII, so `at` is a character boundary.
        out.push_str(&text[plain_from..at]);
        if escape.is_empty() {
            // Writing to a `String` cannot fail.
            let _ = write!(out, "\\u{byte:04x}");
        } else {
            out.push_str(escape);
        }
        plain_from = at + 1;
    }
    out.push_str(&text[plain_from..]);
    out.push('"');
}

/// Appends a JSON value. Its object keys come out in the order the value
/// holds them, which is the same on every call for the same value.
pub(crate) fn value(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => {
            let _ = write!(out, "{number}");
        }
        Value::String(text) => string(out, text),
        Value::Array(items) => {
            let mut array = Array::open(out);
            for item in items {
                self::value(array.item(), item);
            }
            array.close();
        }
        Value::Object(members) => {
            let mut object = Object::open(out);
            for (key, member) in members {
                self::value(object.key(key), member);
            }
            object.close();
        }
    }
}

/// Whether `text` is exactly one JSON object and nothing else. A raw block
/// is checked with this before it is written into a body, so that text which
/// is not a block (a damaged log, or text made to close the surrounding
/// array and open another message) can never change the request's shape.
pub(crate) fn is_object(text: &str) -> bool {
    text.trim_start().starts_with('{')
        && serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok()
}

/// A JSON object being written. Members appear in the order they are added.
pub(crate) struct Object<'a> {
    out: &'a mut String,
    empty: bool,
}

impl<'a> Object<'a> {
    /// Starts an object.
    pub(crate) fn open(out: &'a mut String) -> Self {
        out.push('{');
        Self { out, empty: true }
    }

    /// Writes a member's key and returns where its value goes. The caller
    /// writes exactly one value there.
    pub(crate) fn key(&mut self, key: &str) -> &mut String {
        if !self.empty {
            self.out.push(',');
        }
        self.empty = false;
        string(self.out, key);
        self.out.push(':');
        self.out
    }

    /// A string member.
    pub(crate) fn str(&mut self, key: &str, text: &str) {
        string(self.key(key), text);
    }

    /// A member whose value is `true`.
    pub(crate) fn yes(&mut self, key: &str) {
        self.key(key).push_str("true");
    }

    /// A whole-number member.
    pub(crate) fn number(&mut self, key: &str, number: u64) {
        let _ = write!(self.key(key), "{number}");
    }

    /// A member whose value is a JSON value.
    pub(crate) fn value(&mut self, key: &str, value: &Value) {
        self::value(self.key(key), value);
    }

    /// A member whose value is JSON text, written exactly as it is. The
    /// caller has checked that it is one JSON value.
    pub(crate) fn raw(&mut self, key: &str, json: &str) {
        self.key(key).push_str(json);
    }

    /// Ends the object.
    pub(crate) fn close(self) {
        self.out.push('}');
    }
}

/// A JSON array being written.
pub(crate) struct Array<'a> {
    out: &'a mut String,
    empty: bool,
}

impl<'a> Array<'a> {
    /// Starts an array.
    pub(crate) fn open(out: &'a mut String) -> Self {
        out.push('[');
        Self { out, empty: true }
    }

    /// Returns where the next item goes. The caller writes exactly one
    /// value there.
    pub(crate) fn item(&mut self) -> &mut String {
        if !self.empty {
            self.out.push(',');
        }
        self.empty = false;
        self.out
    }

    /// Ends the array.
    pub(crate) fn close(self) {
        self.out.push(']');
    }
}

/// How many members an object has before the reader keeps an index of
/// their keys. Below it, looking through them is quicker than hashing.
const INDEXED_FROM: usize = 32;

/// The members of a JSON object in the order they were written, each as
/// its own text.
pub(crate) fn ordered_members(
    raw: &RawValue,
) -> Result<Vec<(String, Box<RawValue>)>, serde_json::Error> {
    struct Ordered(Vec<(String, Box<RawValue>)>);

    impl<'de> Deserialize<'de> for Ordered {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct Visitor;

            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Ordered;

                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("a JSON object")
                }

                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut map: A,
                ) -> Result<Ordered, A::Error> {
                    let mut members: Vec<(String, Box<RawValue>)> = Vec::new();
                    // Where each key is, once there are enough of them that
                    // looking through all of them for every new one would
                    // cost the square of their number: an object from a
                    // server may have millions.
                    let mut places: Option<std::collections::HashMap<String, usize>> = None;
                    while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                        if places.is_none() && members.len() >= INDEXED_FROM {
                            places = Some(
                                members
                                    .iter()
                                    .enumerate()
                                    .map(|(at, (known, _))| (known.clone(), at))
                                    .collect(),
                            );
                        }
                        let at = match &places {
                            Some(places) => places.get(&key).copied(),
                            None => members.iter().position(|(known, _)| *known == key),
                        };
                        // A key written twice keeps its last value, as a
                        // JSON reader would.
                        match at {
                            Some(at) => members[at].1 = value,
                            None => {
                                if let Some(places) = &mut places {
                                    places.insert(key.clone(), members.len());
                                }
                                members.push((key, value));
                            }
                        }
                    }
                    Ok(Ordered(members))
                }
            }

            deserializer.deserialize_map(Visitor)
        }
    }

    serde_json::from_str::<Ordered>(raw.get()).map(|ordered| ordered.0)
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_object_with_very_many_members_is_read_in_order_and_quickly() {
        // Enough members that looking through all of them for each new one
        // would be 2.5 thousand million comparisons: with an index of the
        // keys it is fifty thousand lookups.
        let count = 50_000;
        let mut text = String::from("{");
        for at in 0..count {
            text.push_str(&format!("\"k{at}\":{at},"));
        }
        // Two keys written again, on either side of where the index
        // begins: each keeps its place and takes its last value.
        text.push_str("\"k3\":\"again\",\"k40000\":\"again\"}");
        let raw: Box<RawValue> = serde_json::from_str(&text).unwrap();
        let members = ordered_members(&raw).unwrap();
        assert_eq!(members.len(), count);
        for at in [0, 1, 31, 32, 33, 49_999] {
            assert_eq!(members[at].0, format!("k{at}"));
            assert_eq!(members[at].1.get(), at.to_string());
        }
        assert_eq!(members[3].1.get(), "\"again\"");
        assert_eq!(members[40_000].1.get(), "\"again\"");
        // A small object goes the same way.
        let small: Box<RawValue> = serde_json::from_str(r#"{"a":1,"b":2,"a":3}"#).unwrap();
        let members = ordered_members(&small).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!((members[0].0.as_str(), members[0].1.get()), ("a", "3"));
    }

    use cortex_base::Rng as _;
    use cortex_testkit::cases;
    use serde_json::json;

    use super::*;

    fn written(text: &str) -> String {
        let mut out = String::new();
        string(&mut out, text);
        out
    }

    #[test]
    fn a_string_is_escaped_the_shortest_way() {
        assert_eq!(written(""), r#""""#);
        assert_eq!(written("plain"), r#""plain""#);
        assert_eq!(written("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(written("line\nbreak\r\ttab"), r#""line\nbreak\r\ttab""#);
        assert_eq!(written("\u{0}\u{1f}\u{7f}"), "\"\\u0000\\u001f\u{7f}\"");
        // Text that is not ASCII is written as it is, and so is a slash.
        assert_eq!(written("é→日本 </script>"), "\"é→日本 </script>\"");
    }

    #[test]
    fn every_string_reads_back_as_itself() {
        cases::check(
            "json string round trip",
            300,
            |rng| {
                let length = rng.below(40);
                (0..length)
                    .map(|_| match rng.below(6) {
                        0 => char::from(u8::try_from(rng.below(0x20)).unwrap_or(0)),
                        1 => '"',
                        2 => '\\',
                        3 => char::from_u32(0x80 + u32::try_from(rng.below(0x2000)).unwrap_or(0))
                            .unwrap_or('é'),
                        4 => '\u{1f600}',
                        _ => char::from(u8::try_from(0x20 + rng.below(0x5f)).unwrap_or(b' ')),
                    })
                    .collect::<String>()
            },
            |text| {
                let json = written(text);
                match serde_json::from_str::<String>(&json) {
                    Ok(back) if back == *text => Ok(()),
                    other => Err(format!("{json} read back as {other:?}").into()),
                }
            },
        );
    }

    #[test]
    fn objects_and_arrays_keep_the_order_they_were_written_in() {
        let mut out = String::new();
        let mut object = Object::open(&mut out);
        object.str("zebra", "first");
        object.number("apple", 7);
        object.yes("mango");
        object.raw("raw", r#"{"b":1,"a":2.50}"#);
        let mut array = Array::open(object.key("list"));
        array.item().push('1');
        string(array.item(), "two");
        array.close();
        Object::open(object.key("empty")).close();
        Array::open(object.key("none")).close();
        object.close();
        assert_eq!(
            out,
            r#"{"zebra":"first","apple":7,"mango":true,"raw":{"b":1,"a":2.50},"list":[1,"two"],"empty":{},"none":[]}"#
        );
    }

    #[test]
    fn a_value_is_written_as_the_same_json() {
        let source = json!({
            "type": "object",
            "properties": {"path": {"type": ["string", "null"], "description": "a \"path\""}},
            "required": ["path"],
            "additionalProperties": false,
            "n": [1, -2, 3.5, null, true],
        });
        let mut out = String::new();
        value(&mut out, &source);
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), source);
        // The same value gives the same bytes.
        let mut again = String::new();
        value(&mut again, &source);
        assert_eq!(out, again);
    }

    #[test]
    fn only_one_whole_object_is_an_object() {
        for good in [
            r#"{"type":"text","text":"hi"}"#,
            "{}",
            " {\"a\": [1, {\"b\": null}]} ",
        ] {
            assert!(is_object(good), "{good}");
        }
        for bad in [
            "",
            "null",
            "[]",
            "\"text\"",
            "{",
            r#"{"a":1"#,
            // Text made to close the block and start another one.
            r#"{"type":"text","text":"x"},{"type":"text","text":"y"}"#,
            r#"{"a":1}]},{"role":"user""#,
            r#"{"a":1} trailing"#,
        ] {
            assert!(!is_object(bad), "{bad}");
        }
    }
}
