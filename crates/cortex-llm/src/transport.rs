//! The transport: the one part of this crate that does I/O
//! (`docs/architecture/04-providers.md`, "Traits" and "Transport").
//!
//! A [`Transport`] sends one HTTP request and hands back the response as it
//! arrives. Everything above it (building the request, decoding the stream,
//! retrying) is tested against a scripted transport from `cortex-testkit`.
//! The real one, [`HttpTransport`], is behind the `http` feature and is
//! tested against the scripted SSE server.

use std::pin::Pin;

use cortex_base::CancelToken;

use crate::error::TransportError;
use crate::http::{HttpRequest, HttpResponse};

/// A boxed future, as the object-safe traits of this crate return them.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Sends a request and streams the response body.
pub trait Transport: Send + Sync + std::fmt::Debug {
    /// Sends `request`. The future completes when the status and headers
    /// have arrived, with **any** status: a 429 or a 500 is a response, not
    /// a transport error.
    ///
    /// Cancelling `cancel` ends the send, and ends the body with a
    /// [`Cancelled`](crate::TransportErrorKind::Cancelled) error. Dropping
    /// the future or the response body does the same without a word: the
    /// connection is closed, which is the one cancellation signal every
    /// provider honours.
    ///
    /// A transport has no time limits of its own beyond the one for making
    /// a connection. How long to wait for the first event, and how long a
    /// stream may be silent, is decided above it, by the client.
    fn send(
        &self,
        request: HttpRequest,
        cancel: CancelToken,
    ) -> BoxFuture<'_, Result<HttpResponse, TransportError>>;
}

#[cfg(feature = "http")]
pub use real::{HttpOptions, HttpTransport};

#[cfg(feature = "http")]
mod real {
    use std::sync::OnceLock;
    use std::time::Duration;

    use cortex_base::CancelToken;
    use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

    use super::{BoxFuture, Transport};
    use crate::error::{TransportError, TransportErrorKind};
    use crate::http::{BodyStream, Headers, HttpRequest, HttpResponse, Method};

    /// Settings of the HTTP client.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct HttpOptions {
        /// The longest wait for a connection to be made.
        pub connect_timeout: Duration,
        /// The `user-agent` header.
        pub user_agent: String,
        /// Certificate authorities to trust besides the system's, each as
        /// PEM text: for a proxy or a private endpoint.
        pub extra_roots_pem: Vec<Vec<u8>>,
    }

    impl Default for HttpOptions {
        fn default() -> Self {
            Self {
                connect_timeout: Duration::from_secs(30),
                user_agent: concat!("cortex-harness/", env!("CARGO_PKG_VERSION")).to_owned(),
                extra_roots_pem: Vec::new(),
            }
        }
    }

    /// The real transport: one pooled HTTP client for the process.
    ///
    /// **The client is created on first use, not here.** Creating it loads
    /// the system's certificate store, which is the most expensive thing the
    /// harness does before its first request, so it must not be on the
    /// start-up path. [`HttpTransport::new`] costs nothing. The first
    /// [`send`](Transport::send) creates the client, or
    /// [`warm`](Self::warm) does, from a background task, so that the first
    /// request does not pay for it.
    ///
    /// TLS is rustls with the ring provider and the system's certificate
    /// verifier (`docs/decisions/0004-http-and-tls.md`). Proxies come from
    /// the standard environment variables. Redirects are not followed: a
    /// model endpoint has no reason to redirect, and following one would
    /// carry the credential headers to wherever it points.
    #[derive(Debug)]
    pub struct HttpTransport {
        options: HttpOptions,
        client: OnceLock<Result<reqwest::Client, TransportError>>,
    }

    impl HttpTransport {
        /// A transport with these settings. No client exists yet.
        #[must_use]
        pub fn new(options: HttpOptions) -> Self {
            Self {
                options,
                client: OnceLock::new(),
            }
        }

        /// Whether the client has been created.
        #[must_use]
        pub fn is_created(&self) -> bool {
            self.client.get().is_some()
        }

        /// Creates the client now, if it does not exist. It blocks for as
        /// long as loading the certificate store takes, so call it on the
        /// blocking pool or from a background task.
        pub fn warm(&self) -> Result<(), TransportError> {
            self.client().map(|_| ())
        }

        fn client(&self) -> Result<&reqwest::Client, TransportError> {
            self.client
                .get_or_init(|| create(&self.options))
                .as_ref()
                .map_err(Clone::clone)
        }
    }

    impl Default for HttpTransport {
        fn default() -> Self {
            Self::new(HttpOptions::default())
        }
    }

    fn create(options: &HttpOptions) -> Result<reqwest::Client, TransportError> {
        // The process-wide TLS provider. An error means one is installed
        // already, which is what is wanted.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let mut builder = reqwest::Client::builder()
            .connect_timeout(options.connect_timeout)
            .user_agent(options.user_agent.as_str())
            .redirect(reqwest::redirect::Policy::none());
        for pem in &options.extra_roots_pem {
            let certificate = reqwest::Certificate::from_pem(pem).map_err(|error| {
                TransportError::new(
                    TransportErrorKind::InvalidRequest,
                    format!("a configured certificate authority could not be read: {error}"),
                )
            })?;
            builder = builder.add_root_certificate(certificate);
        }
        builder.build().map_err(|error| {
            TransportError::new(
                TransportErrorKind::InvalidRequest,
                format!("the HTTP client could not be created: {}", describe(&error)),
            )
        })
    }

    impl Transport for HttpTransport {
        fn send(
            &self,
            request: HttpRequest,
            cancel: CancelToken,
        ) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
            Box::pin(async move {
                let client = self.client()?;
                let method = match request.method {
                    Method::Get => reqwest::Method::GET,
                    Method::Post => reqwest::Method::POST,
                };
                let sending = client
                    .request(method, request.url.as_str())
                    .headers(header_map(&request.headers)?)
                    .body(request.body)
                    .send();
                let mut response = match cancel.race(sending).await {
                    None => return Err(cancelled()),
                    Some(Err(error)) => return Err(failure(&error, false)),
                    Some(Ok(response)) => response,
                };

                let status = response.status().as_u16();
                let mut headers = Headers::new();
                for (name, value) in response.headers() {
                    // A value that is not text cannot be one this crate reads.
                    if let Ok(value) = value.to_str() {
                        headers.insert(name.as_str(), value);
                    }
                }
                let body = BodyStream::generate(move |out| async move {
                    loop {
                        match cancel.race(response.chunk()).await {
                            None => {
                                out.emit(Err(cancelled())).await;
                                return;
                            }
                            Some(Ok(Some(chunk))) => out.emit(Ok(Vec::from(chunk))).await,
                            Some(Ok(None)) => return,
                            Some(Err(error)) => {
                                out.emit(Err(failure(&error, true))).await;
                                return;
                            }
                        }
                    }
                });
                Ok(HttpResponse {
                    status,
                    headers,
                    body,
                })
            })
        }
    }

    fn header_map(headers: &Headers) -> Result<HeaderMap, TransportError> {
        let mut map = HeaderMap::with_capacity(headers.len());
        for (name, value) in headers.iter() {
            let invalid = || {
                // The name is safe to show. The value may be a credential.
                TransportError::new(
                    TransportErrorKind::InvalidRequest,
                    format!("the header `{name}` cannot be sent: its name or value is not valid"),
                )
            };
            let header = HeaderName::from_bytes(name.as_bytes()).map_err(|_| invalid())?;
            let mut value = HeaderValue::from_str(value).map_err(|_| invalid())?;
            value.set_sensitive(headers.is_sensitive(name));
            map.insert(header, value);
        }
        Ok(map)
    }

    fn cancelled() -> TransportError {
        TransportError::new(TransportErrorKind::Cancelled, "The request was cancelled.")
    }

    /// Classifies a client error by its type, never by its text.
    fn failure(error: &reqwest::Error, response_started: bool) -> TransportError {
        let kind = if error.is_builder() {
            TransportErrorKind::InvalidRequest
        } else if error.is_timeout() {
            TransportErrorKind::Timeout
        } else if response_started || error.is_body() || error.is_decode() {
            TransportErrorKind::Body
        } else {
            TransportErrorKind::Connect
        };
        TransportError::new(kind, describe(error))
    }

    /// The error and its causes in one line, without the URL: a URL can
    /// carry a credential in its query.
    fn describe(error: &reqwest::Error) -> String {
        use std::error::Error as _;

        let mut text = if error.is_builder() {
            "the request could not be built".to_owned()
        } else if error.is_timeout() {
            "the request timed out".to_owned()
        } else if error.is_connect() {
            "the connection could not be made".to_owned()
        } else if error.is_body() || error.is_decode() {
            "the response body could not be read".to_owned()
        } else {
            "the request failed".to_owned()
        };
        let mut cause = error.source();
        while let Some(inner) = cause {
            text.push_str(": ");
            text.push_str(&inner.to_string());
            cause = inner.source();
        }
        text
    }
}
