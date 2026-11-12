//! Resolving a path against the real file system.

#![allow(
    clippy::disallowed_methods,
    reason = "resolving a path asks the file system; async callers run this on the blocking pool"
)]

use std::io;
use std::path::{Path, PathBuf};

use super::lexical::{self, Part};
use super::{AbsPath, PathError, PathStyle, RelPath, root_path, steps};

/// What a [`PathResolver`] needs to know beyond the workspace root.
#[derive(Debug, Clone, Default)]
pub struct PathOptions {
    home: Option<AbsPath>,
    shares: Vec<(String, String)>,
}

impl PathOptions {
    /// No home directory, and no network share allowed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the directory that a leading `~` stands for.
    #[must_use]
    pub fn home(mut self, home: AbsPath) -> Self {
        self.home = Some(home);
        self
    }

    /// Takes the home directory from the environment: `HOME`, or
    /// `USERPROFILE` on Windows. If it is not set, or is not an absolute
    /// path, `~` stays unknown and a path that uses it is refused.
    #[must_use]
    pub fn home_from_env(mut self) -> Self {
        let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        self.home =
            std::env::var_os(variable).and_then(|home| AbsPath::from_os(Path::new(&home)).ok());
        self
    }

    /// Allows paths on one network share, `\\server\share`. Every other
    /// share stays refused. The names are compared without regard to case.
    #[must_use]
    pub fn allow_share(mut self, server: impl Into<String>, share: impl Into<String>) -> Self {
        self.shares.push((server.into(), share.into()));
        self
    }

    /// Refuses a path on a network share that is not on the allow-list.
    /// `written` is the path as the caller wrote it, for the error.
    fn check_share(&self, path: &AbsPath, written: &str) -> Result<(), PathError> {
        let Some((server, share)) = path.share() else {
            return Ok(());
        };
        let style = path.style();
        let allowed = self.shares.iter().any(|(allowed_server, allowed_share)| {
            style.same_name(allowed_server, server) && style.same_name(allowed_share, share)
        });
        if allowed {
            Ok(())
        } else {
            Err(PathError::ShareNotAllowed {
                path: written.to_owned(),
                server: server.to_owned(),
                share: share.to_owned(),
            })
        }
    }
}

/// Resolves paths as written into real locations, for one workspace.
///
/// It always works in the style of the OS it runs on, because it asks that
/// OS's file system.
#[derive(Debug, Clone)]
pub struct PathResolver {
    root: AbsPath,
    options: PathOptions,
}

/// A path after resolution: the real location, and where it is in relation
/// to the workspace.
///
/// "Real" means every symbolic link and Windows junction on the way has been
/// followed, every name is spelled as the file system has it, and on Windows
/// short names (`PROGRA~1`) have been replaced by the long ones. Two paths
/// that lead to the same existing file resolve to equal `ResolvedPath`s, so
/// a rule cannot be dodged through another spelling.
///
/// It describes the file system at the moment of resolution. See the
/// [module](super) documentation for what that does and does not promise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    abs: AbsPath,
    relative: Option<RelPath>,
    exists: bool,
}

impl ResolvedPath {
    /// The real, absolute location. Permission rules are matched against
    /// this.
    #[must_use]
    pub fn abs(&self) -> &AbsPath {
        &self.abs
    }

    /// The location relative to the workspace root, when it is inside the
    /// workspace. `None` means outside: allowed only by permission.
    #[must_use]
    pub fn relative(&self) -> Option<&RelPath> {
        self.relative.as_ref()
    }

    /// Whether the real location is the workspace root or below it.
    #[must_use]
    pub fn is_inside_workspace(&self) -> bool {
        self.relative.is_some()
    }

    /// Whether something existed there when the path was resolved. A path
    /// to a file that is about to be created resolves with `false`: its
    /// existing directories are real, and the rest is as written.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.exists
    }

    /// The path as it is shown to a model: relative to the workspace when
    /// inside it, absolute otherwise, always with `/`.
    #[must_use]
    pub fn display(&self) -> String {
        match &self.relative {
            Some(relative) => relative.to_string(),
            None => self.abs.to_string(),
        }
    }

    /// The path for the standard library's file functions. See
    /// [`AbsPath::to_fs_path`].
    #[must_use]
    pub fn to_fs_path(&self) -> PathBuf {
        self.abs.to_fs_path()
    }
}

impl PathResolver {
    /// A resolver for the workspace at `root`, which must be an absolute
    /// path to an existing directory. The root itself is resolved, so a
    /// workspace opened through a symbolic link is known by its real
    /// location.
    pub fn new(root: &Path, options: PathOptions) -> Result<Self, PathError> {
        let written = AbsPath::from_os(root)?;
        // Before anything is asked of the file system, and so of the network.
        options.check_share(&written, written.as_str())?;
        let real = canonical(&written)?.ok_or_else(|| PathError::Io {
            path: written.to_string(),
            source: io::ErrorKind::NotFound.into(),
        })?;
        options.check_share(&real, written.as_str())?;
        if !is_dir(&real) {
            return Err(PathError::NotADirectory {
                path: real.to_string(),
            });
        }
        Ok(Self {
            root: real,
            options,
        })
    }

    /// The workspace root's real location.
    #[must_use]
    pub fn root(&self) -> &AbsPath {
        &self.root
    }

    /// Resolves a path as a tool received it: relative to the workspace
    /// root, absolute, or starting with `~` for the home directory (`~` or
    /// `~/notes`; `~name` and `./~` are ordinary names).
    ///
    /// - The path is read by the rules of this OS, and refused if it breaks
    ///   them ([`PathError`]). A refused network share is refused before
    ///   anything is asked of the network.
    /// - `..` is resolved as the OS resolves it: against the real directory
    ///   on Linux, where `link/..` is the parent of the link's target, and
    ///   as text on Windows.
    /// - The part of the path that exists is replaced by its real location.
    ///   The part that does not exist yet is kept as written, name-checked.
    /// - A path that leads outside the workspace is not an error. It
    ///   resolves, and [`ResolvedPath::is_inside_workspace`] says so.
    pub fn resolve(&self, input: &str) -> Result<ResolvedPath, PathError> {
        let style = PathStyle::NATIVE;
        let (root, mut parts) = steps(input, style)?;
        let start = match root {
            Some(root) => root_path(style, &root),
            // `~` alone, or `~` and a separator, at the very start. `./~`
            // is a file named `~`.
            None if parts.first() == Some(&Part::Name("~")) && input.starts_with('~') => {
                parts.remove(0);
                self.options.home.clone().ok_or_else(|| PathError::NoHome {
                    path: input.to_owned(),
                })?
            }
            None => self.root.clone(),
        };
        self.options.check_share(&start, input)?;

        let (real, tail) = if style == PathStyle::Windows {
            // Windows resolves `..` as text, against the whole path and
            // stopping at the drive's root, before it opens anything.
            let whole: Vec<Part<'_>> = start.names().map(Part::Name).chain(parts).collect();
            let (_, names) = lexical::collapse(&whole);
            let names: Vec<Part<'_>> = names.into_iter().map(Part::Name).collect();
            let (real, tail) = walk(&start.root(), &names, input)?;
            (
                real,
                tail.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            )
        } else {
            let (real, tail) = walk(&start, &parts, input)?;
            (
                real,
                tail.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            )
        };
        let exists = tail.is_empty();
        let mut abs = real;
        for name in tail {
            // Each name was checked when the path was read.
            abs = abs.join_rel(&RelPath::from_text(style, name));
        }
        // Again on the real location: a drive letter can be mapped to a
        // share, and a link can lead to one.
        self.options.check_share(&abs, input)?;
        let relative = abs.strip_prefix(&self.root);
        Ok(ResolvedPath {
            abs,
            relative,
            exists,
        })
    }
}

/// Follows `parts` from `start` through the file system. Returns the real
/// location of the part that exists, and the names after it that do not.
/// `written` is the path as the caller wrote it, for errors.
fn walk<'a>(
    start: &AbsPath,
    parts: &[Part<'a>],
    written: &str,
) -> Result<(AbsPath, Vec<&'a str>), PathError> {
    // The usual case: the whole path exists, and the OS resolves it in one
    // call, `..` included.
    let mut whole = start.to_fs_path();
    for part in parts {
        match part {
            Part::Name(name) => whole.push(name),
            Part::Parent => whole.push(".."),
        }
    }
    if let Some(real) = canonical_os(&whole, written)? {
        return Ok((real, Vec::new()));
    }

    // Some of it does not exist. Go one step at a time, to find where the
    // real part ends and to resolve everything before that point.
    let mut real = canonical(start)?.ok_or_else(|| PathError::Io {
        path: start.to_string(),
        source: io::ErrorKind::NotFound.into(),
    })?;
    let mut tail: Vec<&str> = Vec::new();
    for part in parts {
        match part {
            Part::Parent => {
                if tail.pop().is_none()
                    && let Some(parent) = real.parent()
                {
                    real = parent;
                }
            }
            Part::Name(name) if !tail.is_empty() => tail.push(name),
            Part::Name(name) => {
                let candidate =
                    real.join_rel(&RelPath::from_text(real.style(), (*name).to_owned()));
                if let Some(next) = canonical(&candidate)? {
                    real = next;
                    continue;
                }
                // Nothing resolves there. If something is there all the
                // same, it is a link to nowhere, and writing "through" it
                // would create a file at a place the path does not show.
                if std::fs::symlink_metadata(candidate.to_fs_path()).is_ok() {
                    return Err(PathError::DanglingLink {
                        path: candidate.to_string(),
                    });
                }
                // New names can only be created inside a directory.
                if !is_dir(&real) {
                    return Err(PathError::Io {
                        path: candidate.to_string(),
                        source: io::ErrorKind::NotADirectory.into(),
                    });
                }
                tail.push(name);
            }
        }
    }
    Ok((real, tail))
}

/// The real location of `path`, or `None` if nothing is there.
fn canonical(path: &AbsPath) -> Result<Option<AbsPath>, PathError> {
    canonical_os(&path.to_fs_path(), path.as_str())
}

/// The real location of the file-system path `path`, or `None` if nothing
/// is there. `shown` names the path in an error.
fn canonical_os(path: &Path, shown: &str) -> Result<Option<AbsPath>, PathError> {
    match std::fs::canonicalize(path) {
        Ok(real) => AbsPath::from_os(&real).map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PathError::Io {
            path: shown.to_owned(),
            source,
        }),
    }
}

fn is_dir(path: &AbsPath) -> bool {
    std::fs::metadata(path.to_fs_path()).is_ok_and(|metadata| metadata.is_dir())
}
