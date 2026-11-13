//! Loading configuration: find the files, read and check each layer, merge
//! them, and read the settings from the result
//! (`docs/architecture/08-configuration.md`).
//!
//! [`load`] either returns a [`Loaded`] configuration or a [`LoadFailure`]
//! that lists everything that is wrong, in every layer, so that one run
//! shows it all. There is no partly loaded configuration: a caller that
//! already has one keeps it (see [`crate::LiveConfig`]).
//!
//! Loading blocks: it reads up to four small files. It runs at start-up
//! and, for a reload, on the blocking pool.

#![allow(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "reads the configuration files; runs at start-up and on the blocking pool"
)]

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use cortex_protocol::lease::Rule;
use toml::Value;

use crate::dirs::{Dirs, ProjectFiles};
use crate::document::{self, Document};
use crate::env::Env;
use crate::key::KeyPath;
use crate::layer::{Diagnostic, LayerKind, Origin, Severity, Withheld};
use crate::merge::{self, LayerView, Merger, Source};
use crate::schema::{self, Effect, Lookup, Meta, Setting, Settings};
use crate::validate::{self, Checked};

/// A configuration file larger than this is refused. A real one is a few
/// kilobytes.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// Whether the user has trusted the project
/// (`docs/architecture/06-permissions-and-sandbox.md`, "Project trust").
/// The answer is the caller's: this crate does not keep the trust store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectTrust {
    /// Not trusted: project settings that can run code or widen access are
    /// held back.
    Untrusted,
    /// Trusted: they apply.
    Trusted,
}

/// One setting given on the command line.
///
/// Its `Debug` form shows the key and the flag, and not the value: a value
/// typed on the command line may be a provider's header, with a key in it.
#[derive(Clone, PartialEq, Eq)]
pub struct Override {
    /// The key, as written: `model.main`.
    pub key: String,
    /// The value, as written. It is read as TOML when the key takes that,
    /// and as plain text otherwise, so `-c model.main=big-model` needs no
    /// quotes.
    pub value: String,
    /// The dedicated flag this came from, such as `--model`. `None` for
    /// `-c key=value`.
    pub flag: Option<String>,
}

impl fmt::Debug for Override {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Override")
            .field("key", &self.key)
            .field("value", &cortex_base::secret::REDACTED)
            .field("flag", &self.flag)
            .finish()
    }
}

impl Override {
    /// Reads `key=value`, the argument of `-c`.
    ///
    /// # Errors
    ///
    /// When there is no `=` or nothing before it.
    pub fn parse(argument: &str) -> Result<Self, String> {
        match argument.split_once('=') {
            Some((key, value)) if !key.trim().is_empty() => Ok(Self {
                key: key.trim().to_owned(),
                value: value.trim().to_owned(),
                flag: None,
            }),
            _ => Err(format!(
                "`{argument}` is not a setting: write it as `key=value`, for example \
                 `model.main=some-model`"
            )),
        }
    }
}

/// Everything [`load`] works from. Nothing else is consulted: not the
/// process's environment, and not its working directory.
#[derive(Debug, Clone)]
pub struct LoadOptions {
    /// The environment.
    pub env: Env,
    /// Where the files are. [`LoadOptions::new`] works it out from the
    /// environment. A test points the managed file, which has a fixed place
    /// on Linux, somewhere of its own.
    pub dirs: Dirs,
    /// The workspace root, for the two project layers. `None` loads none.
    pub workspace: Option<PathBuf>,
    /// Whether the project is trusted.
    pub trust: ProjectTrust,
    /// The profile chosen with `--profile`. It wins over the `profile` key.
    pub profile: Option<String>,
    /// Settings given on the command line, applied in order.
    pub overrides: Vec<Override>,
    /// Whether an unknown key is an error: `--strict-config`, and
    /// `cortex config check`.
    pub strict: bool,
}

impl LoadOptions {
    /// Options with no workspace, no profile and no overrides.
    #[must_use]
    pub fn new(env: Env) -> Self {
        Self {
            dirs: Dirs::from_env(&env),
            env,
            workspace: None,
            trust: ProjectTrust::Untrusted,
            profile: None,
            overrides: Vec::new(),
            strict: false,
        }
    }

    /// The files a load reads, lowest layer first, whether or not they
    /// exist. A file that does not exist is still worth watching: creating
    /// it is a change.
    #[must_use]
    pub fn files(&self) -> Vec<(LayerKind, PathBuf)> {
        let project = self.workspace.as_deref().map(ProjectFiles::at);
        [
            (LayerKind::Managed, self.dirs.managed_config.clone()),
            (LayerKind::User, self.dirs.user_config.clone()),
            (
                LayerKind::Project,
                project.as_ref().map(|files| files.config.clone()),
            ),
            (
                LayerKind::ProjectLocal,
                project.map(|files| files.local_config),
            ),
        ]
        .into_iter()
        .filter_map(|(layer, file)| Some((layer, file?)))
        .collect()
    }
}

/// Why configuration could not be loaded: every error found, with the
/// warnings found on the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadFailure {
    /// Everything that was noticed. At least one is an error.
    pub diagnostics: Vec<Diagnostic>,
}

impl LoadFailure {
    /// The errors alone.
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
    }

    /// Every diagnostic, a line each, as `cortex config check` prints them.
    #[must_use]
    pub fn render(&self, env: &Env) -> String {
        self.diagnostics
            .iter()
            .map(|diagnostic| diagnostic.render(env))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl fmt::Display for LoadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self.errors().count();
        match self.errors().next() {
            Some(first) if count == 1 => {
                write!(f, "the configuration has an error: {}", first.message)
            }
            Some(first) => write!(
                f,
                "the configuration has {count} errors, the first: {}",
                first.message
            ),
            None => f.write_str("the configuration could not be loaded"),
        }
    }
}

impl std::error::Error for LoadFailure {}

impl cortex_base::Classified for LoadFailure {
    fn kind(&self) -> cortex_base::ErrorKind {
        cortex_base::ErrorKind::Config
    }
}

/// A permission rule and where it was written. Managed rules cannot be
/// weakened by user rules, nor user rules by project rules, and the layer
/// is what tells them apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedRule {
    /// The rule.
    pub rule: Rule,
    /// Where it was written.
    pub origin: Origin,
}

/// A change between two loaded configurations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The key whose effective value changed.
    pub key: KeyPath,
    /// When the change takes effect.
    pub effect: Effect,
}

/// A loaded configuration: the settings, where each came from, and what was
/// noticed while loading.
///
/// **Its untyped views never show a literal that may be a secret.**
/// [`value`](Self::value), [`effective`](Self::effective),
/// [`show`](Self::show), [`show_layer`](Self::show_layer) and `Debug` show a
/// provider's literal header as `[redacted]`. The typed `settings` hold it
/// as a `Secret`, which is the way to it.
#[derive(Clone, PartialEq)]
pub struct Loaded {
    /// The settings in effect, defaults included.
    pub settings: Settings,
    /// Warnings and notes. There are no errors: with an error there is no
    /// `Loaded`.
    pub diagnostics: Vec<Diagnostic>,
    /// Project settings held back until the user trusts the project.
    pub withheld: Vec<Withheld>,
    /// The profile that was applied.
    pub profile: Option<String>,
    /// The directories the environment gives.
    pub dirs: Dirs,
    /// The files that were looked for, lowest layer first, and whether each
    /// was there.
    pub files: Vec<(LayerKind, PathBuf, bool)>,
    values: toml::Table,
    origins: BTreeMap<KeyPath, Origin>,
    entry_origins: BTreeMap<KeyPath, Vec<Origin>>,
    layers: BTreeMap<LayerKind, LayerView>,
    locks: Vec<KeyPath>,
}

impl fmt::Debug for Loaded {
    /// The settings and what was noticed, and the merged values with every
    /// literal that may be a secret redacted. The layers as read are left
    /// out, so that a field added later is not printed by accident.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Loaded")
            .field("settings", &self.settings)
            .field("diagnostics", &self.diagnostics)
            .field("withheld", &self.withheld)
            .field("profile", &self.profile)
            .field("dirs", &self.dirs)
            .field("files", &self.files)
            .field(
                "values",
                &schema::redact(&KeyPath::root(), &Value::Table(self.values.clone())),
            )
            .field("locks", &self.locks)
            .finish_non_exhaustive()
    }
}

/// One file layer, read and parsed.
#[derive(Clone)]
pub(crate) struct FileLayer {
    pub(crate) kind: LayerKind,
    file: PathBuf,
    document: Document,
}

impl fmt::Debug for FileLayer {
    /// The layer and its file. Not what the file says: a provider's header
    /// in it may hold a key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileLayer")
            .field("kind", &self.kind)
            .field("file", &self.file)
            .finish_non_exhaustive()
    }
}

/// What to do when the selected profile is not defined.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissingProfile {
    /// It is an error, as it is for every ordinary load.
    Error,
    /// It is a warning, and no profile is applied. For the one merge that
    /// must not fail: dropping a project whose trust was lowered, when the
    /// selected profile was defined by that project alone.
    Warning,
}

/// Loads configuration.
///
/// # Errors
///
/// A [`LoadFailure`] when any layer has an error: a file that cannot be
/// read or parsed, a value of the wrong type or out of range, an unknown
/// key in an enforced table, a newer `config_version`, a profile that does
/// not exist, or a command-line setting that is not one.
pub fn load(options: &LoadOptions) -> Result<Loaded, LoadFailure> {
    load_layers(options).map(|(loaded, _)| loaded)
}

/// [`load`], with the file layers the result was made from, so that a live
/// configuration can merge them again without reading the files.
pub(crate) fn load_layers(options: &LoadOptions) -> Result<(Loaded, Vec<FileLayer>), LoadFailure> {
    let mut diagnostics = Vec::new();
    let mut files = Vec::new();
    let mut layers = Vec::new();

    for (kind, file) in options.files() {
        let document = read_layer(kind, &file, options.strict, &mut diagnostics);
        files.push((kind, file.clone(), document.is_some()));
        if let Some(Some(document)) = document {
            layers.push(FileLayer {
                kind,
                file,
                document,
            });
        }
    }
    let loaded = merge_layers(
        options,
        &layers,
        files,
        diagnostics,
        MissingProfile::Error,
    )?;
    Ok((loaded, layers))
}

/// Merges file layers that were read already, then the profile, the
/// environment and the command line. `diagnostics` holds what reading the
/// files found: with an error among them the merge still runs, so that one
/// load reports everything, and then fails.
pub(crate) fn merge_layers(
    options: &LoadOptions,
    layers: &[FileLayer],
    files: Vec<(LayerKind, PathBuf, bool)>,
    mut diagnostics: Vec<Diagnostic>,
    missing_profile: MissingProfile,
) -> Result<Loaded, LoadFailure> {
    let failed = |diagnostics: &[Diagnostic]| {
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
    };

    let locks = read_locks(layers, &mut diagnostics);
    let mut merger = Merger::new(options.trust == ProjectTrust::Trusted, locks);
    for layer in layers {
        merger.apply(
            &Source {
                layer: layer.kind,
                authority: layer.kind,
                file: Some((&layer.file, &layer.document)),
                prefix: KeyPath::root(),
                detail: None,
            },
            &layer.document.table,
        );
    }

    let profile = apply_profile(
        options,
        layers,
        &mut merger,
        &mut diagnostics,
        missing_profile,
    );
    apply_environment(options, &mut merger, &mut diagnostics);
    for given in &options.overrides {
        apply_override(given, &mut merger, &mut diagnostics);
    }

    diagnostics.append(&mut merger.diagnostics);
    if failed(&diagnostics) {
        return Err(LoadFailure { diagnostics });
    }
    let settings = match Settings::read(&Value::Table(merger.values.clone())) {
        Ok(settings) => settings,
        Err(message) => {
            // Every layer was checked against the same schema, so this is
            // a fault in the checking, not in the user's file. It is still
            // reported, and nothing half-read is returned.
            diagnostics.push(Diagnostic {
                severity: Severity::Error,
                layer: None,
                file: None,
                line: None,
                key: None,
                message: format!("the merged configuration could not be read: {message}"),
            });
            return Err(LoadFailure { diagnostics });
        }
    };
    Ok(Loaded {
        settings,
        diagnostics,
        withheld: merger.withheld,
        profile,
        dirs: options.dirs.clone(),
        files,
        values: merger.values,
        origins: merger.origins,
        entry_origins: merger.entry_origins,
        layers: merger.layers,
        locks: merger.locks,
    })
}

/// Reads, parses and checks one file. `None`: the file is not there.
/// `Some(None)`: it is there and has an error, which was reported.
fn read_layer(
    kind: LayerKind,
    file: &Path,
    strict: bool,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Option<Document>> {
    let mut report = |line: Option<u32>, message: String| {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            layer: Some(kind),
            file: Some(file.to_path_buf()),
            line,
            key: None,
            message,
        });
    };
    let mut bytes = Vec::new();
    match std::fs::File::open(file) {
        Ok(opened) => {
            // One more byte than the limit, to tell "at the limit" from
            // "over it" without reading a huge file.
            if let Err(error) = opened.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes) {
                report(None, format!("could not be read: {error}"));
                return Some(None);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        // A file that is there and cannot be read is an error, not a layer
        // to skip: skipping a managed file would drop its locks.
        Err(error) => {
            report(None, format!("could not be read: {error}"));
            return Some(None);
        }
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        report(
            None,
            format!(
                "is larger than {} KiB, which no configuration file is",
                MAX_FILE_BYTES / 1024
            ),
        );
        return Some(None);
    }
    let Ok(text) = String::from_utf8(bytes) else {
        report(None, "is not UTF-8 text".to_owned());
        return Some(None);
    };
    let document = match document::parse(&text) {
        Ok(document) => document,
        Err(failure) => {
            report(
                failure.line,
                format!("is not valid TOML: {}", failure.message),
            );
            return Some(None);
        }
    };
    let usable = validate::validate(
        &Checked {
            layer: kind,
            file: Some(file),
            document: &document,
            strict,
        },
        diagnostics,
    );
    Some(usable.then_some(document))
}

/// The keys the managed file locks.
fn read_locks(layers: &[FileLayer], diagnostics: &mut Vec<Diagnostic>) -> Vec<KeyPath> {
    let Some(managed) = layers.iter().find(|layer| layer.kind == LayerKind::Managed) else {
        return Vec::new();
    };
    let list = KeyPath::new(["managed", "locked"]);
    let Some(Value::Array(entries)) = merge::get(&managed.document.table, &list) else {
        return Vec::new();
    };
    let mut locks = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let mut warn = |message: String| {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                layer: Some(LayerKind::Managed),
                file: Some(managed.file.clone()),
                line: managed.document.entry_line(&list, index),
                key: Some(list.clone()),
                message,
            });
        };
        let Some(text) = entry.as_str() else {
            continue;
        };
        match KeyPath::parse(text) {
            Ok(key) => {
                if matches!(schema::lookup(&key), Lookup::Unknown { .. }) {
                    warn(format!(
                        "`{text}` names no key this version knows; the lock is kept"
                    ));
                }
                // An unknown key is still locked: a newer version may know
                // it, and a lock that is dropped is a restriction lost.
                locks.push(key);
            }
            Err(error) => warn(format!("{error}; this lock is ignored")),
        }
    }
    locks
}

/// Applies the selected profile from every file layer that defines it.
///
/// `--profile` chooses the profile over the `profile` key, unless the
/// managed file locks that key: a lock holds against the command line too,
/// so the managed file's choice stays, or no profile when it made none.
fn apply_profile(
    options: &LoadOptions,
    layers: &[FileLayer],
    merger: &mut Merger,
    diagnostics: &mut Vec<Diagnostic>,
    missing: MissingProfile,
) -> Option<String> {
    let profile_key = KeyPath::new(["profile"]);
    let flag = match &options.profile {
        Some(name) if merger.is_locked(&profile_key) => {
            diagnostics.push(Diagnostic {
                severity: Severity::Note,
                layer: Some(LayerKind::CommandLine),
                file: None,
                line: None,
                key: Some(profile_key.clone()),
                message: format!(
                    "locked by the managed configuration; `--profile {name}` is not used"
                ),
            });
            None
        }
        other => other.clone(),
    };
    let (name, from_flag) = match flag {
        Some(name) => (name, true),
        None => (
            merge::get(&merger.values, &profile_key)?
                .as_str()?
                .to_owned(),
            false,
        ),
    };
    if from_flag {
        merge::insert(
            &mut merger.values,
            &profile_key,
            Value::String(name.clone()),
        );
        merger.origins.insert(
            profile_key.clone(),
            Origin {
                layer: LayerKind::CommandLine,
                detail: Some("--profile".to_owned()),
                ..Origin::default_value()
            },
        );
    }
    let place = KeyPath::new(["profiles", name.as_str()]);
    let mut defined = false;
    for layer in layers {
        let Some(Value::Table(table)) = merge::get(&layer.document.table, &place) else {
            continue;
        };
        defined = true;
        merger.apply(
            &Source {
                layer: LayerKind::Profile,
                authority: layer.kind,
                file: Some((&layer.file, &layer.document)),
                prefix: place.clone(),
                detail: Some(name.clone()),
            },
            table,
        );
    }
    if !defined && missing == MissingProfile::Warning {
        diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            layer: None,
            file: None,
            line: None,
            key: Some(profile_key),
            message: format!(
                "the profile `{name}` was defined by the project alone, whose configuration is \
                 no longer applied; no profile is applied"
            ),
        });
        return None;
    }
    if !defined {
        let mut known: Vec<&str> = layers
            .iter()
            .filter_map(|layer| layer.document.table.get("profiles")?.as_table())
            .flat_map(|profiles| profiles.keys().map(String::as_str))
            .collect();
        known.sort_unstable();
        known.dedup();
        let origin = merger.origins.get(&profile_key);
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            layer: origin.map(|origin| origin.layer),
            file: origin.and_then(|origin| origin.file.clone()),
            line: origin.and_then(|origin| origin.line),
            key: Some(profile_key),
            message: if known.is_empty() {
                format!("there is no profile named `{name}`: no profile is defined")
            } else {
                format!(
                    "there is no profile named `{name}`. The profiles are: {}",
                    known.join(", ")
                )
            },
        });
    }
    Some(name)
}

/// The environment layer: a short, fixed list of variables.
fn apply_environment(
    options: &LoadOptions,
    merger: &mut Merger,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut set = |variable: &str, key: &[&str], value: String| {
        let mut table = toml::Table::new();
        merge::insert(
            &mut table,
            &KeyPath::new(key.iter().copied()),
            Value::String(value),
        );
        merger.apply(
            &Source {
                layer: LayerKind::Environment,
                authority: LayerKind::Environment,
                file: None,
                prefix: KeyPath::root(),
                detail: Some(variable.to_owned()),
            },
            &table,
        );
    };
    if let Some(level) = options.env.get("CORTEX_LOG") {
        set("CORTEX_LOG", &["log", "level"], level.to_owned());
    }
    match (
        options.env.get("CORTEX_DATA_DIR"),
        options.env.absolute_path("CORTEX_DATA_DIR"),
    ) {
        (Some(_), Some(path)) => set(
            "CORTEX_DATA_DIR",
            &["session", "data_dir"],
            path.to_string_lossy().into_owned(),
        ),
        (Some(value), None) => diagnostics.push(Diagnostic {
            severity: Severity::Warning,
            layer: Some(LayerKind::Environment),
            file: None,
            line: None,
            key: None,
            message: format!(
                "CORTEX_DATA_DIR is `{value}`, which is not an absolute path; it is ignored"
            ),
        }),
        (None, _) => {}
    }
}

/// Applies one command-line setting. A mistake here is always an error: a
/// flag the user typed and that did nothing would be worse than a refusal.
fn apply_override(given: &Override, merger: &mut Merger, diagnostics: &mut Vec<Diagnostic>) {
    let detail = given
        .flag
        .clone()
        .unwrap_or_else(|| format!("-c {}", given.key));
    let mut fail = |key: Option<KeyPath>, message: String| {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            layer: Some(LayerKind::CommandLine),
            file: None,
            line: None,
            key,
            message,
        });
    };
    let key = match KeyPath::parse(&given.key) {
        Ok(key) => key,
        Err(error) => return fail(None, error.to_string()),
    };
    if matches!(
        key.first(),
        Some("config_version" | "profile" | "profiles" | "managed")
    ) {
        return fail(
            Some(key),
            "this key describes a file and cannot be set on the command line. To choose a \
             profile, use `--profile`"
                .to_owned(),
        );
    }
    match schema::lookup(&key) {
        Lookup::Setting { .. } | Lookup::Reserved { .. } => {}
        Lookup::InsideSetting { child } => {
            return fail(
                Some(key),
                format!("`{}` is one setting and is set whole", child.name),
            );
        }
        Lookup::Table { .. } => {
            return fail(
                Some(key),
                "this is a table of settings; set one of its keys".to_owned(),
            );
        }
        Lookup::Unknown { siblings, .. } => {
            let suggestion = key
                .last()
                .and_then(|name| validate::nearest(name, &siblings))
                .map(|known| format!(" Did you mean `{known}`?"))
                .unwrap_or_default();
            return fail(Some(key), format!("unknown key.{suggestion}"));
        }
    }

    // The value as TOML when it reads as TOML and the key takes that, and
    // as plain text otherwise.
    let as_toml = format!("value = {}", given.value)
        .parse::<toml::Table>()
        .ok()
        .and_then(|mut table| table.remove("value"));
    let candidates = as_toml
        .into_iter()
        .chain([Value::String(given.value.clone())]);
    let mut first_errors: Option<Vec<Diagnostic>> = None;
    for candidate in candidates {
        let mut document = Document::default();
        merge::insert(&mut document.table, &key, candidate);
        let mut found = Vec::new();
        let usable = validate::validate(
            &Checked {
                layer: LayerKind::CommandLine,
                file: None,
                document: &document,
                strict: true,
            },
            &mut found,
        );
        if usable {
            merger.apply(
                &Source {
                    layer: LayerKind::CommandLine,
                    authority: LayerKind::CommandLine,
                    file: None,
                    prefix: KeyPath::root(),
                    detail: Some(detail),
                },
                &document.table,
            );
            return;
        }
        first_errors.get_or_insert(found);
    }
    diagnostics.extend(first_errors.unwrap_or_default());
}

impl Loaded {
    /// Where the effective value of a key came from. A key that no layer
    /// set comes from the defaults.
    #[must_use]
    pub fn origin(&self, key: &KeyPath) -> Origin {
        self.origins
            .get(key)
            .cloned()
            .unwrap_or_else(Origin::default_value)
    }

    /// Whether the managed configuration locks a key, so that no layer
    /// above it can change the key's value.
    #[must_use]
    pub fn is_locked(&self, key: &KeyPath) -> bool {
        self.locks.iter().any(|lock| key.is_at_or_under(lock))
    }

    /// The effective value of a key, the default included. A literal that
    /// may be a secret is `[redacted]`: the typed settings hold it.
    #[must_use]
    pub fn value(&self, key: &KeyPath) -> Option<Value> {
        match merge::get(&self.values, key) {
            Some(value) => Some(schema::redact(key, value)),
            None => merge::get(&schema::defaults(), key).map(|value| schema::redact(key, value)),
        }
    }

    /// The content of a section as the layers left it, for a reserved
    /// section whose keys are not declared yet.
    #[must_use]
    pub fn section(&self, name: &str) -> Option<&toml::Table> {
        self.values.get(name)?.as_table()
    }

    /// The permission rules in effect, each with the layer that wrote it.
    #[must_use]
    pub fn rules(&self) -> Vec<PlacedRule> {
        let origins = self
            .entry_origins
            .get(&KeyPath::new(["permissions", "rules"]));
        self.settings
            .permissions
            .rules
            .iter()
            .enumerate()
            .map(|(index, rule)| PlacedRule {
                rule: rule.clone(),
                origin: origins
                    .and_then(|origins| origins.get(index))
                    .cloned()
                    .unwrap_or_else(Origin::default_value),
            })
            .collect()
    }

    /// The data directory: `session.data_dir` when it is set, with a
    /// leading `~` expanded, and otherwise the platform's usual place.
    #[must_use]
    pub fn data_dir(&self) -> Option<PathBuf> {
        match self.settings.session.data_dir.as_deref() {
            Some(text) => expand_home(text, self.dirs.home.as_deref()),
            None => self.dirs.data.clone(),
        }
    }

    /// Every effective setting as a flat list in key order: the defaults,
    /// with what the layers set laid over them. A literal that may be a
    /// secret is `[redacted]`.
    #[must_use]
    pub fn effective(&self) -> Vec<(KeyPath, Value)> {
        self.effective_unredacted()
            .into_iter()
            .map(|(key, value)| {
                let shown = schema::redact(&key, &value);
                (key, shown)
            })
            .collect()
    }

    /// [`effective`](Self::effective) as it is, for comparing two
    /// configurations: a changed header is a change.
    fn effective_unredacted(&self) -> Vec<(KeyPath, Value)> {
        let all = merge::overlay(&schema::defaults(), &self.values);
        merge::flatten(&all)
            .into_iter()
            .map(|(key, value)| (key, value.clone()))
            .collect()
    }

    /// The effective configuration as `cortex config show` prints it: a
    /// setting a line, in key order. With `origins`, each line ends with
    /// where the value came from, and says so when the key is locked.
    #[must_use]
    pub fn show(&self, env: &Env, origins: bool) -> String {
        let lines = self
            .effective()
            .into_iter()
            .map(|(key, value)| {
                let entries = self.entry_origins.get(&key).map(Vec::as_slice);
                (key.clone(), value, self.origin(&key), entries)
            })
            .collect::<Vec<_>>();
        render(
            lines.iter().map(|(key, value, origin, entries)| Line {
                key,
                value,
                origin,
                entries: *entries,
                locked: self.is_locked(key),
            }),
            env,
            origins,
        )
    }

    /// One layer as it was read, before locks and the trust rules: what
    /// `cortex config show --layer <name>` prints. Empty when the layer set
    /// nothing.
    #[must_use]
    pub fn show_layer(&self, layer: LayerKind, env: &Env) -> String {
        if layer == LayerKind::Default {
            let defaults = schema::defaults();
            let origin = Origin::default_value();
            return render(
                merge::flatten(&defaults).iter().map(|(key, value)| Line {
                    key,
                    value,
                    origin: &origin,
                    entries: None,
                    locked: false,
                }),
                env,
                false,
            );
        }
        let Some(view) = self.layers.get(&layer) else {
            return String::new();
        };
        // The entries of a joined list are recorded one by one; a list is
        // shown once, with all of them.
        let mut lines: Vec<(KeyPath, Value, Origin, Vec<Origin>)> = Vec::new();
        for (key, value, origin) in &view.values {
            let value = &schema::redact(key, value);
            let joined = matches!(
                schema::lookup(key),
                Lookup::Setting { child, .. }
                    if matches!(child.meta, Meta::Leaf { joined: true, .. })
            );
            match lines.iter_mut().find(|(known, ..)| known == key) {
                Some((_, Value::Array(entries), _, origins)) if joined => {
                    entries.push(value.clone());
                    origins.push(origin.clone());
                }
                Some(line) => {
                    line.1 = value.clone();
                    line.2 = origin.clone();
                }
                None if joined => lines.push((
                    key.clone(),
                    Value::Array(vec![value.clone()]),
                    origin.clone(),
                    vec![origin.clone()],
                )),
                None => lines.push((key.clone(), value.clone(), origin.clone(), Vec::new())),
            }
        }
        lines.sort_by(|a, b| a.0.cmp(&b.0));
        render(
            lines.iter().map(|(key, value, origin, entries)| Line {
                key,
                value,
                origin,
                entries: (!entries.is_empty()).then_some(entries.as_slice()),
                locked: false,
            }),
            env,
            true,
        )
    }

    /// What differs between an earlier configuration and this one, with
    /// when each change takes effect.
    #[must_use]
    pub fn changes_since(&self, earlier: &Self) -> Vec<Change> {
        let before: BTreeMap<KeyPath, Value> =
            earlier.effective_unredacted().into_iter().collect();
        let after: BTreeMap<KeyPath, Value> = self.effective_unredacted().into_iter().collect();
        let mut changed: Vec<KeyPath> = before
            .iter()
            .filter(|(key, value)| after.get(*key) != Some(*value))
            .map(|(key, _)| key.clone())
            .chain(
                after
                    .keys()
                    .filter(|key| !before.contains_key(*key))
                    .cloned(),
            )
            .collect();
        changed.sort();
        changed
            .into_iter()
            .map(|key| {
                let effect = match schema::lookup(&key) {
                    Lookup::Setting { child, .. } => match child.meta {
                        Meta::Leaf { effect, .. } => effect,
                        _ => Effect::NextEpoch,
                    },
                    // A section that is not declared yet: the cautious
                    // answer, until its step says otherwise.
                    _ => Effect::NextEpoch,
                };
                Change { key, effect }
            })
            .collect()
    }
}

/// Expands a leading `~` to the home directory. Either separator may follow
/// it. `None` when there is a `~` and no home directory to put for it.
#[must_use]
pub fn expand_home(text: &str, home: Option<&Path>) -> Option<PathBuf> {
    match text.strip_prefix('~') {
        Some("") => home.map(Path::to_path_buf),
        Some(rest) if rest.starts_with('/') || rest.starts_with('\\') => {
            let rest = rest.trim_start_matches(['/', '\\']);
            home.map(|home| home.join(rest))
        }
        // `~name` is another user's home in a shell. It is not expanded.
        _ => Some(PathBuf::from(text)),
    }
}

/// One setting to print.
struct Line<'a> {
    key: &'a KeyPath,
    value: &'a Value,
    origin: &'a Origin,
    /// For a joined list, where each entry came from.
    entries: Option<&'a [Origin]>,
    /// Whether the managed configuration locks the key.
    locked: bool,
}

/// Renders settings as lines of `key = value`, with the origin after a `#`
/// when asked for. The `#` signs line up, up to a point: one long value
/// does not push every origin off the screen.
fn render<'a>(lines: impl Iterator<Item = Line<'a>>, env: &Env, origins: bool) -> String {
    const WIDEST: usize = 48;
    let mut rows: Vec<(String, Option<String>)> = Vec::new();
    for Line {
        key,
        value,
        origin,
        entries,
        locked,
    } in lines
    {
        let locked = if locked { ", locked" } else { "" };
        match (value, entries) {
            // A joined list, an entry a line, each with its own origin.
            (Value::Array(items), Some(entry_origins)) if !items.is_empty() => {
                rows.push((format!("{key} = ["), None));
                for (index, item) in items.iter().enumerate() {
                    rows.push((
                        format!("    {item},"),
                        entry_origins
                            .get(index)
                            .map(|origin| format!("{}{locked}", origin.render(env))),
                    ));
                }
                rows.push(("]".to_owned(), None));
            }
            _ => rows.push((
                format!("{key} = {value}"),
                Some(format!("{}{locked}", origin.render(env))),
            )),
        }
    }
    let width = rows
        .iter()
        .filter(|(_, origin)| origin.is_some())
        .map(|(text, _)| text.chars().count())
        .filter(|width| *width <= WIDEST)
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for (line, origin) in rows {
        text.push_str(&line);
        if let (true, Some(origin)) = (origins, origin) {
            let pad = width.saturating_sub(line.chars().count());
            text.push_str(&" ".repeat(pad + 2));
            text.push_str("# ");
            text.push_str(&origin);
        }
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_line_setting_is_key_equals_value() {
        let given = Override::parse("model.main = big-model").unwrap();
        assert_eq!(
            (given.key.as_str(), given.value.as_str()),
            ("model.main", "big-model")
        );
        assert_eq!(given.flag, None);
        // Only the first `=` splits: the value may have one.
        let given = Override::parse("providers.x.base_url=http://h/?a=b").unwrap();
        assert_eq!(given.value, "http://h/?a=b");
        assert_eq!(Override::parse("model.main=").unwrap().value, "");
        for wrong in ["model.main", "=value", "  =  "] {
            assert!(
                Override::parse(wrong).unwrap_err().contains("key=value"),
                "{wrong}"
            );
        }
    }

    #[test]
    fn a_tilde_is_the_home_directory() {
        let home = Path::new("/users/someone");
        let expand = |text| expand_home(text, Some(home));
        assert_eq!(expand("~"), Some(home.to_path_buf()));
        assert_eq!(expand("~/data"), Some(home.join("data")));
        assert_eq!(expand("~\\data\\deep"), Some(home.join("data\\deep")));
        assert_eq!(expand("/abs/path"), Some(PathBuf::from("/abs/path")));
        assert_eq!(expand("relative"), Some(PathBuf::from("relative")));
        assert_eq!(expand("~other/x"), Some(PathBuf::from("~other/x")));
        assert_eq!(expand_home("~/data", None), None);
        assert_eq!(expand_home("/abs", None), Some(PathBuf::from("/abs")));
    }

    #[test]
    fn a_failure_says_how_many_errors_there_are() {
        let error = |message: &str| Diagnostic {
            severity: Severity::Error,
            layer: None,
            file: None,
            line: None,
            key: None,
            message: message.to_owned(),
        };
        let one = LoadFailure {
            diagnostics: vec![error("first")],
        };
        assert_eq!(one.to_string(), "the configuration has an error: first");
        let two = LoadFailure {
            diagnostics: vec![error("first"), error("second")],
        };
        assert_eq!(
            two.to_string(),
            "the configuration has 2 errors, the first: first"
        );
        assert_eq!(two.errors().count(), 2);
        use cortex_base::Classified;
        assert_eq!(two.kind(), cortex_base::ErrorKind::Config);
    }
}
