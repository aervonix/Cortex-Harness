//! The provider abstraction: one way for the agent loop to talk to any model
//! (`docs/architecture/04-providers.md`).
//!
//! The design keeps three things apart. A **protocol** is a wire dialect,
//! and is code. A **provider** is an endpoint that speaks one, and is data.
//! A **model** is a catalog entry, and is data. This crate has the parts
//! every protocol shares and no protocol of its own yet: each arrives with
//! its own build step.
//!
//! | Module | What it gives |
//! |---|---|
//! | [`request`] | [`ModelRequest`]: one call to a model, in neutral terms. |
//! | [`event`] | [`ModelEvent`], the events of a response; [`Usage`]; and [`EventSink`], where a decoder writes what it reads. |
//! | [`stream`] | [`ModelStream`], and streams written as ordinary async code. |
//! | [`protocol`] | The [`Protocol`] and [`StreamDecoder`] traits, and [`ProviderInfo`]. |
//! | [`transport`] | The [`Transport`] trait, and the real HTTP transport behind the `http` feature. |
//! | [`http`] | [`HttpRequest`], [`HttpResponse`] and [`Headers`]: plain data. |
//! | [`auth`] | The [`AuthMethod`] trait and the API-key method. |
//! | [`sse`] | [`SseParser`], the parser for server-sent events. |
//! | [`error`] | [`ProviderError`] and the shared classification rules. |
//! | [`retry`] | [`RetryPolicy`]: when to wait, when to fall back, when to give up. |
//! | [`speed`] | [`SpeedCooldowns`]: the cooldowns of speed tiers. |
//! | [`client`] | The [`ModelClient`] trait, and [`ProviderClient`], which puts the parts together. |
//! | [`usage`] | Usage normalisation and cost. |
//! | [`catalog`] | [`Catalog`], the model catalog. |
//! | [`time`] | The [`Timer`] trait behind every wait. |
//!
//! # What does I/O
//!
//! Only a [`Transport`] does. Building a request, decoding a stream and
//! classifying a failure are pure functions tested from files. The retry
//! layer decides without sleeping. The client reads time from a
//! [`Clock`](cortex_base::Clock) and waits on a [`Timer`], so its tests run
//! against a fake clock and take no time.
//!
//! # Speed tiers
//!
//! A request carries the speed it asks for. Its usage carries the speed it
//! **ran at**, taken from what the response reports, and both when they
//! differ: "asked for fast, served standard" is always on the record
//! (`docs/decisions/0027-speed-tiers.md`). The client waits out a tier's
//! short refusal, falls back to standard on a long one and starts a
//! cooldown, and says each of these in the stream.

pub mod auth;
pub mod catalog;
pub mod client;
pub mod error;
pub mod event;
pub mod http;
pub mod protocol;
pub mod request;
pub mod retry;
pub mod speed;
pub mod sse;
pub mod stream;
pub mod time;
pub mod transport;
pub mod usage;

pub use auth::{ApiKeyAuth, AuthMethod, AuthSummary, KeyPlacement, NoAuth};
pub use catalog::{Catalog, CatalogError, ModelEntry, ToolSchemaStyle, Variants};
pub use client::{ModelClient, ProviderClient, ProviderClientBuilder};
pub use error::{
    AuthError, BuildError, DecodeError, ProviderError, Signal, Throttle, TransportError,
    TransportErrorKind,
};
pub use event::{
    BlockKind, CooldownChange, EventSink, ModelEvent, ReportedUsage, RetryInfo, SpeedMove, Usage,
};
pub use http::{BodyStream, Headers, HttpRequest, HttpResponse, Method};
pub use protocol::{FlagValue, Flags, HttpFailure, Protocol, ProviderInfo, StreamDecoder};
pub use request::{CacheHints, ModelRequest, Reasoning, ReasoningVisibility, ToolDefinition};
pub use retry::{Decision, RetryPolicy, RetryState, SpeedPolicy};
pub use speed::{CooldownStatus, SpeedCooldowns, SpeedInEffect};
pub use sse::{SseEvent, SseParser};
pub use stream::{Emitter, ModelStream};
#[cfg(feature = "http")]
pub use time::TokioTimer;
pub use time::{Sleep, Timer};
#[cfg(feature = "http")]
pub use transport::{HttpOptions, HttpTransport};
pub use transport::{BoxFuture, Transport};
