//! The redaction hook (`docs/security.md`, section 4).
//!
//! A [`Redactor`] sees the text of every entry and every text artifact
//! before it is written, and may mask what looks like a secret. This crate
//! provides the hook and a redactor that does nothing; the filter itself
//! (the harness's own credentials by value, and well-known key formats) is
//! `cortex_base::SecretFilter`, which the core puts behind the hook.
//!
//! **An entry it changed says so.** What the log then holds is not what
//! the model was sent or wrote, and on the providers that tie reasoning to
//! the history before it, the reasoning that came after can no longer be
//! replayed against it. So the entry is written with `"redacted": true` in
//! its envelope ([`Entry::redacted`](crate::Entry)), and whoever resumes
//! the session reads the mark and sends what follows without its reasoning
//! (`docs/architecture/07-sessions-and-context.md`, "Secrets in sessions").
//! A caller that altered an entry itself before handing it over, as the
//! core does when a provider's own block holds a secret, sets the mark
//! with `Session::append_altered`.
//!
//! **What it does not see:**
//!
//! - **The provider's own blocks** (`raw` fields: reasoning signatures,
//!   encrypted content, the provider's usage object). Changing a byte of one
//!   makes the provider refuse it on replay. A block that holds a secret is
//!   the caller's to deal with before the entry is handed over: the core
//!   stores such a message without its raw blocks, and marks it.
//! - **File contents kept for undo** (`checkpoints/`). A restore must put
//!   back exactly what was there.
//! - **Artifacts that are not text**, such as images.

use std::borrow::Cow;

use serde_json::Value;

/// Masks secrets in text before it is written to a session.
pub trait Redactor: Send + Sync + std::fmt::Debug {
    /// The text with anything that looks like a secret masked. Text with
    /// nothing to mask comes back borrowed.
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str>;

    /// Whether this redactor never changes anything, so that the work of
    /// walking an entry can be skipped.
    fn is_noop(&self) -> bool {
        false
    }
}

/// A redactor that changes nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoRedaction;

impl Redactor for NoRedaction {
    fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(text)
    }

    fn is_noop(&self) -> bool {
        true
    }
}

/// The fields of an entry that hold JSON somebody else wrote: a tool
/// call's arguments (the model's), a tool result's payload, and the
/// free-form fields of this crate's own records. A key named `raw` inside
/// one is that somebody's, not a provider's block.
const FREE_FORM: [&str; 6] = [
    "arguments",
    "payload",
    "data",
    "detail",
    "basis",
    "allowed_by",
];

/// Applies `redactor` to every string in `value`, except in fields named
/// `raw`, which hold a provider's blocks. Says whether it changed anything.
///
/// A provider's block is a `raw` field of a part or of a usage record.
/// Inside JSON that is somebody's own ([`FREE_FORM`]) the name means
/// nothing, and what is under it is redacted like the rest.
pub(crate) fn value(value: &mut Value, redactor: &dyn Redactor) -> bool {
    walk(value, redactor, false)
}

fn walk(value: &mut Value, redactor: &dyn Redactor, free_form: bool) -> bool {
    match value {
        Value::String(text) => {
            if let Cow::Owned(masked) = redactor.redact(text) {
                *text = masked;
                return true;
            }
            false
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= walk(item, redactor, free_form);
            }
            changed
        }
        Value::Object(fields) => {
            let mut changed = false;
            for (key, item) in fields.iter_mut() {
                if free_form || key != "raw" {
                    let free_form = free_form || FREE_FORM.contains(&key.as_str());
                    changed |= walk(item, redactor, free_form);
                }
            }
            changed
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::json;

    use super::*;

    /// Masks the word `hunter2`, for tests.
    #[derive(Debug)]
    pub(crate) struct MaskHunter;

    impl Redactor for MaskHunter {
        fn redact<'a>(&self, text: &'a str) -> Cow<'a, str> {
            if text.contains("hunter2") {
                Cow::Owned(text.replace("hunter2", "[redacted]"))
            } else {
                Cow::Borrowed(text)
            }
        }
    }

    #[test]
    fn every_string_is_redacted_except_raw_blocks() {
        let mut entry = json!({
            "item": {"parts": [
                {"kind": "text", "text": "the password is hunter2", "raw": "{\"text\":\"hunter2\"}"},
                {"kind": "tool_call", "arguments": {"cmd": "echo hunter2"}}
            ]},
            "n": 3
        });
        assert!(value(&mut entry, &MaskHunter), "it says that it masked");
        assert!(!value(&mut entry, &MaskHunter), "and that there is no more");
        assert_eq!(
            entry,
            json!({
                "item": {"parts": [
                    {"kind": "text", "text": "the password is [redacted]", "raw": "{\"text\":\"hunter2\"}"},
                    {"kind": "tool_call", "arguments": {"cmd": "echo [redacted]"}}
                ]},
                "n": 3
            })
        );
    }

    /// `raw` is a provider's block where a part or a usage record has it.
    /// Under a name the model chose, in a tool call's arguments or in what
    /// a tool returned, it is a key like any other.
    #[test]
    fn a_key_named_raw_in_somebody_elses_json_is_redacted() {
        let mut entry = json!({
            "item": {"parts": [
                {"kind": "tool_call",
                 "arguments": {"raw": "hunter2", "deep": [{"raw": {"x": "hunter2"}}]},
                 "raw": "{\"input\":\"hunter2\"}"}
            ]},
            "result": {"payload": {"raw": "hunter2"}},
            "usage": {"raw": "{\"note\":\"hunter2\"}"}
        });
        assert!(value(&mut entry, &MaskHunter));
        assert_eq!(
            entry,
            json!({
                "item": {"parts": [
                    {"kind": "tool_call",
                     "arguments": {"raw": "[redacted]", "deep": [{"raw": {"x": "[redacted]"}}]},
                     "raw": "{\"input\":\"hunter2\"}"}
                ]},
                "result": {"payload": {"raw": "[redacted]"}},
                "usage": {"raw": "{\"note\":\"hunter2\"}"}
            })
        );
    }
}
