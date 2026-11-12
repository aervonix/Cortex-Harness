//! No absolute home-directory paths anywhere in the repository (rule 3 of
//! "Rules for every step" in `docs/build-plan.md`). They name a person and a
//! machine, and they do not work for anyone else.
//!
//! Every file is scanned as bytes, binary files included: image metadata and
//! recorded fixtures are where such a path is easiest to miss.

use super::Violation;

/// Roots of home directories on Linux and macOS.
const UNIX_ROOTS: [&str; 2] = ["/home/", "/Users/"];
/// The same on Windows, after the drive letter, with single and with escaped
/// separators.
const WINDOWS_ROOTS: [&str; 2] = [":\\Users\\", ":\\\\Users\\\\"];

pub(crate) fn check(file: &str, bytes: &[u8]) -> Vec<Violation> {
    find(bytes)
        .into_iter()
        .map(|(offset, path)| {
            let line = 1 + bytes[..offset]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count();
            Violation::new(
                file,
                Some(line),
                format!(
                    "absolute home-directory path `{path}`. Write `~` or a path relative to the \
                     repository (rule 3 in docs/build-plan.md)."
                ),
            )
        })
        .collect()
}

/// The home-directory paths in `bytes`: where each starts, and the text up to
/// the end of the user's directory name.
fn find(bytes: &[u8]) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for start in 0..bytes.len() {
        let rest = &bytes[start..];
        let previous = start.checked_sub(1).map(|index| bytes[index]);

        let root_len = if let Some(root) = UNIX_ROOTS
            .iter()
            .find(|root| rest.starts_with(root.as_bytes()))
        {
            // "/home/" in the middle of a longer path or a URL is something
            // else: "site.example/home/page", "/mnt/data/home/".
            if previous.is_some_and(is_name_byte) {
                continue;
            }
            root.len()
        } else if let Some(root) = WINDOWS_ROOTS
            .iter()
            .find(|root| starts_with_ignoring_case(rest, root))
        {
            if !previous.is_some_and(|byte| byte.is_ascii_alphabetic()) {
                continue;
            }
            root.len()
        } else {
            continue;
        };

        // A real path names a user next. "/home/<name>/" and "/home/$USER/"
        // are descriptions, not paths.
        let name_len = rest[root_len..]
            .iter()
            .take_while(|&&byte| is_name_byte(byte))
            .count();
        if name_len == 0 {
            continue;
        }
        let path = String::from_utf8_lossy(&rest[..root_len + name_len]).into_owned();
        // For the Windows form, report from the drive letter.
        if rest[0] == b':' {
            found.push((start - 1, format!("{}{path}", char::from(bytes[start - 1]))));
        } else {
            found.push((start, path));
        }
    }
    found
}

/// A byte that can be part of a user name or an ordinary file name.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
}

fn starts_with_ignoring_case(bytes: &[u8], prefix: &str) -> bool {
    bytes.len() >= prefix.len() && bytes[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::{check, find};

    // The paths these tests look for are assembled from pieces, so that this
    // file does not itself contain one.
    fn unix_home(root: &str, user: &str) -> String {
        format!("/{root}/{user}")
    }

    fn found(text: &str) -> Vec<String> {
        find(text.as_bytes())
            .into_iter()
            .map(|(_, path)| path)
            .collect()
    }

    #[test]
    fn a_linux_home_directory_is_found() {
        let home = unix_home("home", "ada");
        assert_eq!(found(&format!("cd {home}/project")), [home.as_str()]);
        assert_eq!(found(&format!("path = \"{home}\"")), [home.as_str()]);
        assert_eq!(found(&home), [home.as_str()]);
    }

    #[test]
    fn a_macos_home_directory_is_found() {
        let home = unix_home("Users", "ada.lovelace");
        assert_eq!(found(&format!("open {home}/Documents")), [home]);
    }

    #[test]
    fn a_windows_home_directory_is_found_in_every_spelling() {
        let backslashes = ["C:", "Users", "ada"].join("\\");
        let escaped = ["C:", "Users", "ada"].join("\\\\");
        let lower_case = ["d:", "users", "ada"].join("\\");
        let forward = ["C:", "Users", "ada"].join("/");

        assert_eq!(found(&format!("dir {backslashes}\\src")), [backslashes]);
        assert_eq!(
            found(&format!("{{\"cwd\": \"{escaped}\\\\src\"}}")),
            [escaped]
        );
        assert_eq!(found(&lower_case), [lower_case.as_str()]);
        // With forward slashes it is the Unix form after a drive letter.
        assert_eq!(found(&forward), [unix_home("Users", "ada")]);
    }

    #[test]
    fn it_is_found_in_a_binary_file() {
        let home = unix_home("home", "ada");
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0, 0xff, 0xfe];
        bytes.extend_from_slice(home.as_bytes());
        bytes.extend_from_slice(&[0, 0xff]);

        assert_eq!(find(&bytes), [(7, home)]);
    }

    #[test]
    fn the_violation_names_the_file_and_the_line() {
        let text = format!(
            "first line\nsecond line\nlog = \"{}/log\"\n",
            unix_home("home", "ada")
        );
        let violations = check("docs/example.md", text.as_bytes());

        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].file, "docs/example.md");
        assert_eq!(violations[0].line, Some(3));
        assert!(
            violations[0]
                .message
                .contains("absolute home-directory path")
        );
    }

    #[test]
    fn descriptions_and_other_paths_are_left_alone() {
        for text in [
            "~/project and ~/.config/cortex-harness",
            "$HOME/project",
            "crates/cortex-cli/src/main.rs",
            // A description of the pattern, with no user in it.
            "paths under /home/<name>/ or /home/$USER/ are rejected",
            "the root is /home/ on Linux",
            // Longer paths and URLs that merely contain the word.
            "https://site.example/home/page",
            "/mnt/data/home/archive",
            "the Users\\ folder",
            "see /usr/share/doc",
        ] {
            assert_eq!(found(text), Vec::<String>::new(), "in: {text}");
        }
    }
}
