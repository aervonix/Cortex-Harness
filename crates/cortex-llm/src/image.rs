//! Images that are stored as artifacts, and how they reach a request
//! (`docs/architecture/04-providers.md`, "Images").
//!
//! A tool that reads an image does not put the image in the conversation.
//! It stores it and returns a reference: an
//! [`ImagePart`](cortex_protocol::ImagePart) with an `artifact` and no
//! `data`. The conversation, the session log and every front end carry the
//! reference, which is a few bytes.
//!
//! A provider needs the image itself. A [`Protocol`](crate::Protocol) is
//! pure and reads no files, and this crate cannot see the crate that owns
//! the store. So the store is reached through a trait, [`ImageSource`],
//! which the place that assembles the core implements over the session's
//! content store and gives to the client
//! ([`ProviderClientBuilder::images`](crate::ProviderClientBuilder::images)).
//!
//! Before a request is built, the client replaces every reference in it
//! with the image's bytes in base64, in its own copy of the request. The
//! conversation the loop holds is not changed. An artifact's content never
//! changes, so the same reference gives the same bytes in every request,
//! and the request's prefix stays the same.

use cortex_base::{CancelToken, Classified, ErrorKind};
use cortex_protocol::{ArtifactId, Part};

use crate::error::ProviderError;
use crate::request::ModelRequest;
use crate::transport::BoxFuture;

/// The code of the error a request ends with when an image it refers to
/// cannot be loaded.
pub const IMAGE_UNAVAILABLE: &str = "image_unavailable";

/// Where the bytes of a stored image come from.
///
/// Implemented where the core is assembled, over the content store of the
/// session. An implementation may keep what it has read: an artifact's
/// content never changes.
pub trait ImageSource: Send + Sync + std::fmt::Debug {
    /// The bytes of the image stored as `artifact`, as the file they came
    /// from held them: not decoded, not resized.
    fn load<'a>(&'a self, artifact: &'a ArtifactId) -> BoxFuture<'a, Result<Vec<u8>, ImageError>>;
}

/// A stored image could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ImageError {
    /// What went wrong, for a person.
    pub message: String,
}

impl ImageError {
    /// A failure to load an image.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Classified for ImageError {
    /// The conversation refers to something the harness itself stored and
    /// can no longer read.
    fn kind(&self) -> ErrorKind {
        ErrorKind::Internal
    }

    fn code(&self) -> &str {
        IMAGE_UNAVAILABLE
    }
}

/// Bytes as base64 text, with padding: the form a request carries an image
/// in.
#[must_use]
pub fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut push = |six: u32| out.push(char::from(ALPHABET[(six & 0x3f) as usize]));
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = chunk.get(1).copied().map_or(0, u32::from);
        let third = chunk.get(2).copied().map_or(0, u32::from);
        let group = (first << 16) | (second << 8) | third;
        push(group >> 18);
        push(group >> 12);
        if chunk.len() > 1 {
            push(group >> 6);
        }
        if chunk.len() > 2 {
            push(group);
        }
    }
    while !out.len().is_multiple_of(4) {
        out.push('=');
    }
    out
}

/// What became of the images of a request.
pub(crate) enum Inlined {
    /// Every reference now holds its image, or there were none.
    Done,
    /// The request was cancelled while an image was being loaded.
    Cancelled,
    /// An image could not be loaded.
    Failed(ProviderError),
}

/// Replaces every reference to a stored image in `request` with the image's
/// bytes. Without a source, references are left as they are, and the
/// protocol refuses the request when it meets one.
pub(crate) async fn inline_images(
    request: &mut ModelRequest,
    source: Option<&dyn ImageSource>,
    cancel: &CancelToken,
) -> Inlined {
    let Some(source) = source else {
        return Inlined::Done;
    };
    for message in &mut request.messages {
        for part in &mut message.parts {
            let nested = match part {
                Part::ToolResult(result) => result.content.as_mut_slice(),
                other => std::slice::from_mut(other),
            };
            for part in nested {
                let Part::Image(image) = part else { continue };
                if image.data.is_some() {
                    continue;
                }
                let Some(artifact) = image.artifact.take() else {
                    continue;
                };
                match cancel.race(source.load(&artifact)).await {
                    None => return Inlined::Cancelled,
                    Some(Ok(bytes)) => image.data = Some(encode_base64(&bytes)),
                    Some(Err(error)) => {
                        return Inlined::Failed(ProviderError::new(
                            error.kind(),
                            IMAGE_UNAVAILABLE,
                            format!(
                                "The image stored as `{artifact}` could not be loaded: {error}"
                            ),
                        ));
                    }
                }
            }
        }
    }
    Inlined::Done
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_examples_of_its_standard() {
        // RFC 4648, section 10.
        let rows = [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ];
        for (plain, encoded) in rows {
            assert_eq!(encode_base64(plain.as_bytes()), encoded, "{plain:?}");
        }
        // Every value of a byte, and both characters past the letters and
        // digits.
        assert_eq!(encode_base64(&[0xfb, 0xff, 0xfe]), "+//+");
        assert_eq!(encode_base64(&[0x00, 0x00, 0x00]), "AAAA");
        let all: Vec<u8> = (0..=255).collect();
        let encoded = encode_base64(&all);
        assert_eq!(encoded.len(), 344);
        assert!(encoded.starts_with("AAECAwQFBgcICQoLDA0ODxAR"));
        assert!(encoded.ends_with("+fr7/P3+/w=="));
    }

    #[test]
    fn an_image_that_cannot_be_loaded_is_the_harness_s_failure() {
        let error = ImageError::new("the artifact is gone");
        assert_eq!(error.kind(), ErrorKind::Internal);
        assert_eq!(error.code(), IMAGE_UNAVAILABLE);
        assert_eq!(error.to_string(), "the artifact is gone");
    }
}
