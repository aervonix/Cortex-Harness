//! What is sent to the model, and how it is kept within the model's context
//! window (`docs/architecture/07-sessions-and-context.md`).
//!
//! One rule shapes everything here: **history that has been sent to a model
//! is never edited.** Prompt caches match on prefixes, and Anthropic's
//! current models tie each reasoning block to the system prompt, the tools
//! and every message before it. So a session is a sequence of **context
//! epochs**. Within one, the system prompt and the tools are frozen and the
//! history only grows; a notice, a changed instruction file or the date is
//! a new message at the end. Only a new epoch may change the prompt or the
//! tools, or drop old content, and it begins:
//!
//! - when a session first sends a request ([`EpochReason::Start`]);
//! - when a session read again would be built with another prompt or other
//!   tools, because the harness changed them ([`EpochReason::Resume`]);
//! - when the main role's model, provider or reasoning level changes: the
//!   prompt and the tools stay, so earlier reasoning stays valid
//!   ([`EpochReason::Model`]);
//! - at a compaction: a summary begins the history, the prompt is built
//!   again ([`EpochReason::Compaction`]).
//!
//! **An assistant message is replayed with its reasoning only in an epoch
//! whose prefix is the one it was produced under.** In any other it is sent
//! without its reasoning or the provider's own blocks ([`project`]), as a
//! message from another provider would be.
//!
//! The loop owns its [`Context`] and drives it: the reading of instruction
//! files, the summary requests and the records in the log are the loop's
//! (`crate::epochs`), because they need its store, its clock and its
//! models. What a session's context is made from is [`ContextParts`].

mod budget;
mod compact;
mod history;
mod instructions;
mod prefix;
mod prompt;

use std::collections::VecDeque;
use std::sync::Arc;

use cortex_base::{AbsPath, Classified, ErrorKind};
use cortex_llm::{CacheHints, ToolDefinition};
use cortex_protocol::item::{ArtifactRef, NoticeSource};
use cortex_protocol::{Item, Message, Part};
use cortex_tools::{PlanBoard, ToolSpec};
use serde_json::Value;

pub use budget::{DEFAULT_RATIO, OUTPUT_RESERVE_CAP, limit_of};
pub use compact::SUMMARY_INSTRUCTIONS;
pub use history::{NOTICE_LEAD, append, project};
pub use instructions::{
    DEFAULT_CAP, DEFAULT_NAMES, FileText, InstructionFiles, MemoryInstructionFiles,
    NoInstructionFiles,
};
pub use prefix::{PrefixDifference, check_prefix};
pub use prompt::{CORE_INSTRUCTIONS, Environment};

pub(crate) use budget::{Budget, Pressure, pressure};
pub(crate) use compact::{ARTIFACT_ROUNDS, Carried, instructions_message};
pub(crate) use history::{History, Source};
pub(crate) use instructions::{
    Change, Discovered, Found, Given, Place, discover, nested_dirs, notice_text, places, read_one,
    shown as shown_in,
};
pub(crate) use prefix::{Shape, difference, excerpt, hash_of};
pub(crate) use prompt::{Built, Sections, build};

use crate::store::{EpochReason, EpochStart, Opened};

/// The content of one request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Assembled {
    /// The system prompt.
    pub system: String,
    /// The tools the model may call, in the order they are declared.
    pub tools: Vec<ToolDefinition>,
    /// The conversation, oldest first.
    pub messages: Vec<Message>,
    /// Where the cacheable parts end.
    pub cache: CacheHints,
}

/// The context could not be made to fit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ContextError {
    /// Why, for a person.
    pub message: String,
}

impl ContextError {
    /// An error with this message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Classified for ContextError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::ContextOverflow
    }
}

/// How a session keeps its context: the thresholds, the instruction files
/// and the summaries. Each is a key of `[context]`
/// (`docs/architecture/08-configuration.md`).
#[derive(Debug, Clone, PartialEq)]
pub struct ContextSettings {
    /// Whether the loop compacts by itself when the trigger is reached.
    /// Off, it stops and asks.
    pub auto_compact: bool,
    /// Whether a summary is prepared in the background, from
    /// `prepare_at`.
    pub background: bool,
    /// The trigger: compact before a request estimated past this fraction
    /// of the limit.
    pub compact_at: f64,
    /// Prepare a summary in the background past this fraction.
    pub prepare_at: f64,
    /// How much of the most recent conversation a compaction replays after
    /// its summary, in tokens. 0 replays nothing.
    pub keep_recent_tokens: u64,
    /// The names of instruction files, in the order a directory's are read.
    pub instruction_names: Vec<String>,
    /// The most the instruction files may hold in all, in bytes.
    pub instructions_cap: usize,
    /// The most a summary may be, in tokens.
    pub summary_max_output: u64,
}

impl Default for ContextSettings {
    fn default() -> Self {
        Self {
            auto_compact: true,
            background: true,
            compact_at: 0.85,
            prepare_at: 0.70,
            keep_recent_tokens: 0,
            instruction_names: DEFAULT_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            instructions_cap: DEFAULT_CAP,
            summary_max_output: 16_384,
        }
    }
}

/// What a session's context is made from.
pub struct ContextParts {
    /// The facts of section 2 of the system prompt.
    pub environment: Environment,
    /// The user's text, appended last to the system prompt
    /// (`--system-append`).
    pub append: Option<String>,
    /// Where instruction files are read.
    pub instructions: Arc<dyn InstructionFiles>,
    /// The directory of the user's own instruction file: the configuration
    /// directory.
    pub user_dir: Option<AbsPath>,
    /// The thresholds and the names.
    pub settings: ContextSettings,
    /// The session's plan, which a compaction carries across.
    pub plan: Option<Arc<PlanBoard>>,
    /// A key that stays the same for the session, for providers that route
    /// requests with one key to one cache.
    pub cache_key: Option<String>,
}

impl ContextParts {
    /// A context with no instruction files, no environment facts and the
    /// default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            environment: Environment::default(),
            append: None,
            instructions: Arc::new(NoInstructionFiles),
            user_dir: None,
            settings: ContextSettings::default(),
            plan: None,
            cache_key: None,
        }
    }
}

impl Default for ContextParts {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ContextParts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContextParts")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// The definition of a tool as a request declares it.
#[must_use]
pub fn definition(spec: &ToolSpec) -> ToolDefinition {
    ToolDefinition {
        name: spec.name.clone(),
        description: spec.description.clone(),
        schema: spec.parameters.clone(),
    }
}

/// The size of tool definitions as the budget counts them.
fn tools_bytes(tools: &[ToolDefinition]) -> u64 {
    tools
        .iter()
        .map(|tool| {
            (tool.name.len() + tool.description.len() + tool.schema.to_string().len()) as u64
        })
        .sum()
}

/// Whether two epochs send the same prefix: the same system prompt and
/// the same tools, in the same order.
pub(crate) fn same_prefix(one: &EpochStart, other: &EpochStart) -> bool {
    one.system == other.system && one.tools == other.tools
}

/// A session's context, as the loop holds it.
pub(crate) struct Context {
    pub(crate) parts: ContextParts,
    /// The definitions of the session's tools now, for the next epoch.
    pub(crate) tools: Vec<ToolDefinition>,
    /// The epoch in force, and its number on the branch.
    pub(crate) epoch: Option<EpochStart>,
    pub(crate) epoch_number: u64,
    /// Set when the context is read from the store: before the next turn
    /// the epoch is held against the prompt the harness would build now.
    pub(crate) unchecked: bool,
    pub(crate) history: History,
    /// What the model has of each instruction file.
    pub(crate) given: Given,
    /// Goes up each time the context is read from the store, so that a
    /// summary prepared over a history that is no longer the branch's is
    /// known as such.
    pub(crate) generation: u64,
    pub(crate) budget: Budget,
    /// The user's last message on the branch.
    pub(crate) last_user: Option<Vec<Part>>,
    /// The artifacts of the last rounds, newest last.
    pub(crate) artifacts: VecDeque<Vec<ArtifactRef>>,
    /// The state the last summary on the branch carried.
    pub(crate) carried_before: Option<Value>,
    /// The request before this one, for where the next first differs.
    pub(crate) last_shape: Option<Shape>,
    /// The size of the request in flight, for the ratio of bytes to tokens.
    pub(crate) sent_bytes: u64,
    /// The stable prefix of the last request, with what it is compared
    /// under: a debug build's check.
    pub(crate) last_prefix: Option<(u64, Vec<u8>)>,
}

impl Context {
    pub(crate) fn new(parts: ContextParts, tools: &[&ToolSpec]) -> Self {
        Self {
            parts,
            tools: tools.iter().map(|spec| definition(spec)).collect(),
            epoch: None,
            epoch_number: 0,
            unchecked: false,
            history: History::default(),
            given: Given::default(),
            generation: 0,
            budget: Budget::default(),
            last_user: None,
            artifacts: VecDeque::new(),
            carried_before: None,
            last_shape: None,
            sent_bytes: 0,
            last_prefix: None,
        }
    }

    /// Replaces what the context holds with the session as the store has
    /// it: the epoch in force on the branch, and the history from its last
    /// summary on.
    pub(crate) fn load(&mut self, opened: &Opened) {
        self.generation += 1;
        self.unchecked = true;
        self.budget = Budget::default();
        self.last_shape = None;
        self.last_prefix = None;
        self.epoch = opened.epochs.last().map(|(_, epoch)| epoch.clone());
        self.epoch_number = opened.epochs.len() as u64;
        self.carried_before.clone_from(&opened.carried);
        self.history = History::default();

        let items = &opened.items;
        let summary = items
            .iter()
            .rposition(|item| matches!(item, Item::Summary(_)));
        // The epoch each item was recorded under.
        let epoch_at = |index: usize| {
            opened
                .epochs
                .iter()
                .rev()
                .find(|(at, _)| *at <= index)
                .map(|(_, epoch)| epoch)
        };
        let current = self.epoch.clone();
        let produced_here = |index: usize| match (epoch_at(index), &current) {
            (Some(then), Some(now)) => same_prefix(then, now),
            _ => false,
        };
        let from = match summary {
            Some(at) => {
                self.history.push(&items[at]);
                let covered = match &items[at] {
                    Item::Summary(summary) => summary.covers_through.as_ref().and_then(|id| {
                        items[..at]
                            .iter()
                            .position(|item| item.id() == Some(id.as_str()))
                    }),
                    _ => None,
                };
                // The turns the summary did not cover are replayed after
                // it, without their reasoning.
                if let Some(covered) = covered {
                    for item in &items[covered + 1..at] {
                        self.history.push_projected(item);
                    }
                }
                at + 1
            }
            None => 0,
        };
        for (index, item) in items.iter().enumerate().skip(from) {
            if produced_here(index) {
                self.history.push(item);
            } else {
                self.history.push_projected(item);
            }
        }

        // What the model has of the instruction files: what the prompt
        // gave when it was first sent (a change of model keeps it), then
        // the notices since.
        let first = match &current {
            Some(now) => opened
                .epochs
                .iter()
                .rposition(|(_, epoch)| !same_prefix(epoch, now))
                .map_or(0, |before| before + 1),
            None => opened.epochs.len(),
        };
        let (epoch_from, marks) = opened
            .epochs
            .get(first)
            .map_or((0, &[][..]), |(at, epoch)| (*at, &epoch.instructions[..]));
        self.given = Given::from_marks(marks);
        for item in items.iter().skip(epoch_from) {
            if let Item::Notice(notice) = item
                && notice.source == NoticeSource::Instructions
            {
                self.given.note(&notice.text);
            }
        }
        // Its user's last message, and the artifacts of the last rounds.
        self.last_user = None;
        self.artifacts.clear();
        for item in items {
            self.note(item);
        }
    }

    /// Keeps what a compaction carries from an item: the user's last
    /// message, and the artifacts of the last rounds.
    fn note(&mut self, item: &Item) {
        match item {
            Item::UserMessage(user) => self.last_user = Some(user.parts.clone()),
            Item::AssistantMessage(_) => {
                self.artifacts.push_back(Vec::new());
                while self.artifacts.len() > ARTIFACT_ROUNDS {
                    self.artifacts.pop_front();
                }
            }
            Item::ToolResult(result) if !result.artifacts.is_empty() => {
                if self.artifacts.is_empty() {
                    self.artifacts.push_back(Vec::new());
                }
                if let Some(round) = self.artifacts.back_mut() {
                    round.extend(result.artifacts.iter().cloned());
                }
            }
            _ => {}
        }
    }

    /// An item was recorded. Items arrive in the order the log has them.
    pub(crate) fn push(&mut self, item: &Item) {
        self.history.push(item);
        self.note(item);
    }

    /// The size of the next request's content, in bytes.
    pub(crate) fn content_bytes(&self) -> u64 {
        let (system, tools) = self.epoch.as_ref().map_or((0, 0), |epoch| {
            (epoch.system.len() as u64, tools_bytes(&epoch.tools))
        });
        system + tools + self.history.bytes()
    }

    /// The estimate by source, in tokens.
    pub(crate) fn sources(&self) -> crate::stats::ContextSources {
        let tokens = |bytes| self.budget.tokens(bytes);
        let (system, tools) = self.epoch.as_ref().map_or((0, 0), |epoch| {
            (epoch.system.len() as u64, tools_bytes(&epoch.tools))
        });
        let mut sources = crate::stats::ContextSources {
            system: tokens(system),
            tools: tokens(tools),
            ..crate::stats::ContextSources::default()
        };
        for (source, bytes) in self.history.bytes_by_source(0) {
            let tokens = tokens(bytes);
            match source {
                Source::Summary => sources.summary = tokens,
                Source::User => sources.user = tokens,
                Source::Assistant => sources.assistant = tokens,
                Source::ToolResults => sources.tool_results = tokens,
                Source::Notice => sources.notices = tokens,
            }
        }
        sources
    }

    /// The content of the next request.
    pub(crate) fn assemble(&self) -> Assembled {
        let (system, tools) = self
            .epoch
            .as_ref()
            .map(|epoch| (epoch.system.clone(), epoch.tools.clone()))
            .unwrap_or_default();
        Assembled {
            system,
            tools,
            messages: self.history.messages().to_vec(),
            cache: CacheHints {
                stable_prefix: true,
                history: true,
                lifetime: None,
                key: self.parts.cache_key.clone(),
            },
        }
    }

    /// Begins an epoch. With `history`, it is the new epoch's (a
    /// compaction's); without, the history goes on, and when the new
    /// prefix is not the old one every reasoning block in it is dropped.
    pub(crate) fn begin(&mut self, epoch: EpochStart, history: Option<History>) {
        let keeps_prefix = self
            .epoch
            .as_ref()
            .is_some_and(|before| same_prefix(before, &epoch));
        match history {
            Some(history) => self.history = history,
            None if !keeps_prefix => self.history.project_all(),
            None => {}
        }
        if epoch.reason != Some(EpochReason::Model) {
            self.given = Given::from_marks(&epoch.instructions);
        }
        if !keeps_prefix {
            self.budget.new_epoch();
        }
        self.epoch = Some(epoch);
        self.epoch_number += 1;
        self.unchecked = false;
    }
}

#[cfg(test)]
mod tests;
