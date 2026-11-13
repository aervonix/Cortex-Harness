//! Integration tests (tier 2): configuration and credentials on a real file
//! system.
//!
//! **No test here can see the real home directory.** Every test builds its
//! own [`Env`], whose home directory is a folder of its own under Cargo's
//! temporary directory, and the managed file, which has a fixed place on
//! Linux, is pointed into that folder too. Nothing reads the process's
//! environment. Credentials are made-up strings, and the files of other
//! tools are empty fixtures.
//!
//! No test waits: time comes from the fake clock, and file times are set,
//! not waited for.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    reason = "an integration test prepares real files, fails by panicking, and says when it skips"
)]

mod credentials;
mod discovery;
mod generated;
mod layers;
mod loading;
mod measure;
mod printing;
mod provenance;
mod reloading;
mod state;
mod trust;
mod workspace;

use std::fs;
use std::path::{Path, PathBuf};

use cortex_config::{Env, LayerKind, LoadFailure, LoadOptions, Loaded, Platform, ProjectTrust};
use cortex_testkit::Scratch;

/// Writes a file, creating its directory.
fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create the file's directory");
    }
    fs::write(path, contents).expect("write a file");
}

/// Makes a symbolic link to a file or, with `directory`, to a folder. On
/// Windows that needs a privilege an ordinary account lacks unless
/// Developer Mode is on; the CI runner has it, and there the link must be
/// made. Elsewhere `false` when it could not be, and the test says so and
/// stops. Making a link asks nothing of what it names.
fn make_link(target: &Path, link: &Path, directory: bool) -> bool {
    #[cfg(unix)]
    let made = {
        let _ = directory;
        std::os::unix::fs::symlink(target, link)
    };
    #[cfg(windows)]
    let made = if directory {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    };
    match made {
        Ok(()) => true,
        Err(error) if std::env::var_os("GITHUB_ACTIONS").is_none() => {
            println!("skipped: cannot make a link here ({error})");
            false
        }
        Err(error) => panic!("the CI runner cannot make a link: {error}"),
    }
}

/// A network share that does not exist. A test that followed a link to it
/// would wait for the name to fail, and a refusal made on the text takes
/// no time.
#[cfg(windows)]
const NO_SUCH_SHARE: &str = r"\\cortex-no-such-host.invalid\share";

/// A made-up machine: a home directory, a workspace inside it, and a
/// managed file inside it too, so that every path a test prints starts
/// with `~` and is the same on every machine.
struct Machine {
    scratch: Scratch,
    options: LoadOptions,
}

impl Machine {
    fn new(test: &str) -> Self {
        let scratch = cortex_testkit::scratch!(test);
        let home = scratch.join("home");
        fs::create_dir_all(&home).unwrap();
        let home_variable = match Platform::CURRENT {
            Platform::Unix => "HOME",
            Platform::Windows => "USERPROFILE",
        };
        let env = Env::empty(Platform::CURRENT)
            .with(home_variable, home.to_str().unwrap())
            // The user's file in one place on both OSes, so that origins
            // print alike. The default places are tested with the
            // directory rules.
            .with(
                "CORTEX_CONFIG",
                home.join("cortex").join("config.toml").to_str().unwrap(),
            );
        let mut options = LoadOptions::new(env);
        options.dirs.managed_config = Some(home.join("managed").join("config.toml"));
        options.workspace = Some(home.join("work").join("app"));
        fs::create_dir_all(options.workspace.as_ref().unwrap()).unwrap();
        Self { scratch, options }
    }

    fn home(&self) -> PathBuf {
        self.scratch.join("home")
    }

    fn env(&self) -> &Env {
        &self.options.env
    }

    /// The file of a layer.
    fn file(&self, layer: LayerKind) -> PathBuf {
        self.options
            .files()
            .into_iter()
            .find(|(kind, _)| *kind == layer)
            .map(|(_, file)| file)
            .expect("a file layer")
    }

    /// Writes the file of a layer. An empty text removes the file.
    fn layer(&self, layer: LayerKind, text: &str) -> &Self {
        let file = self.file(layer);
        if text.is_empty() {
            let _ = fs::remove_file(file);
        } else {
            write(&file, text);
        }
        self
    }

    fn trusted(&mut self) -> &mut Self {
        self.options.trust = ProjectTrust::Trusted;
        self
    }

    fn try_load(&self) -> Result<Loaded, LoadFailure> {
        cortex_config::load(&self.options)
    }

    fn load(&self) -> Loaded {
        self.try_load()
            .unwrap_or_else(|failure| panic!("load failed:\n{}", failure.render(self.env())))
    }

    /// The diagnostics of a load that worked, a line each.
    fn said(&self, loaded: &Loaded) -> Vec<String> {
        loaded
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.render(self.env()))
            .collect()
    }

    /// The diagnostics of a load that failed, a line each.
    fn failure(&self) -> Vec<String> {
        let failure = self.try_load().expect_err("the load should fail");
        failure
            .render(self.env())
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// The effective value of a key, written as TOML.
fn value(loaded: &Loaded, key: &str) -> String {
    loaded
        .value(&cortex_config::KeyPath::parse(key).unwrap())
        .map_or_else(|| "(not set)".to_owned(), |value| value.to_string())
}

/// The layer the effective value of a key came from.
fn layer_of(loaded: &Loaded, key: &str) -> &'static str {
    loaded
        .origin(&cortex_config::KeyPath::parse(key).unwrap())
        .layer
        .as_str()
}

/// Compares text with a checked-in snapshot. With `CORTEX_UPDATE_SNAPSHOTS`
/// set, the snapshot is written instead, to be reviewed like code.
fn assert_snapshot(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("it")
        .join("snapshots")
        .join(name);
    if std::env::var_os("CORTEX_UPDATE_SNAPSHOTS").is_some() {
        write(&path, actual);
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("no snapshot {}: {error}", path.display()))
        // A checkout on Windows may have turned the line endings.
        .replace("\r\n", "\n");
    assert!(
        expected == actual,
        "{name} differs from its snapshot.\n--- snapshot\n{expected}\n--- now\n{actual}\n\
         If the change is intended, run the test with CORTEX_UPDATE_SNAPSHOTS=1 and review the \
         difference."
    );
}
