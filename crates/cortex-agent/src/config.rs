//! What a session is made of, and how its loop is set.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use cortex_base::{PathResolver, SharedClock};
use cortex_llm::{ModelClient, ModelEntry, Reasoning};
use cortex_protocol::lease::Level;
use cortex_protocol::model::ModelRole;
use cortex_protocol::session::SandboxStatus;
use cortex_protocol::usage::Billing;
use cortex_protocol::{LeaseId, SessionId, Timestamp};
use cortex_tools::{ContentStore, FileTable, ImageSupport, MemoryStore, Outliner, OutputLimits};

use crate::guard::RepetitionLimits;

/// The model that serves one role, and how it is asked.
#[derive(Clone)]
pub struct RoleModel {
    /// The client its requests go to.
    pub client: Arc<dyn ModelClient>,
    /// The model's catalog entry.
    pub entry: Arc<ModelEntry>,
    /// How much it reasons and what of that it shows, already mapped onto
    /// the model's own control.
    pub reasoning: Reasoning,
    /// The reasoning level as the user set it, for the session's state.
    pub effort: Option<String>,
    /// The most output tokens one response may have. `None` leaves it to
    /// the model's own maximum.
    pub max_output: Option<u64>,
    /// How the role's usage is paid for.
    pub billing: Billing,
}

impl RoleModel {
    /// A role served by `client` with the model `entry`: the model's
    /// default reasoning, its own output limit, and usage billed per token.
    #[must_use]
    pub fn new(client: Arc<dyn ModelClient>, entry: Arc<ModelEntry>) -> Self {
        Self {
            client,
            entry,
            reasoning: Reasoning::default(),
            effort: None,
            max_output: None,
            billing: Billing::Metered,
        }
    }
}

impl std::fmt::Debug for RoleModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleModel")
            .field("model", &self.entry.info.id)
            .field("reasoning", &self.reasoning)
            .field("max_output", &self.max_output)
            .finish_non_exhaustive()
    }
}

/// The models of a session, by role.
#[derive(Debug, Clone)]
pub struct Models {
    main: RoleModel,
    others: BTreeMap<ModelRole, RoleModel>,
    catalog_version: String,
}

impl Models {
    /// The models of a session whose turns are served by `main`.
    /// `catalog_version` is the version of the catalog the prices come
    /// from, recorded with every cost.
    #[must_use]
    pub fn new(main: RoleModel, catalog_version: impl Into<String>) -> Self {
        Self {
            main,
            others: BTreeMap::new(),
            catalog_version: catalog_version.into(),
        }
    }

    /// Sets the model of a utility role. Setting `main` replaces the main
    /// model.
    #[must_use]
    pub fn with_role(mut self, role: ModelRole, model: RoleModel) -> Self {
        if role == ModelRole::Main {
            self.main = model;
        } else {
            self.others.insert(role, model);
        }
        self
    }

    /// The model the agent's own turns run on.
    #[must_use]
    pub fn main(&self) -> &RoleModel {
        &self.main
    }

    /// Every role that has a model, `main` first.
    pub fn roles(&self) -> impl Iterator<Item = (ModelRole, &RoleModel)> {
        std::iter::once((ModelRole::Main, &self.main)).chain(
            self.others
                .iter()
                .map(|(role, model)| (role.clone(), model)),
        )
    }

    /// The version of the catalog the prices come from.
    #[must_use]
    pub fn catalog_version(&self) -> &str {
        &self.catalog_version
    }
}

/// Where a session's tools work: its paths, its content store and its
/// table of seen files.
#[derive(Clone)]
pub struct Workspace {
    /// The session's path resolver.
    pub paths: Arc<PathResolver>,
    /// Where artifacts and file versions are kept.
    pub content: Arc<dyn ContentStore>,
    /// The session's table of seen files.
    pub files: Arc<FileTable>,
    /// The limits on what one tool result may hold.
    pub limits: OutputLimits,
    /// Where outlines come from, once the repository index exists.
    pub outliner: Option<Arc<dyn Outliner>>,
    /// The images the main model accepts in a tool result.
    pub images: Option<ImageSupport>,
}

impl Workspace {
    /// A workspace at `paths` with nothing on disk but the files
    /// themselves: artifacts and file versions are kept in memory.
    #[must_use]
    pub fn in_memory(paths: Arc<PathResolver>) -> Self {
        Self {
            paths,
            content: Arc::new(MemoryStore::new()),
            files: Arc::new(FileTable::new()),
            limits: OutputLimits::default(),
            outliner: None,
            images: None,
        }
    }
}

impl std::fmt::Debug for Workspace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Workspace")
            .field("root", self.paths.root())
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// The parts of a session's state that the loop is told and does not
/// change: what `session.updated` carries beside the loop's own figures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// The session's id.
    pub id: SessionId,
    /// The workspace root, as front ends are shown it.
    pub workspace: String,
    /// The session's title.
    pub title: Option<String>,
    /// When the session was created.
    pub created_at: Timestamp,
    /// The session it was forked from.
    pub parent: Option<SessionId>,
    /// The level of the lease in force.
    pub level: Level,
    /// The lease in force.
    pub lease_id: Option<LeaseId>,
    /// Whether the sandbox confines the session's commands.
    pub sandbox: SandboxStatus,
}

impl SessionInfo {
    /// The state of a new session at the default level, with no lease
    /// recorded and no sandbox.
    #[must_use]
    pub fn new(id: SessionId, workspace: impl Into<String>, clock: &SharedClock) -> Self {
        Self {
            id,
            workspace: workspace.into(),
            title: None,
            created_at: clock.now().to_timestamp(),
            parent: None,
            level: Level::Supervised,
            lease_id: None,
            sandbox: SandboxStatus::Off,
        }
    }
}

/// How the loop is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopConfig {
    /// The most tool calls that run at once.
    pub max_parallel_tools: usize,
    /// When the repetition guard speaks and stops. `None` turns it off.
    pub repetition: Option<RepetitionLimits>,
    /// The most rounds of verification one turn gets.
    pub verify_rounds: u32,
    /// The shortest time between two `usage.updated` events while a
    /// response arrives: one frame.
    pub usage_interval: Duration,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_parallel_tools: 8,
            repetition: Some(RepetitionLimits::default()),
            verify_rounds: 2,
            usage_interval: Duration::from_millis(16),
        }
    }
}
