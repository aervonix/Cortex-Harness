//! The schema: every key a configuration file may hold, what it takes, who
//! may set it and when a change takes effect.
//!
//! **A key is declared once.** The [`settings!`] macro turns one declaration
//! into the field of a settings struct, its default, its place in the schema
//! tree ([`Node`]), its reader and its writer. Validation, the trust rules,
//! the reload notices, the JSON Schema and the reference page are all walks
//! over that one tree, so none of them can describe a different
//! configuration from the one the code reads.
//!
//! ```ignore
//! settings! {
//!     /// How requests are retried.
//!     pub struct RetrySettings {
//!         /// The most attempts for one request, the first included.
//!         [Preference, Now, 1..=20]
//!         pub attempts: u32 = 5,
//!     }
//! }
//! ```
//!
//! The bracket before a field says what kind of entry it is:
//!
//! | Bracket | Meaning |
//! |---|---|
//! | `[<trust>, <effect>]` | A setting: its [`Trust`] rule and its [`Effect`]. |
//! | `[<trust>, <effect>, low..=high]` | The same, with the range a number must be in. |
//! | `[<trust>, <effect>, joined]` | The same, for a list that every layer adds to. |
//! | `[nested]` | A table of settings, or a named collection of such tables. |
//! | `[enforced]` | A table of settings the harness enforces: an unknown key in it is an error. |
//! | `[reserved <trust>]` | A section whose keys the step that builds it will declare. Anything in it is accepted, unread. |

mod json;
mod reference;
mod sections;
mod types;

use std::collections::BTreeMap;
use std::sync::OnceLock;

use toml::Value;

pub use json::json_schema;
pub use reference::reference_page;
pub use sections::{
    CompatSettings, CredentialSettings, LimitSet, LimitSettings, LogSettings, ManagedSettings,
    ModelSettings, PermissionSettings, ProviderSettings, RetrySettings, RoleSettings,
    SandboxSettings, SessionSettings, Settings,
};
pub use types::{EnvText, Headers, Profiles, ReasoningVisibility, Reserved, StoreChoice};

use crate::key::KeyPath;

/// The version of the configuration schema this build reads and writes:
/// the `config_version` key.
pub const CONFIG_VERSION: u32 = 1;

/// Who may set a key from a project's files, as declared for the key.
///
/// A repository must not be able to take over the harness by containing a
/// configuration file (`docs/architecture/08-configuration.md`, "Trust and
/// what a project may set"). Most keys have one class. A few depend on the
/// value: lowering a limit is harmless and raising it is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// A harmless preference. A project's value applies always.
    Preference,
    /// It can run code. A project's value applies only after the user has
    /// trusted the project.
    RunsCode,
    /// It can widen access. A project's value applies only after trust.
    WidensAccess,
    /// The user's alone. A project's value is ignored, with a warning.
    UserOnly,
    /// A permission rule: `deny` and `ask` are preferences, `allow` widens
    /// access.
    ByRuleAction,
    /// A permission mode: at or below the mode already in effect it is a
    /// preference; above that, up to `supervised`, it widens access; above
    /// `supervised` it is the user's alone.
    ByLevel,
    /// The sandbox's network setting: `off` is a preference, `full` widens
    /// access.
    ByNetwork,
    /// A limit: a value at or below the one already in effect is a
    /// preference, and so is the first limit where there was none. A higher
    /// one is the user's alone.
    TightenOnly,
}

/// What a project's value for a key is, once the value is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TrustClass {
    /// Applied always.
    Preference,
    /// Applied after trust: it can run code.
    RunsCode,
    /// Applied after trust: it can widen access.
    WidensAccess,
    /// Never applied from a project.
    UserOnly,
}

impl TrustClass {
    /// The class as the reference and the messages name it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::RunsCode => "runs code",
            Self::WidensAccess => "widens access",
            Self::UserOnly => "user only",
        }
    }
}

impl Trust {
    /// The class of `value` for a key with this rule. `current` is the value
    /// in effect from the layers below (defaults included), for the rules
    /// that compare.
    #[must_use]
    pub fn class(self, value: &Value, current: Option<&Value>) -> TrustClass {
        match self {
            Self::Preference => TrustClass::Preference,
            Self::RunsCode => TrustClass::RunsCode,
            Self::WidensAccess => TrustClass::WidensAccess,
            Self::UserOnly => TrustClass::UserOnly,
            Self::ByRuleAction => {
                // Anything but a plain `deny` or `ask` is treated as an
                // allow: a rule that cannot be read is not a restriction.
                match value.get("action").and_then(Value::as_str) {
                    Some("deny" | "ask") => TrustClass::Preference,
                    _ => TrustClass::WidensAccess,
                }
            }
            Self::ByLevel => {
                let rank = |value: &Value| {
                    value.as_str().and_then(|name| {
                        cortex_protocol::Level::KNOWN
                            .iter()
                            .position(|known| *known == name)
                    })
                };
                let supervised = cortex_protocol::Level::KNOWN
                    .iter()
                    .position(|known| *known == cortex_protocol::Level::Supervised.as_str());
                match (rank(value), current.and_then(rank)) {
                    (Some(wanted), _) if Some(wanted) > supervised => TrustClass::UserOnly,
                    (Some(wanted), Some(now)) if wanted <= now => TrustClass::Preference,
                    (Some(_), _) => TrustClass::WidensAccess,
                    (None, _) => TrustClass::UserOnly,
                }
            }
            Self::ByNetwork => match value.as_str() {
                Some("off") => TrustClass::Preference,
                _ => TrustClass::WidensAccess,
            },
            Self::TightenOnly => {
                match (types::magnitude(value), current.and_then(types::magnitude)) {
                    (Some(_), None) => TrustClass::Preference,
                    (Some(wanted), Some(now)) if wanted <= now => TrustClass::Preference,
                    _ => TrustClass::UserOnly,
                }
            }
        }
    }

    /// The rule as the reference page states it.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Preference => "preference",
            Self::RunsCode => "runs code",
            Self::WidensAccess => "widens access",
            Self::UserOnly => "user only",
            Self::ByRuleAction => {
                "by action: `deny` and `ask` are preferences, `allow` widens access"
            }
            Self::ByLevel => {
                "by level: not above the mode in effect is a preference, up to `supervised` \
                 widens access, above it user only"
            }
            Self::ByNetwork => "by value: `off` is a preference, `full` widens access",
            Self::TightenOnly => {
                "tighten only: a lower limit is a preference, a higher one user only"
            }
        }
    }
}

/// When a change to a key takes effect in a running harness
/// (`docs/architecture/08-configuration.md`, "Reloading without a restart").
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Effect {
    /// At once: on the next frame, tool call or request.
    Now,
    /// At the next context epoch, because it shapes the system prompt, the
    /// tool list or the model. A notice says what is pending.
    NextEpoch,
    /// When the harness is next started.
    Restart,
}

impl Effect {
    /// The effect as the reference and the notices name it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Now => "at once",
            Self::NextEpoch => "next epoch",
            Self::Restart => "restart",
        }
    }
}

/// What a setting's value is, for the generated schema and the reference.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// `true` or `false`.
    Bool,
    /// A whole number.
    Integer,
    /// A number, whole or not.
    Number,
    /// Text.
    Text,
    /// One of a fixed set of words.
    Choice(&'static [&'static str]),
    /// A length of time, written as text: `"20s"`.
    Duration,
    /// Text, or `{ env = "NAME" }` to read an environment variable when the
    /// value is used.
    EnvText,
    /// A list.
    List(Box<Kind>),
    /// A table from names to values.
    Map(Box<Kind>),
    /// A permission rule: `effect`, `match` and `action`, and nothing else.
    Rule,
}

/// What kind of entry a child of a table is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Meta {
    /// A setting.
    Leaf {
        /// Who may set it from a project.
        trust: Trust,
        /// When a change takes effect.
        effect: Effect,
        /// The range a number must be in, ends included.
        range: Option<(f64, f64)>,
        /// Whether the value is a list that each layer adds to, instead of
        /// replacing it.
        joined: bool,
    },
    /// A table of settings, or a named collection.
    Nested {
        /// Whether the harness enforces what is in it: an unknown key is
        /// then an error, not a warning.
        enforced: bool,
    },
    /// A section whose keys are not declared yet.
    Reserved {
        /// What everything in it counts as, from a project.
        trust: Trust,
    },
}

/// One entry of a table in the schema.
#[derive(Debug, Clone)]
pub struct Child {
    /// The key's name.
    pub name: &'static str,
    /// Its documentation, a line per element, as written on the field.
    pub doc: &'static [&'static str],
    /// What kind of entry it is.
    pub meta: Meta,
    /// The default, when there is one.
    pub default: Option<Value>,
    /// What it holds.
    pub node: Node,
    /// Reads a value of the field's type, to say whether it is one.
    pub(crate) check: fn(&Value) -> Result<(), String>,
}

impl Child {
    /// The documentation as one paragraph.
    #[must_use]
    pub fn doc_text(&self) -> String {
        self.doc
            .iter()
            .map(|line| line.trim())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_owned()
    }
}

/// A place in the schema.
#[derive(Debug, Clone)]
pub enum Node {
    /// A setting of this kind.
    Leaf(Kind),
    /// A table with these keys.
    Table(Vec<Child>),
    /// A named collection: any name, each holding the same thing.
    Map(Box<Node>),
    /// A reserved section: anything is accepted.
    Open,
    /// The profiles: any name, each holding the keys of the file itself.
    Profiles,
}

impl Node {
    /// The entry called `name`, when this is a table that has it.
    #[must_use]
    pub fn child(&self, name: &str) -> Option<&Child> {
        match self {
            Self::Table(children) => children.iter().find(|child| child.name == name),
            _ => None,
        }
    }
}

/// A type that can be a setting: it has a place in the schema, and is read
/// from and written to a TOML value.
pub trait Setting: Sized {
    /// What the schema says about a value of this type.
    fn node() -> Node;

    /// Reads a value.
    ///
    /// # Errors
    ///
    /// With a phrase that says what was expected, without the key or the
    /// file: the caller knows those.
    fn read(value: &Value) -> Result<Self, String>;

    /// The value as TOML, or `None` for one that is not set.
    fn write(&self) -> Option<Value>;
}

/// The schema of a whole configuration file.
#[must_use]
pub fn root() -> &'static Node {
    static ROOT: OnceLock<Node> = OnceLock::new();
    ROOT.get_or_init(Settings::node)
}

/// What the schema says about a key path.
#[derive(Debug, Clone)]
pub enum Lookup {
    /// It names a setting.
    Setting {
        /// The setting's entry.
        child: &'static Child,
        /// Whether it is inside a table the harness enforces.
        enforced: bool,
    },
    /// It names something inside a setting whose value is itself a table
    /// (a header in a provider's `headers`). Such a setting is set whole.
    InsideSetting {
        /// The setting it is inside.
        child: &'static Child,
    },
    /// It names a table of settings, a named collection or one of its
    /// entries, or a profile.
    Table {
        /// Whether it is inside a table the harness enforces.
        enforced: bool,
    },
    /// It is a reserved section, or inside one.
    Reserved {
        /// What everything in the section counts as, from a project.
        trust: Trust,
    },
    /// No such key is declared.
    Unknown {
        /// The names that are declared where the path went wrong, for a
        /// suggestion.
        siblings: Vec<&'static str>,
        /// Whether that place is inside a table the harness enforces.
        enforced: bool,
    },
}

/// Looks a key path up in the schema. A path into a profile is looked up as
/// the key it sets.
#[must_use]
pub fn lookup(path: &KeyPath) -> Lookup {
    let mut node = root();
    let mut enforced = false;
    for (index, part) in path.parts().iter().enumerate() {
        match node {
            Node::Table(_) => {
                let Some(child) = node.child(part) else {
                    return Lookup::Unknown {
                        siblings: names(node),
                        enforced,
                    };
                };
                match child.meta {
                    Meta::Leaf { .. } => {
                        return if index + 1 == path.len() {
                            Lookup::Setting { child, enforced }
                        } else {
                            Lookup::InsideSetting { child }
                        };
                    }
                    Meta::Reserved { trust } => return Lookup::Reserved { trust },
                    Meta::Nested { enforced: here } => {
                        enforced = enforced || here;
                        node = &child.node;
                    }
                }
            }
            // Any name: the schema goes on below it.
            Node::Map(inner) => node = inner,
            // Any name: the keys of a profile are the keys of the file.
            Node::Profiles => {
                node = root();
                enforced = false;
            }
            Node::Leaf(_) | Node::Open => break,
        }
    }
    Lookup::Table { enforced }
}

/// One declared setting, with its full name. A name inside a named
/// collection has `*` where the name goes: `providers.*.base_url`.
#[derive(Debug, Clone)]
pub struct KeyInfo {
    /// The key, with `*` for a name the user chooses.
    pub path: String,
    /// What it holds. `None` for a reserved section.
    pub kind: Option<Kind>,
    /// Who may set it from a project.
    pub trust: Trust,
    /// When a change takes effect. `None` for a reserved section.
    pub effect: Option<Effect>,
    /// Whether the harness enforces it.
    pub enforced: bool,
    /// Whether each layer adds to the list instead of replacing it.
    pub joined: bool,
    /// The range a number must be in.
    pub range: Option<(f64, f64)>,
    /// The default.
    pub default: Option<Value>,
    /// The documentation.
    pub doc: String,
}

/// Every declared setting and reserved section, in the order of the schema.
/// The keys of a profile are the same keys again, and are not repeated.
#[must_use]
pub fn keys() -> Vec<KeyInfo> {
    let mut keys = Vec::new();
    collect(root(), "", false, &mut keys);
    keys
}

fn collect(node: &Node, prefix: &str, enforced: bool, keys: &mut Vec<KeyInfo>) {
    let Node::Table(children) = node else {
        return;
    };
    for child in children {
        let path = if prefix.is_empty() {
            child.name.to_owned()
        } else {
            format!("{prefix}.{}", child.name)
        };
        match child.meta {
            Meta::Leaf {
                trust,
                effect,
                range,
                joined,
            } => keys.push(KeyInfo {
                kind: match &child.node {
                    Node::Leaf(kind) => Some(kind.clone()),
                    _ => None,
                },
                path,
                trust,
                effect: Some(effect),
                enforced,
                joined,
                range,
                default: child.default.clone(),
                doc: child.doc_text(),
            }),
            Meta::Reserved { trust } => keys.push(KeyInfo {
                path: format!("{path}.*"),
                kind: None,
                trust,
                effect: None,
                enforced,
                joined: false,
                range: None,
                default: None,
                doc: child.doc_text(),
            }),
            Meta::Nested { enforced: here } => {
                let mut node = &child.node;
                let mut path = path;
                while let Node::Map(inner) = node {
                    node = inner;
                    path.push_str(".*");
                }
                collect(node, &path, enforced || here, keys);
            }
        }
    }
}

/// The names of a table's entries, for a "did you mean" suggestion.
pub(crate) fn names(node: &Node) -> Vec<&'static str> {
    match node {
        Node::Table(children) => children.iter().map(|child| child.name).collect(),
        _ => Vec::new(),
    }
}

/// The default of every setting that has one, as a table.
#[must_use]
pub fn defaults() -> toml::Table {
    match Settings::default().write() {
        Some(Value::Table(table)) => table,
        _ => toml::Table::new(),
    }
}

impl<T: Setting> Setting for Option<T> {
    fn node() -> Node {
        T::node()
    }

    fn read(value: &Value) -> Result<Self, String> {
        T::read(value).map(Some)
    }

    fn write(&self) -> Option<Value> {
        self.as_ref().and_then(Setting::write)
    }
}

impl<T: Setting> Setting for BTreeMap<String, T> {
    fn node() -> Node {
        Node::Map(Box::new(T::node()))
    }

    fn read(value: &Value) -> Result<Self, String> {
        let Value::Table(table) = value else {
            return Err("expected a table of named entries".to_owned());
        };
        table
            .iter()
            .map(|(name, entry)| {
                T::read(entry)
                    .map(|entry| (name.clone(), entry))
                    .map_err(|error| format!("{name}: {error}"))
            })
            .collect()
    }

    fn write(&self) -> Option<Value> {
        if self.is_empty() {
            return None;
        }
        let table = self
            .iter()
            .filter_map(|(name, entry)| Some((name.clone(), entry.write()?)))
            .collect();
        Some(Value::Table(table))
    }
}

/// Declares a table of settings: the struct, its defaults, its schema, its
/// reader and its writer. The syntax is described on [the module](self).
macro_rules! settings {
    (
        $(#[doc = $struct_doc:literal])*
        pub struct $name:ident {
            $(
                $(#[doc = $doc:literal])*
                [$($meta:tt)*]
                pub $field:ident : $ty:ty $(= $default:expr)?
            ),* $(,)?
        }
    ) => {
        $(#[doc = $struct_doc])*
        #[derive(Debug, Clone, PartialEq)]
        pub struct $name {
            $(
                $(#[doc = $doc])*
                pub $field: $ty,
            )*
        }

        impl Default for $name {
            fn default() -> Self {
                Self {
                    $( $field: settings!(@default $($default)?), )*
                }
            }
        }

        impl $crate::schema::Setting for $name {
            fn node() -> $crate::schema::Node {
                let defaults = Self::default();
                $crate::schema::Node::Table(vec![
                    $(
                        $crate::schema::Child {
                            name: stringify!($field),
                            doc: &[$($doc),*],
                            meta: settings!(@meta $($meta)*),
                            default: $crate::schema::Setting::write(&defaults.$field),
                            node: <$ty as $crate::schema::Setting>::node(),
                            check: |value| {
                                <$ty as $crate::schema::Setting>::read(value).map(drop)
                            },
                        },
                    )*
                ])
            }

            fn read(value: &::toml::Value) -> Result<Self, String> {
                let ::toml::Value::Table(table) = value else {
                    return Err("expected a table".to_owned());
                };
                let defaults = Self::default();
                Ok(Self {
                    $(
                        $field: match table.get(stringify!($field)) {
                            Some(value) => <$ty as $crate::schema::Setting>::read(value)
                                .map_err(|error| {
                                    format!("{}: {error}", stringify!($field))
                                })?,
                            None => defaults.$field,
                        },
                    )*
                })
            }

            fn write(&self) -> Option<::toml::Value> {
                let mut table = ::toml::Table::new();
                $(
                    if let Some(value) = $crate::schema::Setting::write(&self.$field) {
                        table.insert(stringify!($field).to_owned(), value);
                    }
                )*
                Some(::toml::Value::Table(table))
            }
        }
    };
    (@default) => { Default::default() };
    (@default $default:expr) => { $default };
    (@meta nested) => { $crate::schema::Meta::Nested { enforced: false } };
    (@meta enforced) => { $crate::schema::Meta::Nested { enforced: true } };
    (@meta reserved $trust:ident) => {
        $crate::schema::Meta::Reserved { trust: $crate::schema::Trust::$trust }
    };
    (@meta $trust:ident, $effect:ident) => {
        $crate::schema::Meta::Leaf {
            trust: $crate::schema::Trust::$trust,
            effect: $crate::schema::Effect::$effect,
            range: None,
            joined: false,
        }
    };
    (@meta $trust:ident, $effect:ident, joined) => {
        $crate::schema::Meta::Leaf {
            trust: $crate::schema::Trust::$trust,
            effect: $crate::schema::Effect::$effect,
            range: None,
            joined: true,
        }
    };
    (@meta $trust:ident, $effect:ident, $low:literal ..= $high:literal) => {
        $crate::schema::Meta::Leaf {
            trust: $crate::schema::Trust::$trust,
            effect: $crate::schema::Effect::$effect,
            range: Some({
                // Whole numbers, named as such so that a large bound is not
                // read as an `i32`.
                let (low, high): (i64, i64) = ($low, $high);
                (low as f64, high as f64)
            }),
            joined: false,
        }
    };
}
pub(crate) use settings;

/// Declares a setting that is one of a fixed set of words.
macro_rules! choice {
    (
        $(#[doc = $doc:literal])*
        pub enum $name:ident {
            $(
                $(#[doc = $variant_doc:literal])*
                $variant:ident => $word:literal
            ),+ $(,)?
        }
    ) => {
        $(#[doc = $doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $(
                $(#[doc = $variant_doc])*
                $variant,
            )+
        }

        impl $name {
            /// Every value, as written in a configuration file.
            pub const WORDS: &'static [&'static str] = &[$($word),+];

            /// The value as written in a configuration file.
            #[must_use]
            pub fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $word, )+
                }
            }
        }

        impl $crate::schema::Setting for $name {
            fn node() -> $crate::schema::Node {
                $crate::schema::Node::Leaf($crate::schema::Kind::Choice(Self::WORDS))
            }

            fn read(value: &::toml::Value) -> Result<Self, String> {
                match value.as_str() {
                    $( Some($word) => Ok(Self::$variant), )+
                    _ => Err($crate::schema::types::one_of(Self::WORDS)),
                }
            }

            fn write(&self) -> Option<::toml::Value> {
                Some(::toml::Value::String(self.as_str().to_owned()))
            }
        }
    };
}
pub(crate) use choice;
