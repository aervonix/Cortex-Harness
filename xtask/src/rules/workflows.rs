//! Rules for the GitHub Actions workflows (`docs/testing-and-ci.md`, "CI
//! layout"): actions are pinned by commit hash, every workflow states its
//! token permissions, and the blocking workflow has no secrets and can write
//! nothing.

use super::Violation;

/// The workflow that blocks pull requests.
const BLOCKING: &str = ".github/workflows/ci.yml";

pub(crate) fn check(file: &str, text: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    let blocking = file == BLOCKING;
    let mut states_permissions = false;

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = Some(index + 1);
        // Comments are not rules.
        let line = raw_line.split(" #").next().unwrap_or(raw_line);
        if line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with("permissions:") {
            states_permissions = true;
        }

        if let Some(action) = used_action(line)
            && !action.starts_with("./")
            && !is_pinned(action)
        {
            violations.push(Violation::new(
                file,
                line_number,
                format!(
                    "`{action}` is not pinned by commit hash. Write `owner/action@<40-character \
                     commit hash> # vX.Y.Z`: a tag can be moved to different code, a hash cannot."
                ),
            ));
        }

        if blocking && line.contains("secrets.") {
            violations.push(Violation::new(
                file,
                line_number,
                "the blocking workflow uses no secrets. Pull-request jobs run code from the pull \
                 request, and nothing that blocks may need a paid API or a credential \
                 (rule 9 in docs/build-plan.md).",
            ));
        }
        if blocking && line.trim_end().ends_with(": write") {
            violations.push(Violation::new(
                file,
                line_number,
                "the blocking workflow's token is read-only. Work that must write belongs in a \
                 workflow that does not run on pull requests.",
            ));
        }
    }

    if !states_permissions {
        violations.push(Violation::new(
            file,
            None,
            "a workflow states its token permissions at the top level, for example:\n    \
             permissions:\n      contents: read",
        ));
    }
    violations
}

/// The action named by a `uses:` line, if the line is one.
fn used_action(line: &str) -> Option<&str> {
    let line = line.trim_start().trim_start_matches("- ").trim_start();
    let action = line.strip_prefix("uses:")?.trim();
    Some(action.trim_matches(['"', '\'']))
}

/// `owner/action@` followed by a full 40-character commit hash.
fn is_pinned(action: &str) -> bool {
    action.rsplit_once('@').is_some_and(|(_, reference)| {
        reference.len() == 40 && reference.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use super::check;

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    fn messages(file: &str, text: &str) -> Vec<String> {
        check(file, text)
            .into_iter()
            .map(|violation| violation.to_string())
            .collect()
    }

    fn workflow(steps: &str) -> String {
        format!("name: ci\npermissions:\n  contents: read\njobs:\n  lint:\n    steps:\n{steps}")
    }

    #[test]
    fn an_action_pinned_by_hash_passes() {
        let text = workflow(&format!(
            "      - uses: actions/checkout@{HASH} # v7.0.1\n      - name: Cache\n        uses: \"Swatinem/rust-cache@{HASH}\"\n      - uses: ./.github/actions/local\n"
        ));

        assert_eq!(
            messages(".github/workflows/ci.yml", &text),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_action_pinned_by_tag_or_branch_fails() {
        for reference in ["v7", "v7.0.1", "main", "0123456"] {
            let text = workflow(&format!("      - uses: actions/checkout@{reference}\n"));
            let found = messages(".github/workflows/nightly.yml", &text);

            assert_eq!(found.len(), 1, "{reference}");
            assert!(found[0].starts_with(".github/workflows/nightly.yml:7: `actions/checkout@"));
            assert!(found[0].contains("not pinned by commit hash"));
        }
    }

    #[test]
    fn the_blocking_workflow_has_no_secrets() {
        let step =
            "      - run: cargo test\n        env:\n          KEY: ${{ secrets.PROVIDER_KEY }}\n";

        let found = messages(".github/workflows/ci.yml", &workflow(step));
        assert_eq!(found.len(), 1);
        assert!(
            found[0]
                .starts_with(".github/workflows/ci.yml:9: the blocking workflow uses no secrets")
        );

        // The by-hand workflow for live tests is where a key belongs.
        assert!(messages(".github/workflows/live.yml", &workflow(step)).is_empty());
    }

    #[test]
    fn the_blocking_workflow_cannot_write() {
        let text = "name: ci\npermissions:\n  contents: write\njobs: {}\n";

        let found = messages(".github/workflows/ci.yml", text);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("token is read-only"));
    }

    #[test]
    fn a_workflow_states_its_permissions() {
        let found = messages(".github/workflows/nightly.yml", "name: nightly\njobs: {}\n");

        assert_eq!(found.len(), 1);
        assert!(found[0].contains("states its token permissions"));
    }

    #[test]
    fn comments_are_not_checked() {
        let text = workflow("      # uses: actions/checkout@v7 would be wrong; secrets.X too\n");

        assert_eq!(
            messages(".github/workflows/ci.yml", &text),
            Vec::<String>::new()
        );
    }
}
