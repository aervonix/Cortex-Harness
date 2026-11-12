//! [`Secret`]: a credential that cannot reach a log or a session by accident
//! (`docs/security.md`, "Secrets in logs and session files").

use serde::Deserialize;

/// What a [`Secret`] prints as, wherever it is printed.
pub const REDACTED: &str = "[redacted]";

/// A credential: an API key, a token, a password.
///
/// - `Debug` and `Display` print `[redacted]`, in every format, so a secret
///   inside a struct that is logged, or inside an error message, stays out of
///   the output.
/// - It does **not** implement `Serialize`, so it cannot be written to a
///   session, an event or a configuration dump. A type that holds one cannot
///   derive `Serialize` either, which is the point: the field has to be
///   skipped on purpose.
/// - The only way to the text is [`Secret::expose`], which is easy to find in
///   review. The credential store and the code that builds an authorisation
///   header are the callers.
///
/// It does implement `Deserialize`, because credentials are read from files
/// and from the keychain. It has no `PartialEq`: compare exposed values where
/// that is really wanted. The memory is not wiped when a secret is dropped.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    /// Wraps a credential.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The credential itself. Every call is a place where it can leak: pass
    /// the result straight to what needs it, and never to a log, an error
    /// message or anything that is stored.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether the credential is the empty string, which no provider accepts.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "sk-test-4f9a0c17e2";

    /// A type that holds a secret, as a provider configuration will.
    #[derive(Debug, Clone)]
    struct Holder {
        name: &'static str,
        key: Secret,
        spare: Option<Secret>,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("the provider rejected the key {key}")]
    struct Rejected {
        key: Secret,
    }

    #[test]
    fn a_secret_cannot_be_printed() {
        let secret = Secret::new(KEY);
        let holder = Holder {
            name: "example",
            key: secret.clone(),
            spare: Some(Secret::from(KEY)),
        };
        let error = Rejected {
            key: Secret::from(KEY.to_owned()),
        };
        let printed = [
            format!("{secret}"),
            format!("{secret:?}"),
            format!("{secret:#?}"),
            format!("{secret:>40}"),
            format!("{secret:.3}"),
            secret.to_string(),
            format!("{holder:?}"),
            format!("{holder:#?}"),
            format!("{:?}", Some(&secret)),
            format!("{:?}", vec![secret.clone(), secret.clone()]),
            format!("{:?}", ("key", &secret)),
            format!("{error}"),
            format!("{error:?}"),
        ];
        for text in &printed {
            assert!(!text.contains(KEY), "the key was printed: {text}");
            assert!(!text.contains("sk-"), "part of the key was printed: {text}");
            assert!(text.contains(REDACTED), "no redaction marker in: {text}");
        }
        assert_eq!(holder.name, "example");
        assert_eq!(holder.key.expose(), KEY);
        assert!(holder.spare.is_some());
    }

    #[test]
    fn the_text_is_reached_only_by_asking_for_it() {
        let secret = Secret::new(KEY);
        assert_eq!(secret.expose(), KEY);
        assert!(!secret.is_empty());
        assert!(Secret::new("").is_empty());
    }

    #[test]
    fn a_secret_is_read_from_a_credentials_file() {
        let secret: Secret = serde_json::from_str("\"sk-test-4f9a0c17e2\"").unwrap();
        assert_eq!(secret.expose(), KEY);
        assert!(serde_json::from_str::<Secret>("17").is_err());
    }

    /// The proof that `Secret` cannot be serialised is made by the compiler.
    ///
    /// `Probe` is implemented for every type once, and a second time for
    /// every type that implements `Serialize`. For a type that does, the call
    /// below has two candidates and does not compile ("type annotations
    /// needed"). So this test compiling is the assertion: if `Serialize` is
    /// ever implemented or derived for `Secret`, the crate's tests stop
    /// building. `the_probe_is_real` shows the mechanism is sound by using it
    /// the other way round on a type that is serialisable.
    #[test]
    fn a_secret_cannot_be_serialised() {
        trait Probe<Marker> {
            fn not_serialisable() {}
        }
        struct ForEveryType;
        struct ForSerialisableTypes;
        impl<T: ?Sized> Probe<ForEveryType> for T {}
        impl<T: ?Sized + serde::Serialize> Probe<ForSerialisableTypes> for T {}

        // Compiles only while exactly one of the two implementations applies.
        <Secret as Probe<_>>::not_serialisable();
        // And the same holds for the types that would carry one.
        <Option<Secret> as Probe<_>>::not_serialisable();
        <Vec<Secret> as Probe<_>>::not_serialisable();
        <Holder as Probe<_>>::not_serialisable();
    }

    /// The other half of the proof: the same question asked a second way,
    /// which answers "yes" for `String` and "no" for `Secret`, so the probe
    /// above is not passing for a reason of its own. It relies on an
    /// inherent associated constant shadowing a trait's. The assertions are
    /// evaluated by the compiler too.
    #[test]
    fn the_probe_is_real() {
        struct Check<T: ?Sized>(std::marker::PhantomData<T>);
        trait Fallback {
            const SERIALISABLE: bool = false;
        }
        impl<T: ?Sized> Fallback for Check<T> {}
        #[allow(
            dead_code,
            reason = "chosen by name resolution only for serialisable types"
        )]
        impl<T: ?Sized + serde::Serialize> Check<T> {
            const SERIALISABLE: bool = true;
        }

        const { assert!(Check::<String>::SERIALISABLE) };
        const { assert!(Check::<Option<String>>::SERIALISABLE) };
        const { assert!(!Check::<Secret>::SERIALISABLE) };
        const { assert!(!Check::<Option<Secret>>::SERIALISABLE) };
        const { assert!(!Check::<Vec<Secret>>::SERIALISABLE) };
    }
}
