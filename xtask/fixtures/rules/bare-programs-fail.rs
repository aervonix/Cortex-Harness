// A source file the bare-program rule fails: each line marked `flagged`
// starts a program by its bare name, outside a test.

use std::process::Command;

const TOOL: &str = "cargo-machete";

fn started() {
    let _ = Command::new("git"); // flagged
    let _ = std::process::Command::new("sh"); // flagged
    let _ = tokio::process::Command::new(r"bash"); // flagged
    let _ = Command::new(TOOL); // flagged
    let _ = cortex_tools::CommandLine::new("cmd"); // flagged
    let _ = Shell::new("pwsh"); // flagged
    let _ = Command::new( // flagged
        "powershell",
    );
    // program by name:
    let _ = Command::new("taskkill"); // flagged: an allow needs its reason
}

#[cfg(not(test))]
fn not_a_test() {
    let _ = Command::new("kill"); // flagged
}
