//! Resolving paths on Windows: the "Paths" row of the Windows coverage in
//! `docs/testing-and-ci.md`, and the rows of "Windows differences in one
//! place" in `docs/architecture/05-tools.md` that concern paths.
//!
//! These run only on the Windows CI job. Each refusal here closes a way of
//! reaching a file under a name a permission rule would not recognise.

use std::fs;
use std::os::windows::fs::symlink_dir;
use std::path::Path;

use cortex_base::{PathError, PathOptions, PathResolver};

use crate::Scratch;

/// Whether this is the CI job, where every Windows case must really run.
fn in_ci() -> bool {
    std::env::var_os("GITHUB_ACTIONS").is_some()
}

/// Makes a directory symbolic link. That needs a privilege an ordinary
/// account lacks unless Developer Mode is on. The CI runner has it, and
/// there the link must be made. At a desk without it, the test says so and
/// stops.
fn link_dir_or_skip(target: &Path, link: &Path) -> bool {
    match symlink_dir(target, link) {
        Ok(()) => true,
        Err(error) if !in_ci() => {
            println!("skipped: cannot make a directory link here ({error})");
            false
        }
        Err(error) => panic!("the CI runner cannot make a directory link: {error}"),
    }
}

#[test]
fn both_separators_and_any_case_reach_the_same_file() {
    let scratch = Scratch::new("win_case");
    scratch.file("ws/Src/Main.rs", "fn main() {}");
    let resolver = scratch.resolver("ws");
    let expected = resolver.resolve("Src/Main.rs").unwrap();
    assert!(expected.exists());
    assert_eq!(expected.display(), "Src/Main.rs");

    for spelling in [
        r"Src\Main.rs",
        "src/main.rs",
        r"SRC\MAIN.RS",
        r".\src\.\MAIN.rs",
        "sRc//mAiN.rS",
        r"src\..\SRC/main.rs",
    ] {
        let resolved = resolver.resolve(spelling).unwrap();
        assert_eq!(resolved, expected, "{spelling:?}");
        // The spelling on disk is what is kept and shown, with `/`.
        assert_eq!(resolved.display(), "Src/Main.rs", "{spelling:?}");
        assert_eq!(resolved.abs().as_str(), expected.abs().as_str());
        assert!(!resolved.abs().as_str().contains('\\'));
    }

    // The absolute path, in another case and with either separator.
    let absolute = scratch.join("ws").join("Src").join("Main.rs");
    let absolute = absolute.to_str().unwrap();
    for spelling in [
        absolute.to_owned(),
        absolute.to_lowercase(),
        absolute.to_uppercase(),
        absolute.replace('\\', "/"),
    ] {
        assert_eq!(
            resolver.resolve(&spelling).unwrap(),
            expected,
            "{spelling:?}"
        );
    }

    // A file that does not exist yet: the directories that exist take their
    // real spelling, and the new name stays as written.
    let new = resolver.resolve(r"SRC\New.rs").unwrap();
    assert!(!new.exists());
    assert_eq!(new.display(), "Src/New.rs");
    assert_eq!(
        new,
        resolver.resolve("src/NEW.RS").unwrap(),
        "compared without case"
    );
}

#[test]
fn the_workspace_root_is_known_by_its_real_spelling() {
    let scratch = Scratch::new("win_root");
    scratch.dir("WorkSpace");
    let real = scratch.real("WorkSpace");
    for spelling in [
        scratch.join("workspace"),
        scratch.join("WORKSPACE"),
        scratch.join("WorkSpace").join("."),
    ] {
        let resolver = PathResolver::new(&spelling, PathOptions::new()).unwrap();
        assert_eq!(resolver.root(), &real);
        assert_eq!(
            resolver.root().as_str(),
            real.as_str(),
            "the spelling on disk"
        );
        assert!(resolver.root().as_str().ends_with("/WorkSpace"));
    }
}

#[test]
fn device_names_are_refused() {
    let scratch = Scratch::new("win_devices");
    let resolver = scratch.resolver("ws");
    let inside = scratch.join("ws");
    let inside = inside.to_str().unwrap();

    for name in [
        "NUL",
        "nul",
        "CON",
        "con.txt",
        "PRN",
        "aux.c",
        "COM1",
        "com9.log",
        "LPT1",
        "LPT0",
        "COM¹",
        "CONIN$",
        "CONOUT$",
        "NUL.tar.gz",
        "NUL .txt",
        r"sub\NUL",
        "sub/con/file.txt",
    ] {
        for written in [name.to_owned(), format!(r"{inside}\{name}")] {
            let error = resolver.resolve(&written).unwrap_err();
            assert!(
                matches!(error, PathError::ReservedName { .. }),
                "{written:?}: {error}"
            );
        }
    }
    for written in [
        r"\\.\NUL",
        r"\\.\C:\Windows",
        r"\\.\PhysicalDrive0",
        r"\\.\pipe\cortex",
        r"\\?\C:\Windows",
        r"\\?\UNC\localhost\c$",
        "//./NUL",
        "//?/C:/Windows",
    ] {
        let error = resolver.resolve(written).unwrap_err();
        assert!(
            matches!(error, PathError::DevicePath { .. }),
            "{written:?}: {error}"
        );
    }
    assert!(
        scratch.listing("ws").is_empty(),
        "nothing was created on the way"
    );

    // Names that only look like devices are files like any other.
    for name in [
        "COM10",
        "nul1.txt",
        "CONSOLE",
        "a.NUL",
        "LPT",
        "auxiliary.rs",
    ] {
        scratch.file(&format!("ws/{name}"), "x");
        let resolved = resolver.resolve(name).unwrap();
        assert!(resolved.exists(), "{name:?}");
        assert_eq!(resolved.display(), name);
    }
}

#[test]
fn names_that_windows_would_read_as_another_file_are_refused() {
    let scratch = Scratch::new("win_aliases");
    let secret = scratch.file("ws/secret.env", "TOKEN=1");
    let resolver = scratch.resolver("ws");

    // Why it matters: Windows opens the same file under these other names.
    // A rule that denies `secret.env` would not match them as text.
    let with_dot = scratch.join("ws").join("secret.env.");
    assert_eq!(
        fs::read_to_string(&with_dot).unwrap(),
        "TOKEN=1",
        "Windows drops the trailing dot"
    );
    let stream = format!("{}::$DATA", secret.display());
    assert_eq!(
        fs::read_to_string(&stream).unwrap(),
        "TOKEN=1",
        "`::$DATA` is the file's own content"
    );

    for written in [
        "secret.env.",
        "secret.env ",
        "secret.env. .",
        "SECRET.ENV...",
        "ws./x",
    ] {
        let error = resolver.resolve(written).unwrap_err();
        assert!(
            matches!(error, PathError::TrailingDotOrSpace { .. }),
            "{written:?}: {error}"
        );
    }
    for written in [
        "secret.env::$DATA",
        "secret.env:hidden",
        "SECRET.ENV::$data",
        "secret.env:Zone.Identifier",
        "what?.env",
        "*.env",
        "secret<.env",
        "a|b",
        "tab\tname",
    ] {
        let error = resolver.resolve(written).unwrap_err();
        assert!(
            matches!(error, PathError::InvalidCharacter { .. }),
            "{written:?}: {error}"
        );
    }
    // The absolute spellings are refused the same way.
    for suffix in [".", " ", "::$DATA", ":s"] {
        let written = format!("{}{suffix}", secret.display());
        assert!(resolver.resolve(&written).is_err(), "{written:?}");
    }
    assert_eq!(scratch.listing("ws"), ["secret.env"]);
}

#[test]
fn paths_that_depend_on_an_unstated_current_directory_are_refused() {
    let scratch = Scratch::new("win_relative_forms");
    let resolver = scratch.resolver("ws");
    for written in ["C:", "C:file.txt", r"c:dir\file", "D:..", "Z:x"] {
        let error = resolver.resolve(written).unwrap_err();
        assert!(
            matches!(error, PathError::DriveRelative { .. }),
            "{written:?}: {error}"
        );
    }
    for written in [r"\Windows\System32", "/etc/passwd", r"\", "/"] {
        let error = resolver.resolve(written).unwrap_err();
        assert!(
            matches!(error, PathError::RootRelative { .. }),
            "{written:?}: {error}"
        );
    }
}

#[test]
fn network_shares_are_refused_unless_the_share_is_allowed() {
    let scratch = Scratch::new("win_unc");
    let resolver = scratch.resolver("ws");

    // The server does not exist. If the refusal asked the network anything,
    // this test would take seconds, and it would send the account's
    // credentials to whoever answered. It is refused as text.
    for written in [
        r"\\cortex-no-such-host.invalid\share\file.txt",
        r"\\cortex-no-such-host.invalid\share",
        "//cortex-no-such-host.invalid/share/dir/file.txt",
        r"\\localhost\c$\Windows",
        r"\\127.0.0.1\c$",
    ] {
        match resolver.resolve(written).unwrap_err() {
            PathError::ShareNotAllowed { server, share, .. } => {
                assert!(!server.is_empty() && !share.is_empty(), "{written:?}");
            }
            other => panic!("{written:?}: {other}"),
        }
    }
    for written in [r"\\server", r"\\server\", r"\\\share\x"] {
        let error = resolver.resolve(written).unwrap_err();
        assert!(
            matches!(error, PathError::MalformedShare { .. }),
            "{written:?}: {error}"
        );
    }
    // A workspace on a share is refused too.
    assert!(matches!(
        PathResolver::new(
            Path::new(r"\\cortex-no-such-host.invalid\share\ws"),
            PathOptions::new()
        ),
        Err(PathError::ShareNotAllowed { .. })
    ));

    // With one share allowed, that share is let through and no other.
    let allowing = scratch.resolver_with("ws", PathOptions::new().allow_share("LOCALHOST", "C$"));
    assert!(matches!(
        allowing.resolve(r"\\localhost\d$\x"),
        Err(PathError::ShareNotAllowed { .. })
    ));
    assert!(matches!(
        allowing.resolve(r"\\otherhost\c$\x"),
        Err(PathError::ShareNotAllowed { .. })
    ));
    match allowing.resolve(r"\\localhost\c$\Windows") {
        Ok(resolved) => {
            let (server, share) = resolved.abs().share().expect("still a share path");
            assert!(server.eq_ignore_ascii_case("localhost") && share.eq_ignore_ascii_case("c$"));
            assert!(!resolved.is_inside_workspace());
        }
        // The machine may not offer its administrative share. What matters
        // is that the refusal is no longer ours.
        Err(PathError::Io { .. }) => {}
        Err(other) => panic!("an allowed share was refused: {other}"),
    }
}

#[test]
fn a_directory_link_that_leaves_the_workspace_resolves_outside() {
    let scratch = Scratch::new("win_symlink_out");
    scratch.file("outside/deep/secret.txt", "s");
    scratch.file("ws/beside.txt", "inside");
    if !link_dir_or_skip(
        &scratch.join("outside").join("deep"),
        &scratch.join("ws").join("vendor"),
    ) {
        return;
    }
    let resolver = scratch.resolver("ws");

    let resolved = resolver.resolve(r"vendor\secret.txt").unwrap();
    assert_eq!(resolved.abs(), &scratch.real("outside/deep/secret.txt"));
    assert!(
        !resolved.is_inside_workspace(),
        "the check applies to the target"
    );
    let resolved = resolver.resolve("VENDOR/new.txt").unwrap();
    assert!(!resolved.exists());
    assert!(
        !resolved.is_inside_workspace(),
        "a new file there would be created outside"
    );
    assert_eq!(
        resolved.abs(),
        &scratch.real("outside/deep").join("new.txt").unwrap()
    );

    // Windows resolves `..` as text, before it follows any link: after
    // `vendor\..` the path is back in the workspace. The resolver agrees
    // with what Windows would open.
    let resolved = resolver.resolve(r"vendor\..\beside.txt").unwrap();
    assert!(resolved.is_inside_workspace());
    assert_eq!(resolved.display(), "beside.txt");
    assert_eq!(
        fs::read_to_string(
            scratch
                .join("ws")
                .join("vendor")
                .join("..")
                .join("beside.txt")
        )
        .unwrap(),
        "inside",
        "and that is the file Windows opens"
    );
}

/// A junction is the kind of directory link any account can make, so it is
/// the one an untrusted repository or command is most likely to plant. It
/// is made with `mklink`, which is a process: hence the `sys_` name.
#[test]
fn sys_a_junction_that_leaves_the_workspace_resolves_outside() {
    let scratch = Scratch::new("win_junction_out");
    scratch.file("outside/secret.txt", "s");
    scratch.dir("ws/sub");
    let link = scratch.join("ws").join("sub").join("vendor");
    let made = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(scratch.join("outside"))
        .output()
        .expect("run mklink");
    assert!(
        made.status.success(),
        "mklink failed: {}",
        String::from_utf8_lossy(&made.stderr)
    );
    let resolver = scratch.resolver("ws");

    for written in [
        r"sub\vendor\secret.txt",
        "SUB/VENDOR/SECRET.TXT",
        r"sub\.\vendor\secret.txt",
    ] {
        let resolved = resolver.resolve(written).unwrap();
        assert_eq!(
            resolved.abs(),
            &scratch.real("outside/secret.txt"),
            "{written:?}"
        );
        assert!(
            !resolved.is_inside_workspace(),
            "{written:?} leaves the workspace"
        );
        assert!(resolved.exists());
    }
    let directory = resolver.resolve(r"sub\vendor").unwrap();
    assert_eq!(directory.abs(), &scratch.real("outside"));
    assert!(!directory.is_inside_workspace());

    let new = resolver.resolve(r"sub\vendor\made\later.txt").unwrap();
    assert!(!new.exists());
    assert!(!new.is_inside_workspace());
    assert_eq!(
        new.abs(),
        &scratch.real("outside").join("made/later.txt").unwrap()
    );

    // A junction to a place that is gone is refused: as a link to nothing,
    // or by Windows itself.
    fs::remove_file(scratch.join("outside").join("secret.txt")).unwrap();
    fs::remove_dir(scratch.join("outside")).unwrap();
    for written in [r"sub\vendor", r"sub\vendor\x.txt"] {
        let error = resolver.resolve(written).unwrap_err();
        assert!(
            matches!(error, PathError::DanglingLink { .. } | PathError::Io { .. }),
            "{written:?}: {error}"
        );
    }
}

#[test]
fn a_short_name_resolves_to_the_same_path_as_the_long_one() {
    // Short ("8.3") names are a per-volume setting, so try the build's
    // volume and then the temporary directory's.
    let volumes = [
        Path::new(env!("CARGO_TARGET_TMPDIR")).to_path_buf(),
        std::env::temp_dir(),
    ];
    for volume in &volumes {
        let scratch = Scratch::under(volume, "win_short_names");
        scratch.file("ws/secret-settings.env", "TOKEN=1");
        scratch.file("ws/long-directory-name/inner-file-name.txt", "x");
        let short_file = scratch.join("ws").join("SECRET~1.ENV");
        if fs::read_to_string(&short_file).ok().as_deref() != Some("TOKEN=1") {
            println!("no short names on {}", volume.display());
            continue;
        }

        let resolver = scratch.resolver("ws");
        let long = resolver.resolve("secret-settings.env").unwrap();
        for short in ["SECRET~1.ENV", "secret~1.env", r".\Secret~1.Env"] {
            let resolved = resolver.resolve(short).unwrap();
            assert_eq!(resolved, long, "{short:?}");
            assert_eq!(
                resolved.display(),
                "secret-settings.env",
                "the long name is the path"
            );
            assert_eq!(
                resolved.abs().key(),
                long.abs().key(),
                "so a rule on it matches"
            );
        }
        let long = resolver
            .resolve("long-directory-name/inner-file-name.txt")
            .unwrap();
        let resolved = resolver.resolve(r"LONG-D~1\INNER-~1.TXT").unwrap();
        assert_eq!(resolved, long);
        assert_eq!(
            resolved.display(),
            "long-directory-name/inner-file-name.txt"
        );
        // A new file in a directory reached by its short name.
        let new = resolver.resolve(r"LONG-D~1\new.txt").unwrap();
        assert_eq!(new.display(), "long-directory-name/new.txt");
        return;
    }
    assert!(
        !in_ci(),
        "no volume on the CI runner makes short names, so this case did not run"
    );
    println!("skipped: no volume here makes short names");
}
