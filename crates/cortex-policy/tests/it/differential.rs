//! Command analysis checked against the real shells.
//!
//! The tokenizers say "this line is these commands with these arguments".
//! A permission rule is matched against that, so it must be what the shell
//! does with the line. These tests generate lines, keep the ones the
//! analysis models, and ask the shell itself:
//!
//! - **`bash` and `sh`** run each line with its commands replaced by a
//!   function that prints its arguments. What the shell passed must be the
//!   words the analysis gave, part by part.
//! - **PowerShell** parses each line with its own parser. The line must be
//!   one command, with no operator, whose elements are constants with the
//!   texts the analysis gave.
//! - **What a program receives from PowerShell**, on Windows: each line is
//!   run with its command replaced by a program that prints its arguments,
//!   through Windows PowerShell 5.1 and through pwsh, in pwsh's own way of
//!   passing arguments and in the old one it keeps for `.cmd` and `.bat`
//!   programs. The parser can read one word where the program receives two.
//!
//! A line the analysis refuses is not looked at: refusing too much costs a
//! question, never a wrong answer. The tests start a shell, hence `sys_`.

use std::process::{Command, Stdio};

use cortex_base::{Rng, SeededRng};
#[cfg(unix)]
use cortex_policy::command::Joiner;
use cortex_policy::command::{Analysis, ShellSyntax, analyse};
use cortex_testkit::cases;

use crate::{Scratch, in_ci};

/// How many generated lines each shell is shown. `CORTEX_PROPERTY_SCALE`
/// multiplies it, as it does every property's cases.
fn line_count() -> u64 {
    let scale = std::env::var("CORTEX_PROPERTY_SCALE")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(1)
        .max(1);
    1500 * scale
}

// ------------------------------------------------------------------ POSIX

/// Pieces of words, chosen to cover everything the POSIX subset accepts
/// and the characters next to it.
const POSIX_PLAIN: [&str; 22] = [
    "a", "B", "9", "_", "-", ".", "/", ":", "=", "+", ",", "@", "%", "é", "--opt", "src/x.rs", "2",
    "x=y", "-n", "a.b", "0", "v1.2",
];

const POSIX_QUOTED: [&str; 26] = [
    "'a b'",
    "''",
    "'$HOME'",
    "'`id`'",
    "'$(id)'",
    "'\"'",
    "'a\\b'",
    "'*'",
    "'~'",
    "'#!{};|&<>()'",
    "'line\nbreak'",
    "'\t'",
    "\"a b\"",
    "\"\"",
    "\"'\"",
    "\"a\\\"b\"",
    "\"a\\\\b\"",
    "\"a\\$b\"",
    "\"a\\`b\"",
    "\"a\\nb\"",
    "\"\\x\"",
    "\"*~#!{};|&<>()\"",
    "\"line\nbreak\"",
    "\"é ü\"",
    "'é'",
    "\"a\\\\\"",
];

const POSIX_SPECIAL: [&str; 8] = ["*", "?", "[a]", "~", "~/x", "*.rs", "a*b", "[!x]y"];

const POSIX_JOINS: [&str; 6] = [" && ", " ; ", "\n", "&&", ";", " &&\n "];

/// Redirections. Their files are in the directory above the one the lines
/// run in, which stays empty so that no glob has anything to match. The
/// ones with no space before them follow a word directly, which is where a
/// word of digits is a descriptor number and not an argument.
const POSIX_REDIRECTS: [&str; 10] = [
    " 2>&1",
    " >/dev/null",
    " > ../out.txt",
    " >> ../out.txt",
    " < ../in.txt",
    " 2>../err.txt",
    " >&2",
    "2>&1",
    ">../out.txt",
    "<../in.txt",
];

fn posix_word(rng: &mut SeededRng) -> String {
    let mut word = String::new();
    for _ in 0..1 + rng.below(3) {
        match rng.below(6) {
            0..=2 => word.push_str(cases::pick(rng, &POSIX_PLAIN)),
            3 | 4 => word.push_str(cases::pick(rng, &POSIX_QUOTED)),
            _ => word.push_str(cases::pick(rng, &POSIX_SPECIAL)),
        }
    }
    word
}

fn posix_line(rng: &mut SeededRng) -> String {
    let mut line = String::new();
    for part in 0..1 + rng.below(3) {
        if part > 0 {
            line.push_str(cases::pick(rng, &POSIX_JOINS));
        }
        line.push_str("show");
        for _ in 0..rng.below(4) {
            line.push(' ');
            if rng.below(8) == 0 {
                line.push_str("   ");
            }
            line.push_str(&posix_word(rng));
        }
        if rng.below(4) == 0 {
            line.push_str(cases::pick(rng, &POSIX_REDIRECTS));
        }
    }
    line
}

/// What a shell passed to each `show` of each line: lines, then parts, then
/// arguments.
#[cfg(unix)]
fn run_posix(shell: &str, lines: &[String], scratch: &Scratch) -> Option<Vec<Vec<Vec<String>>>> {
    // `show` writes to descriptor 3, so that a line's own redirections do
    // not swallow what it reports.
    let mut script = String::from(
        "exec 3>&1\nshow() { { printf '\\036'; for a in \"$@\"; do printf '%s\\037' \"$a\"; done; } >&3; }\n",
    );
    for line in lines {
        script.push_str("printf '\\035' >&3\n");
        script.push_str(line);
        script.push('\n');
    }
    let script_file = scratch.file("script.sh", &script);
    scratch.file("in.txt", "input\n");
    let empty = scratch.dir("empty");
    let output = Command::new(shell)
        .arg(&script_file)
        .current_dir(&empty)
        .env_clear()
        .env("HOME", "/fake/home")
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).expect("the shell printed UTF-8");
    let mut all = Vec::new();
    for line in text.split('\u{1d}').skip(1) {
        let mut parts = Vec::new();
        for part in line.split('\u{1e}').skip(1) {
            let mut arguments: Vec<String> = part.split('\u{1f}').map(str::to_owned).collect();
            // Every argument ends with the separator.
            arguments.pop();
            parts.push(arguments);
        }
        all.push(parts);
    }
    Some(all)
}

/// The arguments the analysis says each `show` gets. `None` for a line the
/// analysis does not model, or whose parts are not all `show`.
#[cfg(unix)]
fn modelled_posix(line: &str) -> Option<Vec<Vec<String>>> {
    let Analysis::Parts(parts) = analyse(line, ShellSyntax::Posix) else {
        return None;
    };
    let mut expected = Vec::new();
    for part in &parts {
        if part.program() != "show" || matches!(part.joiner, Joiner::Pipe | Joiner::Or) {
            return None;
        }
        // The analysis flags every glob as something it cannot know the
        // expansion of, which is right. This test can only compare the
        // ones that match nothing: in the empty directory the lines run
        // in, that is every pattern but one that begins with a dot (which
        // matches `.` and `..`) or names another directory (`/*`).
        if part
            .words
            .iter()
            .any(|word| word.glob && (word.text.starts_with('.') || word.text.contains('/')))
        {
            return None;
        }
        expected.push(
            part.arguments()
                .iter()
                .map(|word| {
                    if word.home {
                        // `HOME` is set to this for the run.
                        format!("/fake/home{}", word.text.get(1..).unwrap_or(""))
                    } else {
                        word.text.clone()
                    }
                })
                .collect(),
        );
    }
    Some(expected)
}

/// `bash` and `sh` pass exactly the words the analysis gives, for every
/// line it models.
#[cfg(unix)]
#[test]
fn sys_the_posix_shells_pass_the_words_the_analysis_gives() {
    let mut rng = SeededRng::new(0x5eed_0012);
    let mut lines = Vec::new();
    let mut expected = Vec::new();
    let mut refused = 0usize;
    for _ in 0..line_count() {
        let line = posix_line(&mut rng);
        match modelled_posix(&line) {
            Some(words) => {
                lines.push(line);
                expected.push(words);
            }
            None => refused += 1,
        }
    }
    assert!(
        lines.len() > 300,
        "too few lines are modelled to test anything: {} of {}",
        lines.len(),
        lines.len() + refused
    );
    let mut shells_run = 0;
    for shell in ["bash", "sh"] {
        let scratch = cortex_testkit::scratch!(&format!("differential_{shell}"));
        let Some(got) = run_posix(shell, &lines, &scratch) else {
            assert!(!in_ci(), "`{shell}` is needed in CI");
            println!("skipped: no `{shell}` here");
            continue;
        };
        shells_run += 1;
        assert_eq!(got.len(), lines.len(), "`{shell}` did not run every line");
        let differences: Vec<String> = lines
            .iter()
            .zip(&expected)
            .zip(&got)
            .filter(|((_, expected), got)| got != expected)
            .map(|((line, expected), got)| {
                format!("{line:?}\n  the shell: {got:?}\n  the analysis: {expected:?}")
            })
            .take(20)
            .collect();
        assert!(
            differences.is_empty(),
            "`{shell}` and the analysis disagree:\n{}",
            differences.join("\n")
        );
    }
    assert!(shells_run > 0 || !in_ci());
    println!(
        "{} lines compared, {refused} refused by the analysis",
        lines.len()
    );
}

/// The lines the generator makes cover what the tokenizer accepts: a
/// sanity check on the test itself, on every OS.
#[test]
fn the_generated_posix_lines_exercise_the_subset() {
    let mut rng = SeededRng::new(0x5eed_0012);
    let (mut quoted, mut globs, mut homes, mut redirects, mut several) = (0, 0, 0, 0, 0);
    for _ in 0..1500 {
        let line = posix_line(&mut rng);
        let Analysis::Parts(parts) = analyse(&line, ShellSyntax::Posix) else {
            continue;
        };
        several += usize::from(parts.len() > 1);
        for part in &parts {
            redirects += part.redirects.len();
            for word in &part.words {
                quoted += usize::from(word.quoted);
                globs += usize::from(word.glob);
                homes += usize::from(word.home);
            }
        }
    }
    for (what, count) in [
        ("quoted words", quoted),
        ("globs", globs),
        ("home directories", homes),
        ("redirections", redirects),
        ("lines of several commands", several),
    ] {
        assert!(count > 20, "only {count} {what} among the modelled lines");
    }
}

// ------------------------------------------------------------- PowerShell

const PS_COMMANDS: [&str; 6] = [
    "Show-It",
    "git",
    "Get-ChildItem",
    r".\tool.exe",
    r"C:\Tools\rg.exe",
    "cargo",
];

const PS_PLAIN: [&str; 30] = [
    "a", "B", "9", "_", "-", ".", "/", "\\", ":", "=", "+", "src", "-Path", "-Recurse", "--opt",
    "x=y", "5", "-5", "1.0", "0x10", "5kb", "C:\\Work", "..", ".5", "a.b", "-n", "--", "v1", "1e3",
    "-Path:x",
];

const PS_QUOTED: [&str; 26] = [
    "'a b'",
    "''",
    "'it''s'",
    "'$x'",
    "'`n'",
    "'\"'",
    "';|&(){}@,%#'",
    "'é'",
    "\"a b\"",
    "\"\"",
    "\"say \"\"hi\"\"\"",
    "\"'\"",
    "\";|&(){}@,%#\"",
    "\"é ü\"",
    "'-NotAParameter'",
    "\"C:\\Program Files\\x\"",
    "';(){}@,#'",
    "\";(){}@,#\"",
    "'a  b'",
    "' lead'",
    "'trail '",
    "'a\\b c'",
    "'a\\'",
    "'a \\'",
    "'x&y'",
    "'--opt=a b'",
];

const PS_SPECIAL: [&str; 6] = ["*", "?", "*.rs", "~", "~\\x", "a*b"];

fn powershell_line(rng: &mut SeededRng) -> String {
    let mut line = String::new();
    if rng.below(6) == 0 {
        line.push_str("  ");
    }
    line.push_str(cases::pick(rng, &PS_COMMANDS));
    for _ in 0..rng.below(5) {
        line.push(' ');
        if rng.below(8) == 0 {
            line.push('\t');
        }
        match rng.below(7) {
            0..=2 => {
                for _ in 0..1 + rng.below(3) {
                    line.push_str(cases::pick(rng, &PS_PLAIN));
                }
            }
            3 | 4 => line.push_str(cases::pick(rng, &PS_QUOTED)),
            5 => line.push_str(cases::pick(rng, &PS_SPECIAL)),
            // A quote next to other text, which the analysis refuses.
            _ => {
                line.push_str(cases::pick(rng, &PS_PLAIN));
                line.push_str(cases::pick(rng, &PS_QUOTED));
            }
        }
    }
    line
}

/// A script that parses each line of a file with PowerShell's own parser,
/// and prints `OK` and the texts of the command's elements, or `NO`.
const PS_PARSE: &str = r#"
param([string]$File)
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$lines = [System.IO.File]::ReadAllLines($File, [System.Text.Encoding]::UTF8)
$sep = [string][char]31
foreach ($line in $lines) {
    $tokens = $null
    $errors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseInput($line, [ref]$tokens, [ref]$errors)
    $out = 'NO'
    if ($errors.Count -eq 0 -and $null -ne $ast.EndBlock -and $ast.EndBlock.Statements.Count -eq 1 `
            -and $null -eq $ast.BeginBlock -and $null -eq $ast.ProcessBlock -and $null -eq $ast.ParamBlock) {
        $statement = $ast.EndBlock.Statements[0]
        if ($statement -is [System.Management.Automation.Language.PipelineAst] `
                -and $statement.PipelineElements.Count -eq 1 -and -not $statement.Background) {
            $command = $statement.PipelineElements[0]
            if ($command -is [System.Management.Automation.Language.CommandAst] `
                    -and $command.InvocationOperator -eq 'Unknown' -and $command.Redirections.Count -eq 0) {
                $parts = @()
                $ok = $true
                foreach ($element in $command.CommandElements) {
                    if ($element -is [System.Management.Automation.Language.StringConstantExpressionAst]) {
                        $parts += [string]$element.Value
                    } elseif ($element -is [System.Management.Automation.Language.CommandParameterAst]) {
                        $parts += $element.Extent.Text
                    } elseif ($element -is [System.Management.Automation.Language.ConstantExpressionAst]) {
                        $parts += $element.Extent.Text
                    } else {
                        $ok = $false
                    }
                }
                if ($ok) { $out = 'OK' + $sep + ($parts -join $sep) + $sep }
            }
        }
    }
    [Console]::Out.WriteLine($out)
}
"#;

/// What PowerShell's parser makes of each line: the element texts of its
/// one command, or `None` for a line that is anything else.
fn run_powershell(lines: &[String], scratch: &Scratch) -> Option<Vec<Option<Vec<String>>>> {
    let script = scratch.file("parse.ps1", PS_PARSE);
    let file = scratch.file("lines.txt", lines.join("\n"));
    let output = Command::new("pwsh")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-File"])
        .arg(&script)
        .arg(&file)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    assert!(
        output.status.success(),
        "PowerShell failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).expect("PowerShell printed UTF-8");
    Some(
        text.lines()
            .map(|line| {
                let rest = line.strip_prefix("OK\u{1f}")?;
                let mut elements: Vec<String> = rest.split('\u{1f}').map(str::to_owned).collect();
                elements.pop();
                Some(elements)
            })
            .collect(),
    )
}

/// PowerShell's own parser reads every line the analysis models as one
/// command with exactly the words the analysis gives. This is where a
/// wrong belief about PowerShell's tokens would show.
#[test]
fn sys_powershell_reads_a_modelled_line_as_the_analysis_does() {
    let mut rng = SeededRng::new(0x5eed_0112);
    let mut lines = Vec::new();
    let mut expected: Vec<Vec<String>> = Vec::new();
    for _ in 0..line_count() {
        let line = powershell_line(&mut rng);
        let Analysis::Parts(parts) = analyse(&line, ShellSyntax::PowerShell) else {
            continue;
        };
        assert_eq!(parts.len(), 1);
        expected.push(
            parts[0]
                .words
                .iter()
                .map(|word| word.text.clone())
                .collect(),
        );
        lines.push(line);
    }
    assert!(
        lines.len() > 200,
        "too few lines are modelled: {}",
        lines.len()
    );

    let scratch = cortex_testkit::scratch!("differential_powershell");
    let Some(got) = run_powershell(&lines, &scratch) else {
        assert!(!in_ci(), "`pwsh` is needed in CI");
        println!("skipped: no `pwsh` here");
        return;
    };
    assert_eq!(
        got.len(),
        lines.len(),
        "PowerShell did not answer for every line"
    );
    let differences: Vec<String> = lines
        .iter()
        .zip(&expected)
        .zip(&got)
        .filter(|((_, expected), got)| got.as_ref() != Some(*expected))
        .map(|((line, expected), got)| {
            format!("{line:?}\n  PowerShell: {got:?}\n  the analysis: {expected:?}")
        })
        .take(40)
        .collect();
    assert!(
        differences.is_empty(),
        "PowerShell and the analysis disagree (`None`: not one command of constants):\n{}",
        differences.join("\n")
    );
    println!("{} lines compared", lines.len());
}

/// What a program receives when PowerShell runs a modelled line: exactly
/// the words the analysis gives, through Windows PowerShell 5.1, through
/// pwsh, and through pwsh in the old way of passing arguments, which it
/// keeps for `.cmd` and `.bat` programs. The parser comparison above cannot
/// see this: 5.1 drops an empty text and passes a `"` on unescaped, so the
/// program can receive more words than the parser read (found by the review
/// of step 12).
#[cfg(windows)]
#[test]
fn sys_a_program_receives_the_words_the_analysis_gives_from_every_powershell() {
    use cortex_testkit::children::{echo_arguments, echoed};

    // Lines that bear on how words reach a program, then generated ones.
    // The echoing program is started once per line, so there are few.
    let mut arguments: Vec<String> = [
        "'a b' c",
        "'a  b' ' lead' 'trail '",
        r"'C:\Program Files\x' 'a\b c'",
        "--opt=1.5 -5.5 - -- -.5",
        "'-NotAParameter' \"-a:b\"",
        "'it''s' \"a b\"",
        r"'a\' b",
        r"a\ b\\",
        "x=y --src-prefix=a/",
        "'(x)' '{y}' '@z' ',w' '#v' ';u'",
    ]
    .iter()
    .map(|text| (*text).to_owned())
    .collect();
    let mut rng = SeededRng::new(0x5eed_0212);
    while arguments.len() < 40 {
        let line = powershell_line(&mut rng);
        let Some((_, rest)) = line.trim_start().split_once(' ') else {
            continue;
        };
        if rest.is_ascii() {
            arguments.push(rest.to_owned());
        }
    }
    let mut lines = Vec::new();
    let mut expected: Vec<Vec<String>> = Vec::new();
    for rest in &arguments {
        let line = format!("Show-It {rest}");
        let Analysis::Parts(parts) = analyse(&line, ShellSyntax::PowerShell) else {
            continue;
        };
        expected.push(
            parts[0].words[1..]
                .iter()
                .map(|word| word.text.clone())
                .collect(),
        );
        lines.push(rest.clone());
    }
    assert!(
        lines.len() >= 20,
        "too few lines are modelled: {}",
        lines.len()
    );

    let scratch = cortex_testkit::scratch!("powershell_argv");
    let (program, prefix) = echo_arguments(crate::ENTRY);
    let mut script = String::from(
        "param([string]$Mode)\n\
         $ErrorActionPreference = 'Stop'\n\
         [Console]::OutputEncoding = [System.Text.Encoding]::UTF8\n\
         if ($Mode) { $PSNativeCommandArgumentPassing = $Mode }\n",
    );
    let call = format!("& '{}' {}", program.replace('\'', "''"), prefix.join(" "));
    for rest in &lines {
        script.push_str(&format!("{call} {rest}\n"));
    }
    // With a byte-order mark, which Windows PowerShell needs to read the
    // file as UTF-8.
    let file = scratch.file("argv.ps1", &format!("\u{feff}{script}"));
    for (shell, mode) in [("powershell", ""), ("pwsh", ""), ("pwsh", "Legacy")] {
        let output = Command::new(shell)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&file)
            .arg(mode)
            .stdin(Stdio::null())
            .output();
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                assert!(!in_ci(), "`{shell}` is needed in CI: {error}");
                println!("skipped {shell}: {error}");
                continue;
            }
        };
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{shell} {mode} failed: {}\n{text}",
            String::from_utf8_lossy(&output.stderr)
        );
        let got = echoed(&text);
        assert_eq!(got.len(), lines.len(), "{shell} {mode}: {text}");
        let differences: Vec<String> = lines
            .iter()
            .zip(&expected)
            .zip(&got)
            .filter(|((_, expected), got)| expected != got)
            .map(|((line, expected), got)| {
                format!("{line:?}\n  the program received: {got:?}\n  the analysis: {expected:?}")
            })
            .collect();
        assert!(
            differences.is_empty(),
            "{shell} {mode} passed other words than the analysis gave:\n{}",
            differences.join("\n")
        );
        println!("{shell} {mode}: {} lines compared", lines.len());
    }
}

#[test]
fn the_generated_powershell_lines_exercise_the_subset() {
    let mut rng = SeededRng::new(0x5eed_0112);
    let (mut modelled, mut refused, mut quoted, mut patterns) = (0, 0, 0, 0);
    for _ in 0..1500 {
        match analyse(&powershell_line(&mut rng), ShellSyntax::PowerShell) {
            Analysis::Parts(parts) => {
                modelled += 1;
                for word in &parts[0].words {
                    quoted += usize::from(word.quoted);
                    patterns += usize::from(word.glob || word.home);
                }
            }
            Analysis::NotModelled { .. } => refused += 1,
        }
    }
    assert!(
        modelled > 200 && refused > 50,
        "{modelled} modelled, {refused} refused"
    );
    assert!(
        quoted > 50 && patterns > 20,
        "{quoted} quoted, {patterns} patterns"
    );
}
