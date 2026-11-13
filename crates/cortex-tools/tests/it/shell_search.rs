//! Which shell is found (`docs/architecture/05-tools.md`, "Which shell"):
//! the configured one, or the system's default with its fallbacks.
//!
//! The search is a value, so the order for each system is tested on both:
//! the "programs" are empty files in directories of the test's own, and
//! nothing is started.

use std::path::{Path, PathBuf};

use cortex_tools::shell::ShellError;
use cortex_tools::{ShellKind, ShellSearch};

use crate::Workspace;

/// Makes an empty file that counts as a program.
fn program(directory: &Path, name: &str) -> PathBuf {
    std::fs::create_dir_all(directory).expect("create a directory");
    let path = directory.join(name);
    std::fs::write(&path, "").expect("write a file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make it runnable");
    }
    path
}

fn search(windows: bool, path: &[&Path]) -> ShellSearch {
    ShellSearch {
        windows,
        path: path
            .iter()
            .map(|directory| directory.to_path_buf())
            .collect(),
        system_root: None,
        comspec: None,
    }
}

#[test]
fn linux_takes_bash_and_falls_back_to_sh() {
    let ws = Workspace::new("search_linux");
    let (first, second) = (ws.dir.join("first"), ws.dir.join("second"));
    let sh = program(&first, "sh");
    let bash = program(&second, "bash");

    // bash wherever it is on the path, before sh.
    let found = search(false, &[&first, &second])
        .find(None, None)
        .expect("a shell");
    assert_eq!(found.kind(), ShellKind::Bash);
    assert_eq!(found.program(), bash);

    let found = search(false, &[&first]).find(None, None).expect("a shell");
    assert_eq!(found.kind(), ShellKind::Posix);
    assert_eq!(found.program(), sh);

    let none = search(false, &[&ws.dir.join("empty")]).find(None, None);
    assert_eq!(
        none,
        Err(ShellError::NoneFound("bash and sh on PATH".to_owned()))
    );
    // The shells of the other system are not looked for.
    let other = ws.dir.join("windows");
    program(&other, "pwsh.exe");
    program(&other, "cmd.exe");
    assert!(search(false, &[&other]).find(None, None).is_err());
}

#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_run_is_not_a_shell() {
    use std::os::unix::fs::PermissionsExt;

    let ws = Workspace::new("search_not_runnable");
    let (first, second) = (ws.dir.join("first"), ws.dir.join("second"));
    let plain = program(&first, "bash");
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    std::fs::create_dir_all(second.join("sh")).expect("a directory named like a shell");
    let real = program(&ws.dir.join("third"), "bash");

    let found = search(false, &[&first, &second, &ws.dir.join("third")])
        .find(None, None)
        .expect("a shell");
    assert_eq!(found.program(), real);
}

#[test]
fn windows_takes_powershell_7_then_windows_powershell_then_cmd() {
    let ws = Workspace::new("search_windows");
    let all = ws.dir.join("all");
    let pwsh = program(&all, "pwsh.exe");
    program(&all, "powershell.exe");
    program(&all, "cmd.exe");
    let found = search(true, &[&all]).find(None, None).expect("a shell");
    assert_eq!(found.kind(), ShellKind::Pwsh);
    assert_eq!(found.program(), pwsh);
    assert_eq!(found.name(), "PowerShell 7 (pwsh)");

    // Without PowerShell 7: Windows PowerShell, from the path.
    let older = ws.dir.join("older");
    let powershell = program(&older, "powershell.exe");
    program(&older, "cmd.exe");
    let found = search(true, &[&older]).find(None, None).expect("a shell");
    assert_eq!(found.kind(), ShellKind::WindowsPowerShell);
    assert_eq!(found.program(), powershell);
    assert_eq!(found.name(), "Windows PowerShell 5.1");

    // Or from where Windows keeps it, when the path does not have it.
    let system = ws.dir.join("system");
    let kept = program(
        &system
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0"),
        "powershell.exe",
    );
    let system_cmd = program(&system.join("System32"), "cmd.exe");
    let mut with_system = search(true, &[&ws.dir.join("empty")]);
    with_system.system_root = Some(system.clone());
    let found = with_system.find(None, None).expect("a shell");
    assert_eq!(found.kind(), ShellKind::WindowsPowerShell);
    assert_eq!(found.program(), kept);

    // With neither PowerShell: cmd.exe, by the system's own word for it,
    // then from where Windows keeps it.
    let bare = ws.dir.join("bare");
    let bare_cmd = program(&bare.join("System32"), "cmd.exe");
    let elsewhere = program(&ws.dir.join("elsewhere"), "CMD.EXE");
    let mut only_cmd = search(true, &[&ws.dir.join("empty")]);
    only_cmd.system_root = Some(bare.clone());
    only_cmd.comspec = Some(elsewhere.clone());
    let found = only_cmd.find(None, None).expect("a shell");
    assert_eq!(found.kind(), ShellKind::Cmd);
    assert_eq!(found.program(), elsewhere);
    assert_eq!(found.name(), "cmd.exe");
    // A `ComSpec` that names something else is not believed.
    only_cmd.comspec = Some(program(&ws.dir.join("elsewhere"), "not-a-shell.exe"));
    assert_eq!(
        only_cmd.find(None, None).expect("a shell").program(),
        bare_cmd
    );
    only_cmd.comspec = Some(ws.dir.join("missing").join("cmd.exe"));
    assert_eq!(
        only_cmd.find(None, None).expect("a shell").program(),
        bare_cmd
    );
    let _ = system_cmd;

    // Nothing at all.
    let nothing = search(true, &[&ws.dir.join("empty")]).find(None, None);
    assert!(
        matches!(nothing, Err(ShellError::NoneFound(_))),
        "{nothing:?}"
    );
    // And the shells of the other system are not looked for.
    let other = ws.dir.join("linux");
    program(&other, "bash");
    program(&other, "sh");
    assert!(search(true, &[&other]).find(None, None).is_err());
}

#[test]
fn a_configured_shell_is_the_one_used_and_nothing_else() {
    let ws = Workspace::new("search_configured");
    let bin = ws.dir.join("bin");
    program(&bin, "bash");
    let zsh = program(&bin, "zsh");
    let cmd = program(&bin, "cmd.exe");

    // By name, from the path, in place of the default.
    let found = search(false, &[&bin])
        .find(Some("zsh"), None)
        .expect("a shell");
    assert_eq!(found.kind(), ShellKind::Posix);
    assert_eq!(found.program(), zsh);
    assert_eq!(found.name(), "zsh (a POSIX shell)");

    // By path.
    let found = search(false, &[])
        .find(Some(zsh.to_str().expect("text")), None)
        .expect("a shell");
    assert_eq!(found.program(), zsh);

    // On Windows a name is tried with `.exe`, and cmd.exe can be chosen.
    let found = search(true, &[&bin])
        .find(Some("cmd"), None)
        .expect("a shell");
    assert_eq!(found.kind(), ShellKind::Cmd);
    assert_eq!(found.program(), cmd);

    // One that is not there is an error: no other shell is used in its
    // place.
    assert_eq!(
        search(false, &[&bin]).find(Some("fish"), None),
        Err(ShellError::Configured("fish".to_owned()))
    );
    let missing = ws.dir.join("missing").join("bash");
    assert!(matches!(
        search(false, &[&bin]).find(Some(missing.to_str().expect("text")), None),
        Err(ShellError::Configured(_))
    ));

    // Configured arguments come before the command, in place of the
    // kind's own.
    let found = search(false, &[&bin])
        .find(Some("bash"), Some(&["-l".to_owned(), "-c".to_owned()]))
        .expect("a shell");
    let line = found.command_line("true").expect("runnable");
    assert_eq!(line.arguments.len(), 3);
}

/// `target` written relative to this process's current directory, as
/// `./node_modules/.bin` in a user's `PATH` is relative to wherever the
/// harness is started. `None` where it cannot be (another drive).
fn relative_to_here(target: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let here = std::env::current_dir().expect("a current directory");
    // The drive, in any case: `D:` and `d:` are one.
    let anchor = |path: &Path| {
        path.components()
            .take_while(|part| matches!(part, Component::Prefix(_) | Component::RootDir))
            .collect::<PathBuf>()
            .to_string_lossy()
            .to_ascii_lowercase()
    };
    if anchor(&here) != anchor(target) {
        return None;
    }
    let names = |path: &Path| {
        path.components()
            .filter(|part| matches!(part, Component::Normal(_)))
            .count()
    };
    let mut relative: PathBuf = (0..names(&here)).map(|_| "..").collect();
    relative.extend(
        target
            .components()
            .filter(|part| matches!(part, Component::Normal(_))),
    );
    Some(relative)
}

/// A user's `PATH` often starts with a relative entry
/// (`./node_modules/.bin`). It names a directory of wherever the harness
/// was started, which is usually the workspace, so a file there named like
/// the shell would run every command, and be started (for its version) as
/// soon as a session opens. Found by the review of step 13a.
#[test]
fn a_relative_path_entry_does_not_make_a_file_there_the_shell() {
    let ws = Workspace::new("search_relative_entry");
    let planted = ws.dir.join("node_modules").join(".bin");
    let real = ws.dir.join("real");
    for (windows, name) in [(false, "bash"), (true, "pwsh.exe")] {
        program(&planted, name);
        let wanted = program(&real, name);
        let Some(relative) = relative_to_here(&planted) else {
            return;
        };
        assert!(relative.is_relative());
        // The relative entry does lead to the planted file from here.
        assert!(relative.join(name).is_file(), "{}", relative.display());

        let search = ShellSearch {
            windows,
            path: vec![
                PathBuf::new(),
                PathBuf::from("."),
                relative.clone(),
                real.clone(),
            ],
            system_root: None,
            comspec: None,
        };
        let found = search.find(None, None).expect("a shell");
        assert_eq!(
            found.program(),
            wanted,
            "the shell is a file found through a relative PATH entry"
        );
        // And by name, as configuration may give it.
        let configured = search.find(Some(name), None).expect("a shell");
        assert_eq!(configured.program(), wanted);
        // With nothing but relative entries there is no shell.
        let only_relative = ShellSearch {
            path: vec![relative],
            ..search
        };
        assert!(only_relative.find(None, None).is_err());
    }
}

/// The shell configuration names may be a name on `PATH` or an absolute
/// path. A relative path would be found from wherever the harness was
/// started, as a relative entry of `PATH` would.
#[test]
fn a_configured_shell_given_as_a_relative_path_is_refused() {
    let ws = Workspace::new("search_relative_configured");
    let bin = ws.dir.join("bin");
    program(&bin, "bash");
    program(&bin, "pwsh.exe");
    for (windows, configured) in [
        (false, "bin/bash"),
        (false, "./bash"),
        (false, "~/bin/bash"),
        (true, r".\pwsh.exe"),
        (true, r"bin\pwsh.exe"),
    ] {
        assert_eq!(
            search(windows, &[&bin]).find(Some(configured), None),
            Err(ShellError::NotAbsolute(configured.to_owned())),
            "{configured}"
        );
    }
}

/// On Windows the search is the harness's own: `.exe` and the name as
/// given. The other extensions of `PATHEXT` (a batch file named `pwsh`)
/// never make a shell, wherever they are on the path.
#[test]
fn on_windows_only_an_exe_is_taken_for_a_shell() {
    let ws = Workspace::new("search_windows_pathext");
    let first = ws.dir.join("first");
    for name in ["pwsh.bat", "pwsh.cmd", "pwsh.com", "pwsh.ps1"] {
        program(&first, name);
    }
    let second = ws.dir.join("second");
    let pwsh = program(&second, "pwsh.exe");
    let found = search(true, &[&first, &second])
        .find(None, None)
        .expect("a shell");
    assert_eq!(found.program(), pwsh);
    assert_eq!(found.kind(), ShellKind::Pwsh);
}

/// `%ComSpec%` and `%SystemRoot%` are believed only when they are
/// absolute: a relative one names a place from wherever the harness was
/// started.
#[test]
fn a_relative_comspec_or_system_root_is_not_believed() {
    let ws = Workspace::new("search_relative_comspec");
    let planted = ws.dir.join("planted");
    let planted_cmd = program(&planted.join("System32"), "cmd.exe");
    let system = ws.dir.join("system");
    let real_cmd = program(&system.join("System32"), "cmd.exe");
    let (Some(relative_root), Some(relative_cmd)) =
        (relative_to_here(&planted), relative_to_here(&planted_cmd))
    else {
        return;
    };

    let mut found = search(true, &[&ws.dir.join("empty")]);
    found.comspec = Some(relative_cmd);
    found.system_root = Some(system.clone());
    assert_eq!(found.find(None, None).expect("a shell").program(), real_cmd);

    found.comspec = None;
    found.system_root = Some(relative_root);
    assert!(found.find(None, None).is_err(), "a relative %SystemRoot%");
}

#[test]
fn the_real_search_finds_this_systems_shell() {
    let found = ShellSearch::from_env()
        .find(None, None)
        .expect("every system these tests run on has a shell");
    if cfg!(windows) {
        assert!(
            matches!(
                found.kind(),
                ShellKind::Pwsh | ShellKind::WindowsPowerShell | ShellKind::Cmd
            ),
            "{found:?}"
        );
    } else {
        assert!(
            matches!(found.kind(), ShellKind::Bash | ShellKind::Posix),
            "{found:?}"
        );
    }
    assert!(found.program().is_absolute(), "{found:?}");
}
