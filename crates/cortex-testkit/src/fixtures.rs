//! Ready-made models and requests for tests.
//!
//! The models are made up. They exist to have every shape of speed tier in
//! one place: a `fast` tier that fails when it cannot be served and has a
//! limit and a cache of its own, a `priority` tier that the provider
//! downgrades by itself, and a model with no tier at all.

use std::sync::Arc;

use cortex_llm::{Catalog, ModelEntry, ModelRequest};
use cortex_protocol::model::ModelRole;
use cortex_protocol::{Message, ModelId, Part, ProviderId, Role};

/// The provider every fixture model belongs to.
pub const PROVIDER: &str = "test";
/// The name of the test protocol ([`TestProtocol`](crate::TestProtocol)).
pub const PROTOCOL: &str = "cortex-test";
/// A model with a `fast` tier and a `priority` tier.
pub const MODEL: &str = "test-model";
/// A model with no speed tier.
pub const PLAIN_MODEL: &str = "test-plain";

/// The text of the fixture catalog, in the catalog file's structure.
pub const CATALOG_JSON: &str = r#"{
  "version": "test-1",
  "model": [
    {
      "provider": "test", "id": "test-model", "name": "Test Model",
      "protocol": "cortex-test",
      "context_window": 200000, "max_output": 8000,
      "input": ["text", "image"],
      "prices": {
        "input": 4, "output": 20, "cache_read": 0.4,
        "cache_write": [{"class": "5m", "price": 5}, {"class": "1h", "price": 8}]
      },
      "reasoning": {"levels": ["low", "medium", "high"], "default": "medium"},
      "capabilities": {"strict_tools": true, "structured_output": true},
      "speed": [
        {
          "name": "fast", "label": "Fast mode", "effect": "faster_output",
          "when_unavailable": "error", "separate_limit": true, "shares_cache": false,
          "prices": {
            "input": 8, "output": 40, "cache_read": 0.8,
            "cache_write": [{"class": "5m", "price": 10}, {"class": "1h", "price": 16}]
          }
        },
        {
          "name": "priority", "label": "Priority processing", "effect": "priority",
          "when_unavailable": "downgrade",
          "prices": {"input": 7, "output": 35}
        }
      ]
    },
    {
      "provider": "test", "id": "test-plain", "name": "Test Plain",
      "protocol": "cortex-test",
      "context_window": 32000, "max_output": 4000,
      "input": ["text"],
      "prices": {"input": 1, "output": 2}
    }
  ]
}"#;

/// The fixture catalog.
#[must_use]
pub fn catalog() -> Catalog {
    Catalog::from_json(CATALOG_JSON).expect("the fixture catalog is valid")
}

/// The model with a `fast` and a `priority` tier.
#[must_use]
pub fn model() -> Arc<ModelEntry> {
    entry(MODEL)
}

/// The model with no speed tier.
#[must_use]
pub fn plain_model() -> Arc<ModelEntry> {
    entry(PLAIN_MODEL)
}

fn entry(id: &str) -> Arc<ModelEntry> {
    let catalog = catalog();
    let entry = catalog
        .get(&ProviderId::new(PROVIDER), &ModelId::new(id))
        .expect("the fixture catalog has the model");
    Arc::clone(entry)
}

/// A request to `model` in the `main` role, with a system prompt and one
/// user message, at standard speed.
#[must_use]
pub fn request(model: Arc<ModelEntry>) -> ModelRequest {
    let mut request = ModelRequest::new(model, ModelRole::Main);
    request.system = "You are a test.".to_owned();
    request.messages = vec![user("Hello")];
    request
}

/// A user message with one text part.
#[must_use]
pub fn user(text: &str) -> Message {
    Message {
        role: Role::User,
        parts: vec![Part::text(text)],
        origin: None,
    }
}
