//! Rules for the GitHub Actions workflows (`docs/testing-and-ci.md`, "CI
//! layout"): actions are pinned by commit hash, every workflow's token is
//! read-only by default, a workflow that runs on pull requests has no secrets
//! but the owner's protected terms (for the owner guard) and can write
//! nothing, and the one thing any workflow may write is an
//! issue, from the job that reports a failure. And no script reads a value
//! someone typed: a workflow's inputs, and what an event carries, reach a
//! `run:` script through the environment, never as text pasted into it.

use super::Violation;

/// The one job that may hold a token that writes, and the one thing it may
/// write. The non-blocking workflows (`main`, `nightly`) open or update an
/// issue when they fail, because a failure nobody is told about is a failure
/// that is ignored. Everything else a workflow does needs to read only, and
/// a job that builds and runs code must not be the job that can write.
const REPORT_JOB: &str = "report";
const REPORT_PERMISSION: &str = "issues";

pub(crate) fn check(file: &str, text: &str) -> Vec<Violation> {
    let mut violations = Vec::new();
    let on_pull_requests = runs_on_pull_requests(text);
    let mut states_permissions = false;
    let mut in_jobs = false;
    let mut job: Option<&str> = None;
    // The indent of the `run:` key whose script the lines are in.
    let mut script: Option<usize> = None;

    for (index, raw_line) in text.lines().enumerate() {
        let line_number = Some(index + 1);
        // A script's lines are checked whole: `#` begins a comment in YAML
        // but not in every shell, and a typed value is no safer after one.
        let raw_indent = raw_line.len() - raw_line.trim_start().len();
        if script.is_some_and(|key| raw_line.trim().is_empty() || raw_indent > key) {
            if pastes_a_typed_value(raw_line) {
                violations.push(pasted(file, line_number));
            }
            if on_pull_requests && uses_a_secret_other_than_the_owners_terms(raw_line) {
                violations.push(secret_on_pull_requests(file, line_number));
            }
            continue;
        }
        script = None;
        if let Some((key, rest)) = run_key(raw_line) {
            if rest.starts_with('|') || rest.starts_with('>') || rest.is_empty() {
                script = Some(key);
            } else if pastes_a_typed_value(rest) {
                violations.push(pasted(file, line_number));
            }
        }
        // Comments are not rules.
        let line = raw_line.split(" #").next().unwrap_or(raw_line);
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        if let Some(value) = plain_value_that_is_not_one(line) {
            violations.push(Violation::new(
                file,
                line_number,
                format!(
                    "`{value}` is not a value YAML can read unquoted: a `: ` in it, or a `:` at \
                     its end, begins a mapping, and GitHub then reads the whole file as no \
                     workflow at all (no trigger, no job). Quote it, or end it otherwise \
                     (`combine::tests::sys_`, not `combine::`)."
                ),
            ));
        }
        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            in_jobs = line.trim_end() == "jobs:";
            job = None;
        } else if in_jobs && indent == 2 {
            job = line.trim().strip_suffix(':');
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

        if on_pull_requests && uses_a_secret_other_than_the_owners_terms(line) {
            violations.push(secret_on_pull_requests(file, line_number));
        }

        if let Some(permission) = written_permission(line) {
            let in_report_job = indent >= 4 && job == Some(REPORT_JOB);
            if on_pull_requests {
                violations.push(Violation::new(
                    file,
                    line_number,
                    "a workflow that runs on pull requests has a read-only token. Work that \
                     must write belongs in a workflow that does not run on pull requests.",
                ));
            } else if permission != REPORT_PERMISSION || !in_report_job {
                violations.push(Violation::new(
                    file,
                    line_number,
                    format!(
                        "`{permission}: write` is not allowed here. A workflow's token is \
                         read-only, with one exception: the job named `{REPORT_JOB}` of a \
                         workflow that does not run on pull requests may have \
                         `{REPORT_PERMISSION}: write`, to open or update an issue when the \
                         workflow fails. It is given to that job alone, so that no job which \
                         builds or runs code holds a token that can write."
                    ),
                ));
            }
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

/// The one secret a workflow that runs on pull requests may read: the
/// owner's protected terms, which the owner guard looks for and which pay
/// for nothing (`docs/testing-and-ci.md`, "The owner guard"). It is given
/// to the environment variable of the same name and nothing else.
const OWNER_TERMS_LINE: &str = "OWNER_TERMS: ${{ secrets.OWNER_TERMS }}";

/// The value of a `key: value` line when it is unquoted and YAML cannot
/// read it as one: it holds `: ` or ends with `:`. Found when a step's
/// `run: cargo test … combine::` made the nightly workflow unreadable.
fn plain_value_that_is_not_one(line: &str) -> Option<&str> {
    let line = line.trim();
    let line = line.strip_prefix("- ").unwrap_or(line);
    let (key, value) = line.split_once(": ")?;
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    let value = value.trim();
    let quoted_or_block = value
        .chars()
        .next()
        .is_none_or(|first| "\"'|>{[&*!".contains(first));
    (!quoted_or_block && (value.contains(": ") || value.ends_with(':'))).then_some(value)
}

/// Whether a line reads a secret, other than in [`OWNER_TERMS_LINE`].
fn uses_a_secret_other_than_the_owners_terms(line: &str) -> bool {
    line.contains("secrets.") && line.trim() != OWNER_TERMS_LINE
}

/// Whether the workflow has a pull-request trigger. Such a workflow runs
/// code that anybody can propose.
fn runs_on_pull_requests(text: &str) -> bool {
    text.lines()
        .map(|line| line.split(" #").next().unwrap_or(line).trim())
        .any(|line| {
            line.starts_with("pull_request")
                || (line.starts_with("on:") && line.contains("pull_request"))
        })
}

/// The permission a line gives write access to, if it gives any.
/// `permissions: write-all` gives all of them.
fn written_permission(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.replace(' ', "") == "permissions:write-all" {
        return Some("all");
    }
    let (permission, level) = line.split_once(':')?;
    (level.trim() == "write").then_some(permission.trim())
}

/// The indent of a `run:` key and what follows it on its line, if the line
/// is one: `run: cargo test`, or `- run: |` with the script below it.
fn run_key(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let key = trimmed.trim_start_matches("- ");
    let rest = key.strip_prefix("run:")?;
    let indent = line.len() - key.len();
    Some((indent, rest.trim()))
}

/// Whether a script's text has a value someone typed pasted into it: a
/// workflow's input, or a part of the event (a branch's name, a pull
/// request's title). A value such as `"; curl … | sh"` would then be a
/// command. Through `env:`, the same value is a variable's content.
fn pastes_a_typed_value(script: &str) -> bool {
    // Every expression in the text, however it is written: `inputs.x`,
    // `(inputs.x)`, `inputs['x']`, `format('{0}', inputs.x)`,
    // `toJSON(inputs)`, and the parts of the event someone names.
    let mut rest = script;
    while let Some(start) = rest.find("${{") {
        let after = &rest[start + 3..];
        let end = after.find("}}").unwrap_or(after.len());
        let expression: String = after[..end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let typed = [
            "inputs",
            "github.event",
            "github.head_ref",
            "github.ref_name",
        ]
        .iter()
        .any(|name| expression.contains(name));
        if typed {
            return true;
        }
        rest = &after[end..];
    }
    false
}

fn secret_on_pull_requests(file: &str, line: Option<usize>) -> Violation {
    Violation::new(
        file,
        line,
        "a workflow that runs on pull requests uses no secret but the owner's protected \
         terms, as the line `OWNER_TERMS: ${{ secrets.OWNER_TERMS }}`. Its jobs run code \
         from the pull request, and nothing that blocks may need a paid API or a \
         credential (rule 9 in docs/build-plan.md).",
    )
}

fn pasted(file: &str, line: Option<usize>) -> Violation {
    Violation::new(
        file,
        line,
        "a `run:` script has a workflow input or a part of the event pasted into its text. \
         Give it to the step's `env:` and read the variable: a typed value pasted into a \
         script is code the script runs.",
    )
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

    #[test]
    fn a_value_yaml_cannot_read_unquoted_is_refused() {
        let found = messages(
            ".github/workflows/x.yml",
            &workflow(
                "      - name: Plain, with a colon:here and (brackets)\n\
                 \x20       run: cargo test -p xtask -- --ignored combine::\n\
                 \x20     - name: Quoted\n\
                 \x20       run: \"echo a: b\"\n\
                 \x20     - name: An echo: of a mapping\n\
                 \x20       run: |\n\
                 \x20         echo a: b\n",
            ),
        );
        let refused: Vec<&String> = found
            .iter()
            .filter(|message| message.contains("YAML"))
            .collect();
        assert_eq!(refused.len(), 2, "{found:#?}");
        assert!(
            refused[0].contains("`cargo test -p xtask -- --ignored combine::`"),
            "{refused:?}"
        );
        assert!(
            refused[1].contains("`An echo: of a mapping`"),
            "{refused:?}"
        );
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

    /// A workflow that pull requests trigger.
    fn blocking(steps: &str) -> String {
        format!(
            "name: ci\non:\n  pull_request:\n  push:\n    branches: [main]\npermissions:\n  contents: read\njobs:\n  lint:\n    steps:\n{steps}"
        )
    }

    /// A workflow that pull requests cannot trigger, with a job that reports.
    fn non_blocking(report_permissions: &str) -> String {
        format!(
            "name: nightly\non:\n  schedule:\n    - cron: \"7 3 * * *\"\npermissions:\n  contents: read\njobs:\n  build:\n    steps:\n      - run: cargo build\n  report:\n    needs: [build]\n    permissions:\n{report_permissions}    steps:\n      - run: gh issue create\n"
        )
    }

    #[test]
    fn a_pull_request_workflow_has_no_secrets() {
        let step =
            "      - run: cargo test\n        env:\n          KEY: ${{ secrets.PROVIDER_KEY }}\n";

        let found = messages(".github/workflows/ci.yml", &blocking(step));
        assert_eq!(found.len(), 1);
        assert!(
            found[0]
                .contains("ci.yml:13: a workflow that runs on pull requests uses no secret but"),
            "{}",
            found[0]
        );

        // The by-hand workflow for live tests is where a key belongs.
        assert!(messages(".github/workflows/live.yml", &workflow(step)).is_empty());
    }

    #[test]
    fn the_owners_terms_are_the_one_secret_a_pull_request_may_read() {
        let guard = "      - run: cargo xtask owner-guard\n        env:\n          OWNER_TERMS: ${{ secrets.OWNER_TERMS }}\n";
        assert!(messages(".github/workflows/ci.yml", &blocking(guard)).is_empty());
        // Under another name, beside another secret, or in a command line,
        // it is a secret like any other.
        for line in [
            "          TERMS: ${{ secrets.OWNER_TERMS }}\n",
            "          OWNER_TERMS: ${{ secrets.OWNER_TERMS }}${{ secrets.KEY }}\n",
            "      - run: echo ${{ secrets.OWNER_TERMS }}\n",
            "          OWNER_TERMS: ${{ secrets.OWNER_TERMS_AND_MORE }}\n",
        ] {
            let step = format!("      - run: cargo xtask owner-guard\n        env:\n{line}");
            assert_eq!(
                messages(".github/workflows/ci.yml", &blocking(&step)).len(),
                1,
                "{line}"
            );
        }
    }

    #[test]
    fn a_pull_request_workflow_cannot_write() {
        for text in [
            "name: ci\non: [push, pull_request]\npermissions:\n  contents: write\njobs: {}\n",
            "name: ci\non:\n  pull_request_target:\npermissions:\n  contents: read\njobs:\n  report:\n    permissions:\n      issues: write\n",
        ] {
            let found = messages(".github/workflows/any-name.yml", text);
            assert_eq!(found.len(), 1, "{text}");
            assert!(found[0].contains("has a read-only token"), "{}", found[0]);
        }
    }

    #[test]
    fn the_report_job_of_a_non_blocking_workflow_may_write_issues() {
        let text = non_blocking("      issues: write\n");
        assert_eq!(
            messages(".github/workflows/nightly.yml", &text),
            Vec::<String>::new()
        );
    }

    #[test]
    fn nothing_else_may_write() {
        // Another permission in the report job.
        let found = messages(
            ".github/workflows/nightly.yml",
            &non_blocking("      issues: write\n      contents: write\n"),
        );
        assert_eq!(found.len(), 1);
        assert!(
            found[0].contains("nightly.yml:15: `contents: write` is not allowed here"),
            "{}",
            found[0]
        );

        // The right permission in the wrong job.
        let text = non_blocking("      issues: read\n").replace(
            "  build:\n",
            "  build:\n    permissions:\n      issues: write\n",
        );
        let found = messages(".github/workflows/nightly.yml", &text);
        assert_eq!(found.len(), 1);
        assert!(found[0].contains("`issues: write` is not allowed here"));
        assert!(found[0].contains("the job named `report`"));

        // At the top level, where every job would get it.
        let text = non_blocking("      issues: write\n").replace(
            "permissions:\n  contents: read\n",
            "permissions:\n  contents: read\n  issues: write\n",
        );
        let found = messages(".github/workflows/nightly.yml", &text);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].contains("nightly.yml:7:"), "{}", found[0]);

        // Everything at once.
        let text = non_blocking("      issues: write\n").replace(
            "permissions:\n  contents: read\n",
            "permissions: write-all\n",
        );
        let found = messages(".github/workflows/nightly.yml", &text);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert!(found[0].contains("`all: write` is not allowed here"));
    }

    #[test]
    fn a_typed_value_is_never_pasted_into_a_script() {
        let dispatched = |steps: &str| {
            format!(
                "name: try\non:\n  workflow_dispatch:\n    inputs:\n      command:\n        default: test\npermissions:\n  contents: read\njobs:\n  test:\n    steps:\n{steps}"
            )
        };
        // Through the environment: fine, and so is an input in a step's
        // condition or in a `with:` value.
        let through_env = dispatched(
            "      - name: Run\n        if: ${{ inputs.command != '' }}\n        env:\n          TRY: ${{ inputs.command }}\n          BASE: ${{ github.event.pull_request.base.sha }}\n        run: |\n          cargo xtask try --command \"$TRY\"\n          echo ${{ matrix.name }}\n      - uses: ./.github/actions/x\n        with:\n          what: ${{ inputs.command }}\n",
        );
        assert_eq!(
            messages(".github/workflows/try.yml", &through_env),
            Vec::<String>::new()
        );

        for (script, line) in [
            ("      - run: cargo xtask ${{ inputs.command }}\n", 12),
            (
                "      - name: Run\n        run: |\n          set -e\n          cargo xtask ${{inputs.command}}\n",
                15,
            ),
            (
                "      - run: >-\n          echo\n          ${{ github.event.inputs.command }}\n",
                14,
            ),
            (
                "      - run: |\n          git checkout ${{ github.head_ref }}\n",
                13,
            ),
            (
                "      - run: |\n          # ${{ github.event.pull_request.title }}\n",
                13,
            ),
            (
                "      - run: |\n\n          echo \"${{ github.event.pull_request.title }}\"\n",
                14,
            ),
            ("      - run: echo ${{ (inputs.command) }}\n", 12),
            (
                "      - run: echo ${{ format('{0}', inputs.command) }}\n",
                12,
            ),
            ("      - run: echo '${{ toJSON(inputs) }}'\n", 12),
            ("      - run: echo ${{ inputs['command'] }}\n", 12),
            ("      - run: echo ${{ github.ref_name }}\n", 12),
        ] {
            let found = messages(".github/workflows/try.yml", &dispatched(script));
            assert_eq!(found.len(), 1, "{script}: {found:?}");
            assert!(
                found[0].starts_with(&format!(
                    ".github/workflows/try.yml:{line}: a `run:` script"
                )),
                "{script}: {found:?}"
            );
        }
        // A secret in a pull request's script is still found there.
        let secret = blocking("      - run: |\n          echo ${{ secrets.PROVIDER_KEY }}\n");
        let found = messages(".github/workflows/ci.yml", &secret);
        assert!(
            found.len() == 1 && found[0].contains("uses no secret but"),
            "{found:?}"
        );
        // A script ends where its block does.
        let after = dispatched(
            "      - run: |\n          cargo test\n        env:\n          X: ${{ inputs.command }}\n",
        );
        assert_eq!(
            messages(".github/workflows/try.yml", &after),
            Vec::<String>::new()
        );
        // Expressions that read nothing typed are fine in a script.
        let fine = dispatched(
            "      - run: cargo build ${{ runner.os == 'Windows' && '--release' || '' }} ${{ matrix.target }}\n",
        );
        assert_eq!(
            messages(".github/workflows/try.yml", &fine),
            Vec::<String>::new()
        );
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
