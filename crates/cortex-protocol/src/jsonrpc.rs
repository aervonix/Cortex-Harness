//! JSON-RPC 2.0 envelopes: the three kinds of message that travel in both
//! directions over standard I/O and local sockets
//! (`docs/architecture/11-rpc-protocol.md`, "Messages").
//!
//! The envelopes carry `params` and `result` as untyped JSON. The typed
//! contents are in [`crate::methods`]: each method's parameters and result,
//! and the mapping between a method name and its types.

use serde::de::Error as _;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ErrorData;

/// The `jsonrpc` member of every message.
pub const JSONRPC_VERSION: &str = "2.0";

/// The standard JSON-RPC error codes, and the one code this protocol adds.
pub mod code {
    /// The line was not valid JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON was not a valid JSON-RPC message.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist, or needs a capability or role the
    /// connection does not have.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// The parameters do not fit the method.
    pub const INVALID_PARAMS: i64 = -32602;
    /// The server failed in a way that says nothing about the request.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Every application error. Its `data` is an [`ErrorData`](crate::ErrorData),
    /// and a client branches on `data.kind` and `data.code`, never on this number.
    pub const APPLICATION_ERROR: i64 = -32000;
}

/// The id that ties a response to its request: a number or a string, chosen
/// by the side that sends the request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RequestId {
    /// A whole number.
    Number(i64),
    /// A string.
    String(String),
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(number) => write!(f, "{number}"),
            Self::String(string) => f.write_str(string),
        }
    }
}

impl From<i64> for RequestId {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}

impl From<&str> for RequestId {
    fn from(value: &str) -> Self {
        Self::String(value.to_owned())
    }
}

impl Serialize for RequestId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Number(number) => serializer.serialize_i64(*number),
            Self::String(string) => serializer.serialize_str(string),
        }
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = RequestId;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a whole number or a string")
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<RequestId, E> {
                Ok(RequestId::Number(value))
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<RequestId, E> {
                i64::try_from(value)
                    .map(RequestId::Number)
                    .map_err(|_| E::custom("a request id must fit in 64 signed bits"))
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<RequestId, E> {
                Ok(RequestId::String(value.to_owned()))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// A request: the receiver must answer it with a [`Response`] that has the same id.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Chosen by the sender, unique among its requests that are still open.
    pub id: RequestId,
    /// The method name, for example `turn/start`.
    pub method: String,
    /// The method's parameters. Absent when the method takes none.
    pub params: Option<Value>,
}

/// A notification: a message that gets no answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    /// The method name, for example `session/event`.
    pub method: String,
    /// The notification's parameters.
    pub params: Option<Value>,
}

/// The answer to a [`Request`].
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// The request's id. `None` only when the request was so malformed that
    /// its id could not be read.
    pub id: Option<RequestId>,
    /// The method's result, or the error.
    pub result: Result<Value, ErrorObject>,
}

/// The error member of a failed [`Response`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorObject {
    /// One of the codes in [`code`].
    pub code: i64,
    /// One sentence for a person.
    pub message: String,
    /// For an application error, an [`ErrorData`]. Otherwise optional detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ErrorObject {
    /// An application error: [`code::APPLICATION_ERROR`], with the typed
    /// error as `data` and its message repeated as the JSON-RPC message.
    #[must_use]
    pub fn application(error: &ErrorData) -> Self {
        Self {
            code: code::APPLICATION_ERROR,
            message: error.message.clone(),
            // An `ErrorData` is strings, numbers and one JSON map, so writing
            // it as a value cannot fail. `None` keeps the error usable if it
            // ever does.
            data: serde_json::to_value(error).ok(),
        }
    }

    /// The typed error, when `data` holds one.
    #[must_use]
    pub fn error_data(&self) -> Option<ErrorData> {
        self.data
            .as_ref()
            .and_then(|data| ErrorData::deserialize(data).ok())
    }
}

/// Any JSON-RPC message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A message with an id and a method.
    Request(Request),
    /// A message with a method and no id.
    Notification(Notification),
    /// A message with an id and a result or an error.
    Response(Response),
}

impl From<Request> for Message {
    fn from(request: Request) -> Self {
        Self::Request(request)
    }
}

impl From<Notification> for Message {
    fn from(notification: Notification) -> Self {
        Self::Notification(notification)
    }
}

impl From<Response> for Message {
    fn from(response: Response) -> Self {
        Self::Response(response)
    }
}

impl Serialize for Request {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("jsonrpc", JSONRPC_VERSION)?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("method", &self.method)?;
        if let Some(params) = &self.params {
            map.serialize_entry("params", params)?;
        }
        map.end()
    }
}

impl Serialize for Notification {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("jsonrpc", JSONRPC_VERSION)?;
        map.serialize_entry("method", &self.method)?;
        if let Some(params) = &self.params {
            map.serialize_entry("params", params)?;
        }
        map.end()
    }
}

impl Serialize for Response {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("jsonrpc", JSONRPC_VERSION)?;
        map.serialize_entry("id", &self.id)?;
        match &self.result {
            Ok(result) => map.serialize_entry("result", result)?,
            Err(error) => map.serialize_entry("error", error)?,
        }
        map.end()
    }
}

impl Serialize for Message {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Request(request) => request.serialize(serializer),
            Self::Notification(notification) => notification.serialize(serializer),
            Self::Response(response) => response.serialize(serializer),
        }
    }
}

/// Keeps "the member is there and is null" apart from "the member is absent",
/// which JSON-RPC needs for `id` and for `result`.
fn present<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

/// Every member a message can have. Which ones are present says what kind of
/// message it is.
#[derive(Deserialize)]
struct Wire {
    jsonrpc: Option<String>,
    #[serde(default, deserialize_with = "present")]
    id: Option<Value>,
    method: Option<String>,
    params: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    result: Option<Value>,
    error: Option<ErrorObject>,
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = Wire::deserialize(deserializer)?;
        if wire.jsonrpc.as_deref() != Some(JSONRPC_VERSION) {
            return Err(D::Error::custom("`jsonrpc` must be \"2.0\""));
        }
        let id = match wire.id {
            None => None,
            Some(Value::Null) => Some(None),
            Some(value) => Some(Some(
                RequestId::deserialize(value).map_err(D::Error::custom)?,
            )),
        };
        match (wire.method, id) {
            (Some(method), Some(Some(id))) => Ok(Self::Request(Request {
                id,
                method,
                params: wire.params,
            })),
            (Some(_), Some(None)) => Err(D::Error::custom(
                "a request's `id` must be a number or a string",
            )),
            (Some(method), None) => Ok(Self::Notification(Notification {
                method,
                params: wire.params,
            })),
            (None, Some(id)) => {
                let result = match (wire.result, wire.error) {
                    (Some(result), None) => Ok(result),
                    (None, Some(error)) => Err(error),
                    (Some(_), Some(_)) => {
                        return Err(D::Error::custom(
                            "a response has `result` or `error`, not both",
                        ));
                    }
                    (None, None) => {
                        return Err(D::Error::custom("a response needs `result` or `error`"));
                    }
                };
                Ok(Self::Response(Response { id, result }))
            }
            (None, None) => Err(D::Error::custom(
                "a message needs `method`, or `id` with `result` or `error`",
            )),
        }
    }
}

/// Reads one kind of message, with an error that names the kind found instead.
macro_rules! deserialize_as {
    ($name:ident, $variant:ident, $expected:literal) => {
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                match Message::deserialize(deserializer)? {
                    Message::$variant(message) => Ok(message),
                    other => Err(D::Error::custom(format_args!(
                        "expected {}, found {}",
                        $expected,
                        other.kind()
                    ))),
                }
            }
        }
    };
}

deserialize_as!(Request, Request, "a request");
deserialize_as!(Notification, Notification, "a notification");
deserialize_as!(Response, Response, "a response");

impl Message {
    /// "a request", "a notification" or "a response".
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Request(_) => "a request",
            Self::Notification(_) => "a notification",
            Self::Response(_) => "a response",
        }
    }
}

/// In the schema the envelopes are named `JsonRpc…`, because the
/// conversation model has a `Message` of its own.
#[cfg(feature = "schema")]
mod schema {
    use std::borrow::Cow;

    use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};

    use super::{ErrorObject, Message, Notification, Request, RequestId, Response};

    impl JsonSchema for RequestId {
        fn schema_name() -> Cow<'static, str> {
            "RequestId".into()
        }
        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            json_schema!({
                "description": "The id that ties a response to its request: a number or a string, chosen by the side that sends the request.",
                "type": ["integer", "string"],
            })
        }
    }

    impl JsonSchema for ErrorObject {
        fn schema_name() -> Cow<'static, str> {
            "JsonRpcError".into()
        }
        fn json_schema(generator: &mut SchemaGenerator) -> Schema {
            let data = generator.subschema_for::<crate::error::ErrorData>();
            json_schema!({
                "description": "The error member of a failed response.",
                "type": "object",
                "properties": {
                    "code": {
                        "type": "integer",
                        "description": "A JSON-RPC error code. -32000 for every application error.",
                    },
                    "message": { "type": "string", "description": "One sentence for a person." },
                    "data": {
                        "description": "For an application error (code -32000), an ErrorData. Otherwise optional detail.",
                        "anyOf": [data, true],
                    },
                },
                "required": ["code", "message"],
            })
        }
    }

    impl JsonSchema for Request {
        fn schema_name() -> Cow<'static, str> {
            "JsonRpcRequest".into()
        }
        fn json_schema(generator: &mut SchemaGenerator) -> Schema {
            let id = generator.subschema_for::<RequestId>();
            json_schema!({
                "description": "A JSON-RPC request: the receiver must answer it with a response that has the same id.",
                "type": "object",
                "properties": {
                    "jsonrpc": { "const": "2.0" },
                    "id": id,
                    "method": { "type": "string" },
                    "params": true,
                },
                "required": ["jsonrpc", "id", "method"],
            })
        }
    }

    impl JsonSchema for Notification {
        fn schema_name() -> Cow<'static, str> {
            "JsonRpcNotification".into()
        }
        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            json_schema!({
                "description": "A JSON-RPC notification: a message that gets no answer.",
                "type": "object",
                "properties": {
                    "jsonrpc": { "const": "2.0" },
                    "method": { "type": "string" },
                    "params": true,
                },
                "required": ["jsonrpc", "method"],
            })
        }
    }

    impl JsonSchema for Response {
        fn schema_name() -> Cow<'static, str> {
            "JsonRpcResponse".into()
        }
        fn json_schema(generator: &mut SchemaGenerator) -> Schema {
            let id = generator.subschema_for::<RequestId>();
            let error = generator.subschema_for::<ErrorObject>();
            json_schema!({
                "description": "The answer to a JSON-RPC request. It has `result` or `error`, never both.",
                "type": "object",
                "properties": {
                    "jsonrpc": { "const": "2.0" },
                    "id": { "anyOf": [id, { "type": "null" }] },
                    "result": true,
                    "error": error,
                },
                "required": ["jsonrpc", "id"],
            })
        }
    }

    impl JsonSchema for Message {
        fn schema_name() -> Cow<'static, str> {
            "JsonRpcMessage".into()
        }
        fn json_schema(generator: &mut SchemaGenerator) -> Schema {
            let request = generator.subschema_for::<Request>();
            let notification = generator.subschema_for::<Notification>();
            let response = generator.subschema_for::<Response>();
            json_schema!({
                "description": "Any JSON-RPC message: one JSON object per line.",
                "anyOf": [request, notification, response],
            })
        }
    }
}
