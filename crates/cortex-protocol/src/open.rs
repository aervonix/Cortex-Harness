//! Open enumerations: the two shapes that let the protocol grow without a
//! version bump (`docs/architecture/11-rpc-protocol.md`, "Handshake and
//! versioning").
//!
//! - [`open_enum!`] makes a **string enumeration** with an `Unknown(String)`
//!   variant. A value this build does not know is kept, not rejected, and is
//!   written back unchanged.
//! - [`open_union!`] makes a **tagged union** of objects with an
//!   `Unknown(UnknownVariant)` variant. An object whose tag this build does
//!   not know is kept whole, and is written back unchanged.
//!
//! Two more macros make the **closed** counterparts, [`closed_enum!`] and
//! [`closed_union!`], for the values the harness enforces (leases, grants,
//! rules, limits). There an unknown value is an error, because a restriction
//! that is not understood cannot be enforced and must not be dropped
//! (`docs/decisions/0028-enforced-values-fail-closed.md`).
//!
//! Both are written by hand, not derived, for two reasons. Serde's
//! `#[serde(other)]` throws the unknown value away, so a client that relays
//! or stores a message would silently change it. And a malformed *known*
//! variant must stay an error with a message that names the field: it must
//! not be mistaken for an unknown one.

use serde::Serialize;
use serde::ser::SerializeMap;
use serde_json::{Map, Value};

/// A variant of a tagged union that this version of the protocol does not
/// know: its tag and every other field, kept as they arrived.
///
/// Receivers skip it or show it generically. It is written back unchanged, so
/// a client that relays or stores messages loses nothing.
#[derive(Debug, Clone, PartialEq)]
pub struct UnknownVariant {
    /// The value of the union's tag field (`type` or `kind`).
    pub tag: String,
    /// Every other field of the object.
    pub fields: Map<String, Value>,
}

impl UnknownVariant {
    /// The human-readable `message` field, when the object has one. Notices
    /// always do, so an unknown notice can still be shown.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.fields.get("message").and_then(Value::as_str)
    }

    pub(crate) fn serialize_tagged<S: serde::Serializer>(
        &self,
        serializer: S,
        tag_name: &'static str,
    ) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.fields.len() + 1))?;
        map.serialize_entry(tag_name, &self.tag)?;
        for (key, value) in &self.fields {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// A known variant on the wire: `{"kind": "...", ...the payload's fields}`.
#[derive(Serialize)]
pub(crate) struct KindTagged<'a, T> {
    pub(crate) kind: &'a str,
    #[serde(flatten)]
    pub(crate) inner: &'a T,
}

/// A known variant on the wire: `{"type": "...", ...the payload's fields}`.
#[derive(Serialize)]
pub(crate) struct TypeTagged<'a, T> {
    pub(crate) r#type: &'a str,
    #[serde(flatten)]
    pub(crate) inner: &'a T,
}

/// Reads an object and takes its tag out. The union's `Deserialize` then
/// reads the rest as the payload the tag names.
pub(crate) fn take_tag<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
    tag_name: &'static str,
    union: &'static str,
) -> Result<(String, Map<String, Value>), D::Error> {
    use serde::Deserialize;
    use serde::de::Error;

    let mut fields = Map::<String, Value>::deserialize(deserializer)?;
    match fields.remove(tag_name) {
        Some(Value::String(tag)) => Ok((tag, fields)),
        Some(_) => Err(D::Error::custom(format_args!(
            "{union}: `{tag_name}` must be a string"
        ))),
        None => Err(D::Error::custom(format_args!(
            "{union}: missing field `{tag_name}`"
        ))),
    }
}

/// An open enumeration, as an enforced value sees it: is this one of the
/// listed values?
pub(crate) trait Listed {
    /// Whether the value is one this version lists.
    fn is_listed(&self) -> bool;
    /// The value as written on the wire.
    fn wire(&self) -> &str;
    /// The name of the enumeration, for an error.
    fn enumeration() -> &'static str;
}

/// Reads an open enumeration where it is part of an enforced value: a value
/// that is not listed is an error, not something to keep. Used with
/// `#[serde(deserialize_with = …)]`.
pub(crate) fn listed<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de> + Listed,
{
    use serde::de::Error;

    let value = T::deserialize(deserializer)?;
    if value.is_listed() {
        Ok(value)
    } else {
        Err(D::Error::custom(format_args!(
            "unknown {} `{}`",
            T::enumeration(),
            value.wire()
        )))
    }
}

/// Joins the lines of a doc comment as the macros capture them.
#[cfg(feature = "schema")]
pub(crate) fn doc_text(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|line| line.trim())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

/// The schema of an open string enumeration: each known value with its
/// description, then any other string.
#[cfg(feature = "schema")]
pub(crate) fn string_enum_schema(doc: &[&str], variants: &[(&str, &[&str])]) -> schemars::Schema {
    let mut any_of: Vec<Value> = variants
        .iter()
        .map(|(wire, doc)| serde_json::json!({ "const": wire, "description": doc_text(doc) }))
        .collect();
    any_of.push(serde_json::json!({
        "type": "string",
        "description": "A value this version of the protocol does not know. Receivers need a default branch.",
        "x-cortex-open": true,
    }));
    schemars::json_schema!({
        "description": doc_text(doc),
        "anyOf": any_of,
    })
}

/// The schema of an open tagged union: each known variant as its tag plus
/// its payload, then any object with a tag this version does not know.
#[cfg(feature = "schema")]
pub(crate) fn union_schema(
    doc: &[&str],
    tag_name: &str,
    variants: Vec<(&str, String, schemars::Schema)>,
) -> schemars::Schema {
    let known: Vec<&str> = variants.iter().map(|(wire, _, _)| *wire).collect();
    let mut any_of: Vec<Value> = variants
        .iter()
        .map(|(wire, doc, payload)| {
            serde_json::json!({
                "description": doc,
                "allOf": [
                    {
                        "type": "object",
                        "properties": { tag_name: { "const": wire } },
                        "required": [tag_name],
                    },
                    payload,
                ],
            })
        })
        .collect();
    any_of.push(serde_json::json!({
        "description": "A variant this version of the protocol does not know. Receivers skip it or show it generically.",
        "type": "object",
        "properties": { tag_name: { "type": "string", "not": { "enum": known } } },
        "required": [tag_name],
        "x-cortex-open": true,
    }));
    schemars::json_schema!({
        "description": doc_text(doc),
        "anyOf": any_of,
    })
}

/// The schema of a closed string enumeration: each value with its
/// description, and nothing else.
#[cfg(feature = "schema")]
pub(crate) fn closed_enum_schema(doc: &[&str], variants: &[(&str, &[&str])]) -> schemars::Schema {
    let any_of: Vec<Value> = variants
        .iter()
        .map(|(wire, doc)| serde_json::json!({ "const": wire, "description": doc_text(doc) }))
        .collect();
    schemars::json_schema!({
        "description": doc_text(doc),
        "anyOf": any_of,
    })
}

/// The schema of a closed tagged union: each variant as one object with its
/// tag and its payload's fields, and no other variant and no other field.
/// The payload is written inline, because a payload that forbids unknown
/// fields cannot be combined with a separate object that adds the tag.
#[cfg(feature = "schema")]
pub(crate) fn closed_union_schema(
    doc: &[&str],
    tag_name: &str,
    variants: Vec<(&str, String, schemars::Schema)>,
) -> schemars::Schema {
    let any_of: Vec<Value> = variants
        .into_iter()
        .map(|(wire, doc, payload)| {
            let mut variant = payload.to_value();
            if let Some(object) = variant.as_object_mut() {
                object.insert("description".to_owned(), Value::String(doc));
                object.insert("additionalProperties".to_owned(), Value::Bool(false));
                let properties = object
                    .entry("properties")
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Some(properties) = properties.as_object_mut() {
                    properties.insert(tag_name.to_owned(), serde_json::json!({ "const": wire }));
                }
                let required = object
                    .entry("required")
                    .or_insert_with(|| Value::Array(Vec::new()));
                if let Some(required) = required.as_array_mut() {
                    required.insert(0, Value::String(tag_name.to_owned()));
                }
            }
            variant
        })
        .collect();
    schemars::json_schema!({
        "description": doc_text(doc),
        "anyOf": any_of,
    })
}

/// Defines a string enumeration that may grow.
///
/// ```ignore
/// open_enum! {
///     /// Why the model stopped.
///     pub enum StopReason {
///         /// The model finished.
///         End => "end",
///     }
/// }
/// ```
macro_rules! open_enum {
    ($(#[doc = $doc:literal])* $vis:vis enum $name:ident { $($body:tt)+ }) => {
        open_enum!(
            @define Unknown,
            "A value this version of the protocol does not know, as it was written on the wire.",
            [$($doc),*] $vis $name { $($body)+ }
        );
    };
    // The same, with the catch-all variant named: for an enumeration whose
    // other values are expected and meaningful, not merely newer.
    (
        $(#[doc = $doc:literal])*
        $vis:vis enum $name:ident, other = $other:ident($odoc:literal) { $($body:tt)+ }
    ) => {
        open_enum!(@define $other, $odoc, [$($doc),*] $vis $name { $($body)+ });
    };
    (
        @define $other:ident, $odoc:literal, [$($doc:literal),*]
        $vis:vis $name:ident {
            $(
                $(#[doc = $vdoc:literal])*
                $variant:ident => $wire:literal
            ),+ $(,)?
        }
    ) => {
        $(#[doc = $doc])*
        ///
        /// Open: a value that is not listed is kept, and written back unchanged.
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis enum $name {
            $(
                $(#[doc = $vdoc])*
                $variant,
            )+
            #[doc = $odoc]
            ///
            /// Build values with [`Self::from_wire`], so that a listed string
            /// is never held here.
            $other(String),
        }

        impl $name {
            /// Every value this version knows, as written on the wire.
            pub const KNOWN: &'static [&'static str] = &[$($wire),+];

            /// The value as written on the wire.
            #[must_use]
            pub fn as_str(&self) -> &str {
                match self {
                    $(Self::$variant => $wire,)+
                    Self::$other(other) => other,
                }
            }

            /// Reads a wire value. A listed string gives its variant; any
            /// other string is kept.
            #[must_use]
            pub fn from_wire(value: &str) -> Self {
                match value {
                    $($wire => Self::$variant,)+
                    other => Self::$other(other.to_owned()),
                }
            }

            /// Whether the value is one of the listed ones.
            #[must_use]
            pub fn is_known(&self) -> bool {
                !matches!(self, Self::$other(_))
            }
        }

        impl $crate::open::Listed for $name {
            fn is_listed(&self) -> bool {
                self.is_known()
            }
            fn wire(&self) -> &str {
                self.as_str()
            }
            fn enumeration() -> &'static str {
                stringify!($name)
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Visitor;
                impl ::serde::de::Visitor<'_> for Visitor {
                    type Value = $name;
                    fn expecting(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                        f.write_str("a string")
                    }
                    fn visit_str<E: ::serde::de::Error>(self, value: &str) -> Result<$name, E> {
                        Ok($name::from_wire(value))
                    }
                }
                deserializer.deserialize_str(Visitor)
            }
        }

        #[cfg(feature = "schema")]
        impl ::schemars::JsonSchema for $name {
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }
            fn json_schema(_: &mut ::schemars::SchemaGenerator) -> ::schemars::Schema {
                $crate::open::string_enum_schema(
                    &[$($doc),*],
                    &[$(($wire, &[$($vdoc),*])),+],
                )
            }
        }
    };
}

/// Defines a tagged union of objects that may grow. `[kind]` or `[type]`
/// names the tag field. Every variant holds one payload type that is written
/// as a JSON object.
///
/// ```ignore
/// open_union! {
///     /// One part of a message.
///     pub enum Part [kind] {
///         /// Plain text.
///         Text(TextPart) => "text",
///     }
/// }
/// ```
macro_rules! open_union {
    ($(#[doc = $doc:literal])* $vis:vis enum $name:ident [kind] { $($body:tt)+ }) => {
        open_union!(@define "kind", KindTagged, kind, [$($doc),*] $vis $name { $($body)+ });
    };
    ($(#[doc = $doc:literal])* $vis:vis enum $name:ident [type] { $($body:tt)+ }) => {
        open_union!(@define "type", TypeTagged, r#type, [$($doc),*] $vis $name { $($body)+ });
    };
    (
        @define $tag:literal, $helper:ident, $field:ident, [$($doc:literal),*]
        $vis:vis $name:ident {
            $(
                $(#[doc = $vdoc:literal])*
                $variant:ident($inner:ty) => $wire:literal
            ),+ $(,)?
        }
    ) => {
        $(#[doc = $doc])*
        ///
        /// Open: an object whose tag this version does not know is kept as
        /// `Unknown` and written back unchanged.
        #[derive(Debug, Clone, PartialEq)]
        #[allow(
            clippy::large_enum_variant,
            reason = "a value is built once and matched often; boxing each payload would cost an allocation per message"
        )]
        $vis enum $name {
            $(
                $(#[doc = $vdoc])*
                $variant($inner),
            )+
            /// A variant this version of the protocol does not know.
            Unknown($crate::open::UnknownVariant),
        }

        impl $name {
            /// The name of the field that says which variant an object is.
            pub const TAG: &'static str = $tag;

            /// Every variant this version knows, as written on the wire.
            pub const KNOWN: &'static [&'static str] = &[$($wire),+];

            /// This value's variant, as written on the wire.
            #[must_use]
            pub fn tag(&self) -> &str {
                match self {
                    $(Self::$variant(_) => $wire,)+
                    Self::Unknown(unknown) => &unknown.tag,
                }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match self {
                    $(
                        Self::$variant(inner) => ::serde::Serialize::serialize(
                            &$crate::open::$helper { $field: $wire, inner },
                            serializer,
                        ),
                    )+
                    Self::Unknown(unknown) => unknown.serialize_tagged(serializer, $tag),
                }
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let (tag, fields) = $crate::open::take_tag(deserializer, $tag, stringify!($name))?;
                match tag.as_str() {
                    $(
                        $wire => <$inner as ::serde::Deserialize>::deserialize(
                            ::serde_json::Value::Object(fields),
                        )
                        .map(Self::$variant)
                        .map_err(|error| {
                            <D::Error as ::serde::de::Error>::custom(format_args!(
                                "{} `{}`: {error}",
                                stringify!($name),
                                $wire,
                            ))
                        }),
                    )+
                    _ => Ok(Self::Unknown($crate::open::UnknownVariant { tag, fields })),
                }
            }
        }

        #[cfg(feature = "schema")]
        impl ::schemars::JsonSchema for $name {
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }
            fn json_schema(generator: &mut ::schemars::SchemaGenerator) -> ::schemars::Schema {
                let variants = vec![
                    $((
                        $wire,
                        $crate::open::doc_text(&[$($vdoc),*]),
                        generator.subschema_for::<$inner>(),
                    )),+
                ];
                $crate::open::union_schema(&[$($doc),*], $tag, variants)
            }
        }
    };
}

/// Defines a string enumeration of an enforced value. It has no catch-all:
/// a value that is not listed is an error.
macro_rules! closed_enum {
    (
        $(#[doc = $doc:literal])*
        $vis:vis enum $name:ident {
            $(
                $(#[doc = $vdoc:literal])*
                $variant:ident => $wire:literal
            ),+ $(,)?
        }
    ) => {
        $(#[doc = $doc])*
        ///
        /// Closed: it is part of an enforced value, so a value that is not
        /// listed is an error and the value it is in is refused.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis enum $name {
            $(
                $(#[doc = $vdoc])*
                $variant,
            )+
        }

        impl $name {
            /// Every value, as written on the wire.
            pub const KNOWN: &'static [&'static str] = &[$($wire),+];

            /// The value as written on the wire.
            #[must_use]
            pub fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $wire,)+
                }
            }

            /// Reads a wire value. `None` for a string that is not listed.
            #[must_use]
            pub fn from_wire(value: &str) -> Option<Self> {
                match value {
                    $($wire => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Visitor;
                impl ::serde::de::Visitor<'_> for Visitor {
                    type Value = $name;
                    fn expecting(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                        f.write_str("a string")
                    }
                    fn visit_str<E: ::serde::de::Error>(self, value: &str) -> Result<$name, E> {
                        $name::from_wire(value).ok_or_else(|| {
                            E::custom(format_args!("unknown {} `{value}`", stringify!($name)))
                        })
                    }
                }
                deserializer.deserialize_str(Visitor)
            }
        }

        #[cfg(feature = "schema")]
        impl ::schemars::JsonSchema for $name {
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }
            fn json_schema(_: &mut ::schemars::SchemaGenerator) -> ::schemars::Schema {
                $crate::open::closed_enum_schema(
                    &[$($doc),*],
                    &[$(($wire, &[$($vdoc),*])),+],
                )
            }
        }
    };
}

/// Defines a tagged union of an enforced value, tagged by `kind`. It has no
/// catch-all: a `kind` that is not listed is an error. The payload types
/// forbid unknown fields themselves.
macro_rules! closed_union {
    (
        $(#[doc = $doc:literal])*
        $vis:vis enum $name:ident [kind] {
            $(
                $(#[doc = $vdoc:literal])*
                $variant:ident($inner:ty) => $wire:literal
            ),+ $(,)?
        }
    ) => {
        $(#[doc = $doc])*
        ///
        /// Closed: it is part of an enforced value, so a `kind` that is not
        /// listed is an error and the value it is in is refused.
        #[derive(Debug, Clone, PartialEq, Eq)]
        $vis enum $name {
            $(
                $(#[doc = $vdoc])*
                $variant($inner),
            )+
        }

        impl $name {
            /// The name of the field that says which variant an object is.
            pub const TAG: &'static str = "kind";

            /// Every variant, as written on the wire.
            pub const KNOWN: &'static [&'static str] = &[$($wire),+];

            /// This value's variant, as written on the wire.
            #[must_use]
            pub fn tag(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $wire,)+
                }
            }
        }

        impl ::serde::Serialize for $name {
            fn serialize<S: ::serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match self {
                    $(
                        Self::$variant(inner) => ::serde::Serialize::serialize(
                            &$crate::open::KindTagged { kind: $wire, inner },
                            serializer,
                        ),
                    )+
                }
            }
        }

        impl<'de> ::serde::Deserialize<'de> for $name {
            fn deserialize<D: ::serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let (tag, fields) = $crate::open::take_tag(deserializer, "kind", stringify!($name))?;
                match tag.as_str() {
                    $(
                        $wire => <$inner as ::serde::Deserialize>::deserialize(
                            ::serde_json::Value::Object(fields),
                        )
                        .map(Self::$variant)
                        .map_err(|error| {
                            <D::Error as ::serde::de::Error>::custom(format_args!(
                                "{} `{}`: {error}",
                                stringify!($name),
                                $wire,
                            ))
                        }),
                    )+
                    other => Err(<D::Error as ::serde::de::Error>::custom(format_args!(
                        "unknown {} `{other}`",
                        stringify!($name),
                    ))),
                }
            }
        }

        #[cfg(feature = "schema")]
        impl ::schemars::JsonSchema for $name {
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }
            fn json_schema(generator: &mut ::schemars::SchemaGenerator) -> ::schemars::Schema {
                let variants = vec![
                    $((
                        $wire,
                        $crate::open::doc_text(&[$($vdoc),*]),
                        <$inner as ::schemars::JsonSchema>::json_schema(generator),
                    )),+
                ];
                $crate::open::closed_union_schema(&[$($doc),*], "kind", variants)
            }
        }
    };
}

pub(crate) use {closed_enum, closed_union, open_enum, open_union};
