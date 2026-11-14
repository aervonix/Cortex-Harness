//! Finding a program the harness starts by name: its shell, and git
//! (`docs/architecture/05-tools.md`, "Which shell").
//!
//! **Only the absolute directories of `PATH` are looked in.** An empty or
//! relative entry is common (`./node_modules/.bin`) and names a directory
//! of wherever the harness was started, which is usually the workspace. A
//! file there named `bash` or `git` would otherwise run in place of the
//! real one, as soon as a session opens and before anything is asked. A
//! program started by its bare name has the same flaw twice over: on Linux
//! the child changes to its working directory and then searches the
//! parent's `PATH`, so a relative entry is read from the workspace; and on
//! Windows the system's search also looks in the current directory. So the
//! harness finds the program itself, here, and starts it by the absolute
//! path found.
//!
//! The search is the harness's own on Windows too: a name is tried with
//! `.exe` and as it is given, never with the other extensions of
//! `PATHEXT` (a `git.bat` is not git).

#![allow(
    clippy::disallowed_methods,
    reason = "looking at the file system blocks; callers run it off the core thread"
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The directories of this process's `PATH`, in order, as the system splits
/// them. The relative ones are among them: [`find`] passes over them.
#[must_use]
pub fn path_directories() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

/// The first program called `name` in the absolute directories of
/// `directories`. With `windows`, by Windows's rules: `name.exe` first when
/// `name` has no extension, then `name` itself, and any file counts as a
/// program. A candidate that is not an absolute path (a name that is itself
/// a path with a drive and no root, `C:git.exe`) is never taken.
#[must_use]
pub fn find(directories: &[PathBuf], name: &str, windows: bool) -> Option<PathBuf> {
    let mut names = vec![OsString::from(name)];
    if windows && Path::new(name).extension().is_none() {
        names.insert(0, OsString::from(format!("{name}.exe")));
    }
    directories
        .iter()
        .filter(|directory| directory.is_absolute())
        .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
        .find(|candidate| candidate.is_absolute() && is_program(candidate, windows))
}

/// The first `name` in the absolute directories of `directories` whose
/// real path is not inside any of `avoid`: a session's write roots, where a
/// confined command could have planted a program the harness starts (found
/// by the third review of step 26). `avoid` holds real paths; a candidate
/// is compared by its real path, so a link into a root is caught. With no
/// `avoid` roots this is [`find`].
#[must_use]
#[allow(
    clippy::disallowed_methods,
    reason = "resolving a candidate's path is part of choosing it; callers run it off the core \
              thread"
)]
pub fn find_outside(
    directories: &[PathBuf],
    name: &str,
    windows: bool,
    avoid: &[PathBuf],
) -> Option<PathBuf> {
    let mut names = vec![OsString::from(name)];
    if windows && Path::new(name).extension().is_none() {
        names.insert(0, OsString::from(format!("{name}.exe")));
    }
    directories
        .iter()
        .filter(|directory| directory.is_absolute())
        .flat_map(|directory| names.iter().map(move |name| directory.join(name)))
        .find(|candidate| {
            if !candidate.is_absolute() || !is_program(candidate, windows) {
                return false;
            }
            let real = std::fs::canonicalize(candidate).unwrap_or_else(|_| candidate.clone());
            !avoid.iter().any(|root| real.starts_with(root))
        })
}

/// Whether `path` is a file that can be run: a regular file, and on Linux
/// one with a permission to run it. On Windows any file counts.
#[must_use]
pub fn is_program(path: &Path, windows: bool) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    if !windows {
        use std::os::unix::fs::PermissionsExt;
        return metadata.permissions().mode() & 0o111 != 0;
    }
    let _ = windows;
    true
}

/// Where `git` is, for every git the harness starts itself: found once in
/// the life of the process, in the absolute directories of its `PATH`.
/// `None` when there is none, which a caller treats as git not being
/// installed.
#[must_use]
pub fn git() -> Option<&'static Path> {
    static GIT: OnceLock<Option<PathBuf>> = OnceLock::new();
    GIT.get_or_init(|| find(&path_directories(), "git", cfg!(windows)))
        .as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::{Rng, SystemRng};

    fn scratch(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cortex-base-program-{test}-{:016x}",
            SystemRng::new().next_u64()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn program(directory: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(directory).unwrap();
        let path = directory.join(name);
        std::fs::write(&path, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    /// `target` written relative to this process's current directory, as a
    /// relative entry of a user's `PATH` is. `None` on another drive.
    fn relative_to_here(target: &Path) -> Option<PathBuf> {
        use std::path::Component;
        let here = std::env::current_dir().unwrap();
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

    /// A relative entry that leads to a planted `git` is passed over, on
    /// either system's rules, and an absolute one after it is taken. Found
    /// by the independent review of the fixes to step 13a: the harness
    /// started `git` by its bare name, so the system searched the relative
    /// entry.
    #[test]
    fn a_relative_path_entry_does_not_make_a_file_there_the_program() {
        let dir = scratch("relative");
        let planted = dir.join("node_modules").join(".bin");
        let real = dir.join("real");
        for (windows, name) in [(false, "git"), (true, "git.exe")] {
            program(&planted, name);
            let wanted = program(&real, name);
            let Some(relative) = relative_to_here(&planted) else {
                return;
            };
            assert!(relative.join(name).is_file(), "{}", relative.display());
            let directories = [
                PathBuf::new(),
                PathBuf::from("."),
                relative.clone(),
                real.clone(),
            ];
            assert_eq!(find(&directories, "git", windows), Some(wanted), "{name}");
            assert_eq!(find(&[relative], "git", windows), None, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// On Windows a name is tried with `.exe` and as given: never as a
    /// batch file or a script, wherever one is on the path.
    #[test]
    fn on_windows_only_an_exe_or_the_name_itself_is_taken() {
        let dir = scratch("pathext");
        let first = dir.join("first");
        for name in ["git.bat", "git.cmd", "git.com", "git.ps1"] {
            program(&first, name);
        }
        let exe = program(&dir.join("second"), "git.exe");
        assert_eq!(find(&[first, dir.join("second")], "git", true), Some(exe));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_is_found_where_the_system_has_it() {
        // Every system these tests run on has git, on an absolute entry.
        let git = git().expect("git on PATH");
        assert!(git.is_absolute(), "{}", git.display());
    }

    /// A program inside a write root (where a confined command could have
    /// planted it) is passed over, and the next one on the path is taken;
    /// when nothing is left outside the roots, there is none.
    #[test]
    fn find_outside_passes_over_a_program_inside_a_root() {
        let dir = scratch("outside");
        let root = dir.join("ws").join("bin");
        let clean = dir.join("usr").join("bin");
        for windows in [false, true] {
            let name = if windows { "git.exe" } else { "git" };
            let planted = program(&root, name);
            let real = program(&clean, name);
            let path = [root.clone(), clean.clone()];
            // With the root avoided, the planted one is passed over.
            assert_eq!(
                find_outside(&path, "git", windows, &[dir.join("ws")]),
                Some(real.clone()),
                "{name}"
            );
            // With nothing avoided, the first is taken: that is `find`.
            assert_eq!(
                find_outside(&path, "git", windows, &[]),
                Some(planted.clone())
            );
            assert_eq!(
                find(&path, "git", windows),
                find_outside(&path, "git", windows, &[])
            );
            // With only the planted one, and the root avoided, there is
            // none.
            assert_eq!(
                find_outside(&[root.clone()], "git", windows, &[dir.join("ws")]),
                None
            );
            let _ = std::fs::remove_file(&real);
            let _ = std::fs::remove_file(&planted);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
