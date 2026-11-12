//! Credentials on requests (`docs/architecture/04-providers.md`,
//! "Credentials").
//!
//! An [`AuthMethod`] adds a credential to a request that a protocol has
//! built. It is the last thing that happens before the request is sent, so
//! nothing that logs or stores a request ever holds a credential, and what
//! it adds is marked sensitive, so that it never prints.
//!
//! The first release has one method, an API key ([`ApiKeyAuth`]). A sign-in
//! flow is another implementation of the same trait.

use cortex_base::Secret;

use crate::error::AuthError;
use crate::http::HttpRequest;
use crate::transport::BoxFuture;

/// A way to authorise requests to one provider.
pub trait AuthMethod: Send + Sync + std::fmt::Debug {
    /// Adds the credential to one request. May refresh a token first, which
    /// is why it is async.
    fn authorize<'a>(
        &'a self,
        request: &'a mut HttpRequest,
    ) -> BoxFuture<'a, Result<(), AuthError>>;

    /// What `cortex auth status` shows. Never the secret.
    fn describe(&self) -> AuthSummary;
}

/// What can be said about a credential without showing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSummary {
    /// The kind of credential: `api_key`, or `none`.
    pub method: String,
    /// Where it came from, in words: a flag, the name of an environment
    /// variable, the credential store.
    pub source: String,
    /// Whether there is a credential at all.
    pub present: bool,
}

/// How a protocol wants an API key sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyPlacement {
    /// As the whole value of a header: `x-api-key: <key>`.
    Header(String),
    /// As a bearer token: `authorization: Bearer <key>`.
    Bearer,
}

/// An API key, sent the way the protocol wants it.
#[derive(Debug, Clone)]
pub struct ApiKeyAuth {
    key: Secret,
    placement: KeyPlacement,
    source: String,
}

impl ApiKeyAuth {
    /// A key, where it goes on a request, and where it came from in words
    /// (`environment variable ANTHROPIC_API_KEY`).
    #[must_use]
    pub fn new(key: Secret, placement: KeyPlacement, source: impl Into<String>) -> Self {
        Self {
            key,
            placement,
            source: source.into(),
        }
    }
}

impl AuthMethod for ApiKeyAuth {
    fn authorize<'a>(
        &'a self,
        request: &'a mut HttpRequest,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(async move {
            if self.key.is_empty() {
                return Err(AuthError::new(format!(
                    "The API key from {} is empty.",
                    self.source
                )));
            }
            match &self.placement {
                KeyPlacement::Header(name) => request.headers.insert_secret(name, &self.key),
                KeyPlacement::Bearer => {
                    request.headers.insert_secret_with_prefix(
                        "authorization",
                        "Bearer ",
                        &self.key,
                    );
                }
            }
            Ok(())
        })
    }

    fn describe(&self) -> AuthSummary {
        AuthSummary {
            method: "api_key".to_owned(),
            source: self.source.clone(),
            present: !self.key.is_empty(),
        }
    }
}

/// No credential: for a local server that asks for none.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoAuth;

impl AuthMethod for NoAuth {
    fn authorize<'a>(
        &'a self,
        _request: &'a mut HttpRequest,
    ) -> BoxFuture<'a, Result<(), AuthError>> {
        Box::pin(std::future::ready(Ok(())))
    }

    fn describe(&self) -> AuthSummary {
        AuthSummary {
            method: "none".to_owned(),
            source: "no credential is configured".to_owned(),
            present: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Poll, Waker};

    use super::*;

    fn now<T>(future: BoxFuture<'_, T>) -> T {
        let mut future = future;
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("an API key needs no waiting"),
        }
    }

    fn request() -> HttpRequest {
        HttpRequest::post("https://example.invalid/v1/messages", b"{}".to_vec())
    }

    #[test]
    fn a_key_goes_where_the_protocol_wants_it_and_never_prints() {
        let key = Secret::new("sk-test-abc123");

        let auth = ApiKeyAuth::new(
            key.clone(),
            KeyPlacement::Header("x-api-key".to_owned()),
            "environment variable EXAMPLE_API_KEY",
        );
        let mut first = request();
        now(auth.authorize(&mut first)).unwrap();
        assert_eq!(first.headers.get("x-api-key"), Some("sk-test-abc123"));
        assert!(first.headers.is_sensitive("x-api-key"));

        let auth = ApiKeyAuth::new(key, KeyPlacement::Bearer, "the credential store");
        let mut second = request();
        now(auth.authorize(&mut second)).unwrap();
        assert_eq!(
            second.headers.get("authorization"),
            Some("Bearer sk-test-abc123")
        );

        for shown in [
            format!("{first:?}"),
            format!("{second:?}"),
            format!("{auth:?}"),
        ] {
            assert!(!shown.contains("abc123"), "{shown}");
        }
        assert_eq!(
            auth.describe(),
            AuthSummary {
                method: "api_key".to_owned(),
                source: "the credential store".to_owned(),
                present: true,
            }
        );
    }

    #[test]
    fn an_empty_key_is_refused_before_anything_is_sent() {
        let auth = ApiKeyAuth::new(Secret::new(""), KeyPlacement::Bearer, "the flag --api-key");
        let mut request = request();
        let error = now(auth.authorize(&mut request)).unwrap_err();
        assert!(error.message.contains("--api-key"), "{error}");
        assert!(request.headers.is_empty());
        assert!(!auth.describe().present);
    }

    #[test]
    fn no_credential_adds_nothing() {
        let mut request = request();
        now(NoAuth.authorize(&mut request)).unwrap();
        assert!(request.headers.is_empty());
        assert_eq!(NoAuth.describe().method, "none");
    }
}
