//! Files that look like secrets
//! (`docs/architecture/06-permissions-and-sandbox.md`, "Rules";
//! `docs/security.md`, section 4).
//!
//! Reading or writing one of these asks first, at every level short of
//! `unrestricted`. It is a **default**, not a rule: it is what happens when
//! no rule has settled the question. That matters because `ask` beats
//! `allow`: as a built-in rule, nothing a user wrote could let the agent
//! read a `.env.example` it has good reason to read. As a default, an allow
//! rule for that file lifts it, and a deny rule still closes it for good.
//!
//! The same default is what keeps such files out of a session's base state:
//! the session store keeps only what the engine would let the agent read
//! without asking.
//!
//! The list is by name, and it is short on purpose. A name is all there is
//! to go on before a file is read, and a long list would ask about ordinary
//! source files. (An earlier list had `id_*`, for SSH keys, and so asked
//! about `src/id_map.rs`.)

/// The endings of an environment file's name that mark a template, which is
/// committed and holds no secret: `.env.example`.
const TEMPLATE_ENDINGS: [&str; 5] = [".example", ".sample", ".template", ".dist", ".defaults"];

/// The file extensions of private keys and key stores.
const KEY_EXTENSIONS: [&str; 5] = [".pem", ".key", ".p12", ".pfx", ".jks"];

/// The names OpenSSH gives a private key.
const SSH_KEYS: [&str; 6] = [
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ecdsa_sk",
    "id_ed25519",
    "id_ed25519_sk",
];

/// Files that hold a login for other programs.
const LOGIN_FILES: [&str; 6] = [
    ".netrc",
    "_netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    ".envrc",
];

/// Whether a file's name says it is likely to hold a secret.
///
/// - Environment files: `.env` and `.env.<anything>`, except templates
///   (`.env.example`, `.env.sample`, `.env.template`, `.env.dist`,
///   `.env.defaults`).
/// - Private keys and key stores: `*.pem`, `*.key`, `*.p12`, `*.pfx`,
///   `*.jks`.
/// - SSH private keys by their usual names: `id_rsa`, `id_dsa`, `id_ecdsa`,
///   `id_ecdsa_sk`, `id_ed25519`, `id_ed25519_sk`. Not `id_rsa.pub`, and
///   not any other name that begins `id_`.
/// - Files that hold a login or a token for other programs: `.netrc`,
///   `_netrc`, `.npmrc`, `.pypirc`, `.git-credentials`, and `.envrc`, which
///   a shell hook loads into the environment.
///
/// The name is compared without regard to ASCII case on every OS: matching
/// too much here only means asking once more.
#[must_use]
pub fn looks_secret(file_name: &str) -> bool {
    let name = file_name.to_ascii_lowercase();
    if name == ".env" {
        return true;
    }
    if let Some(rest) = name.strip_prefix(".env.") {
        return !TEMPLATE_ENDINGS
            .iter()
            .any(|ending| format!(".{rest}").ends_with(ending));
    }
    KEY_EXTENSIONS.iter().any(|ending| name.ends_with(ending))
        || SSH_KEYS.contains(&name.as_str())
        || LOGIN_FILES.contains(&name.as_str())
}

/// The names [`looks_secret`] recognises, as patterns for one name: for
/// asking whether a pattern a shell expands could match one. Wider than
/// the function, which is the safe side: a template's name
/// (`.env.example`) matches `.env.*` here.
pub(crate) fn secret_patterns() -> impl Iterator<Item = String> {
    [".env".to_owned(), ".env.*".to_owned()]
        .into_iter()
        .chain(KEY_EXTENSIONS.iter().map(|ending| format!("*{ending}")))
        .chain(SSH_KEYS.iter().map(|name| (*name).to_owned()))
        .chain(LOGIN_FILES.iter().map(|name| (*name).to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_recognised_by_name() {
        for secret in [
            ".env",
            ".ENV",
            ".env.local",
            ".env.production",
            ".env.example.local",
            "server.pem",
            "TLS.KEY",
            "store.p12",
            "cert.pfx",
            "release.jks",
            "id_rsa",
            "id_ed25519",
            "id_ed25519_sk",
            "id_ecdsa",
            ".netrc",
            "_netrc",
            ".npmrc",
            ".pypirc",
            ".git-credentials",
            ".envrc",
        ] {
            assert!(looks_secret(secret), "{secret}");
        }
    }

    #[test]
    fn ordinary_files_are_not() {
        for ordinary in [
            // The names the earlier list wrongly caught.
            "id_map.rs",
            "id_generator.py",
            "id_rsa.pub",
            "id_ed25519.pub",
            // Templates, which are committed.
            ".env.example",
            ".env.sample",
            ".env.template",
            ".env.dist",
            ".env.defaults",
            ".env.local.example",
            // Names that only resemble the list.
            "env.rs",
            ".environment",
            "environment.md",
            "keys.rs",
            "monkey.txt",
            "pem-files.md",
            "netrc",
            "Cargo.toml",
            "",
        ] {
            assert!(!looks_secret(ordinary), "{ordinary}");
        }
    }
}
