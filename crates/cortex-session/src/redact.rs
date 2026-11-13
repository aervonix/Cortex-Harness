//! The redaction hook (`docs/security.md`, section 4).
//!
//! A [`Redactor`] sees the text of every entry and every text artifact
//! before it is written, and may mask what looks like a secret. This crate
//! provides the hook and a redactor that does nothing; the filter itself
//! (the harness's own credentials by value, and well-known key formats) is
//! built when credentials are wired into the core.
//!
//! **What it does not see:**
//!
//! - **The provider's own blocks** (`raw` fields: reasoning signatures,
//!   encrypted content, the provider's usage object). Changing a byte of one
//!   makes the provider refuse it on replay. Whether a block that holds a
//!   secret is dropped or kept is an open question for the step that builds
//!   the filter.
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

/// Applies `redactor` to every string in `value`, except in fields named
/// `raw`, which hold a provider's blocks.
pub(crate) fn value(value: &mut Value, redactor: &dyn Redactor) {
    match value {
        Value::String(text) => {
            if let Cow::Owned(masked) = redactor.redact(text) {
                *text = masked;
            }
        }
        Value::Array(items) => {
            for item in items {
                self::value(item, redactor);
            }
        }
        Value::Object(fields) => {
            for (key, item) in fields.iter_mut() {
                if key != "raw" {
                    self::value(item, redactor);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
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
        value(&mut entry, &MaskHunter);
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
}
