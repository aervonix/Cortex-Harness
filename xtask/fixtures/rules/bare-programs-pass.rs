// A source file the bare-program rule passes: every program is started by
// a path, or in a test, or with a reason given.

use std::process::Command;

const SHELL: &str = "/bin/sh";

fn found(git: &std::path::Path) {
    // By the path a search found.
    let _ = Command::new(git);
    let _ = Command::new(SHELL);
    let _ = std::process::Command::new("/usr/bin/env");
    let _ = Command::new(r"C:\Windows\System32\cmd.exe");
    // A name in a string that is not a call, or in a comment:
    // Command::new("git")
    let _ = "Command::new(\"git\")";
    let _ = other::new("git");
    // program by name: the user's own editor, as they named it in $EDITOR
    let _ = Command::new("editor");
    let _ = Command::new("pager"); // program by name: the user's pager, by its name
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_test_runs_the_machines_git() {
        let _ = std::process::Command::new("git");
    }
}

#[cfg(all(test, unix))]
mod unix_tests {
    fn sh() {
        let _ = std::process::Command::new("sh");
    }
}

#[test]
fn a_test_outside_a_module() {
    let _ = std::process::Command::new("mkfifo");
}
