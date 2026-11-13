//! A permission rule as the command line writes it
//! (`docs/architecture/10-headless.md`, `--allow` and `--deny`).

use cortex_protocol::lease::{Effect, Rule, RuleAction};

/// Reads a rule written as an effect and a pattern, with a colon or a
/// space between them: `exec:cargo test *`, `write .github/workflows/**`.
/// The action is the flag's: `--allow` or `--deny`.
///
/// Only the form is checked here. Whether the pattern is one the
/// permission engine accepts is the engine's to say, when a session
/// starts.
///
/// # Errors
///
/// When there is no effect or no pattern, or the effect is not one of
/// `read`, `write`, `exec`, `network` and `ext`. The message is for the
/// person who typed the flag.
pub fn parse_rule(text: &str, action: RuleAction) -> Result<Rule, String> {
    let text = text.trim();
    let split = text
        .find(|character: char| character == ':' || character.is_whitespace())
        .ok_or_else(|| {
            format!(
                "`{text}` is not a rule. Write an effect and what it matches, as in `exec:cargo \
                 test *` or `write src/**`."
            )
        })?;
    let (effect, pattern) = (&text[..split], text[split + 1..].trim());
    let effect = Effect::from_wire(effect);
    if !effect.is_known() {
        return Err(format!(
            "`{}` is not an effect. A rule begins with `read`, `write`, `exec`, `network` or \
             `ext`.",
            effect.as_str()
        ));
    }
    if pattern.is_empty() {
        return Err(format!(
            "The rule `{text}` matches nothing: write what it matches after the effect."
        ));
    }
    Ok(Rule {
        effect,
        pattern: pattern.to_owned(),
        action,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rule_is_an_effect_and_a_pattern_with_a_colon_or_a_space_between() {
        for (text, effect, pattern) in [
            ("exec:cargo test *", Effect::Exec, "cargo test *"),
            (
                "write .github/workflows/**",
                Effect::Write,
                ".github/workflows/**",
            ),
            ("  read:  ~/notes/**  ", Effect::Read, "~/notes/**"),
            (
                "network:example.com:443",
                Effect::Network,
                "example.com:443",
            ),
            ("ext docs/search", Effect::Ext, "docs/search"),
            (
                "write C:\\work\\out.txt",
                Effect::Write,
                "C:\\work\\out.txt",
            ),
        ] {
            let rule = parse_rule(text, RuleAction::Allow).unwrap();
            assert_eq!(
                (rule.effect, rule.pattern.as_str()),
                (effect, pattern),
                "{text}"
            );
            assert_eq!(rule.action, RuleAction::Allow);
        }
        assert_eq!(
            parse_rule("exec:rm *", RuleAction::Deny).unwrap().action,
            RuleAction::Deny
        );
    }

    #[test]
    fn what_is_not_a_rule_is_refused_with_what_to_write() {
        for (text, says) in [
            ("cargo", "not a rule"),
            ("", "not a rule"),
            ("run:cargo test", "not an effect"),
            ("exec:", "matches nothing"),
            ("write   ", "not a rule"),
        ] {
            let error = parse_rule(text, RuleAction::Allow).unwrap_err();
            assert!(error.contains(says), "{text:?}: {error}");
        }
    }
}
