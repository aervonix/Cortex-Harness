//! The loop's part in context management
//! (`docs/architecture/07-sessions-and-context.md`): when an epoch begins,
//! the instruction files, the date, the budget, and compaction.
//!
//! The [`Context`](crate::context) holds what is sent. What needs the loop
//! is here: reading files (on the blocking pool), records in the log (the
//! loop's store, and always before the context is told), the models of the
//! roles, and the usage and the ledger.
//!
//! **Where each thing happens.**
//!
//! | When | What |
//! |---|---|
//! | A turn starts, before its prompt | The epoch: begun, or held against the prompt the harness would build now. Instruction files that changed, appeared or went. |
//! | Before each request (a safe boundary: no round is open) | A change of the main role's model starts an epoch. Instruction files under directories a tool first touched. The budget: a summary in the background from `prepare_at`, a compaction from `compact_at`. |
//! | Compacting | The summary prepared in the background if it is still good (waited for if it is not ready), or one asked for now. Then the `summary` entry, the `epoch` entry, and the new epoch. |
//! | After each request | The context record, and in a debug build the check that the request began with the one before it. |

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cortex_base::{AbsPath, CancelToken, Classified, ContentHash};
use cortex_llm::{ModelClient, ModelEvent, ModelRequest, Usage};
use cortex_protocol::event::{
    CompactionNotice, CompactionState, Event, InstructionsNotice, ItemCompleted, Notice,
};
use cortex_protocol::item::{NoticeSource, SummaryItem};
use cortex_protocol::model::ModelRole;
use cortex_protocol::usage::RequestUsage;
use cortex_protocol::{ErrorData, ErrorKind, Item, ItemId, Message, Part, Role, StopReason};
use cortex_tools::run_blocking;

use super::{End, Phase, Prep, Session, Turn};
use crate::config::RoleModel;
use crate::context::{
    Built, Carried, Change, Discovered, Found, History, Place, Pressure, Sections, Shape, build,
    check_prefix, difference, discover, excerpt, hash_of, instructions_message, limit_of,
    nested_dirs, notice_text, places, pressure, project, read_one,
};
use crate::state::TurnState;
use crate::stats::ContextStat;
use crate::store::{EpochReason, EpochStart, Record, StoreError, Summary};
use crate::usage::add_request;

/// A summary taken in the background, while it runs and once it is done.
pub(super) enum Background {
    Running(Job),
    Ready(Ready),
}

/// A summary request running beside the turn.
pub(super) struct Job {
    covered: Cover,
    cancel: CancelToken,
    handle: tokio::task::JoinHandle<Summarized>,
}

/// A summary that is done, its usage counted.
pub(super) struct Ready {
    covered: Cover,
    outcome: Result<String, ErrorData>,
}

/// What a summary was taken over: the first `messages` of the epoch's
/// history, which end with the item `through`. A summary is used only over
/// a history that still begins so.
#[derive(Debug, Clone)]
struct Cover {
    messages: usize,
    through: Option<ItemId>,
}

impl Cover {
    fn holds_for(&self, history: &History) -> bool {
        self.messages <= history.len() && history.end_of(self.messages) == self.through
    }
}

/// What a summary request came to.
pub(super) struct Summarized {
    model: RoleModel,
    role: ModelRole,
    outcome: Result<String, ErrorData>,
    usage: Option<Usage>,
}

/// Why a compaction could not finish.
enum Unfinished {
    Store(StoreError),
    /// The summary, with what it carries and what it replays, does not
    /// fit the new epoch.
    TooLarge(String),
}

/// The date of a time, as the model is told it.
fn date_of(time: cortex_base::UtcTime) -> String {
    time.to_string().chars().take(10).collect()
}

/// Sends a summary request and reads its text.
async fn summarize(
    client: Arc<dyn ModelClient>,
    request: ModelRequest,
    cancel: CancelToken,
) -> (Result<String, ErrorData>, Option<Usage>) {
    let mut stream = client.stream(request, cancel.child());
    let mut text = String::new();
    let mut usage = None;
    loop {
        let Some(event) = cancel.race(stream.next()).await else {
            let error = ErrorData::new(
                ErrorKind::Interrupted,
                "interrupted",
                "The summary was stopped.",
            );
            return (Err(error), usage);
        };
        let Some(event) = event else {
            let error = ErrorData::new(
                ErrorKind::Transient,
                "incomplete_stream",
                "The summary's response ended before it was complete.",
            );
            return (Err(error), usage);
        };
        match event {
            ModelEvent::TextDelta { text: piece, .. } => text.push_str(&piece),
            // A new attempt begins: what came before it does not count.
            ModelEvent::Retry(_) => text.clear(),
            ModelEvent::Start { .. } => text.clear(),
            ModelEvent::Usage(running) => usage = Some(running),
            ModelEvent::End { usage: last, .. } => {
                let outcome = if text.trim().is_empty() {
                    Err(ErrorData::new(
                        ErrorKind::ModelStop,
                        "empty_summary",
                        "The model answered the request for a summary with no text.",
                    ))
                } else {
                    Ok(text)
                };
                return (outcome, Some(last));
            }
            ModelEvent::Error(error) => return (Err(error.to_error_data()), usage),
            _ => {}
        }
    }
}

impl Session {
    /// The main role's model as the session is set now, for this request.
    pub(super) fn refresh_main(&mut self) {
        let inbox = self.shared.lock();
        self.model = inbox.models.main().clone();
        inbox
            .models
            .catalog_version()
            .clone_into(&mut self.catalog_version);
    }

    /// A new context epoch of the table of seen files: the model no longer
    /// has what it was shown, so the next read of any file is in full. The
    /// one place every new epoch reaches the table, a reload's included
    /// (`docs/architecture/07-sessions-and-context.md`, "What each part of
    /// a session's state follows").
    pub(super) fn new_file_epoch(&self) {
        self.env.workspace.files.begin_epoch();
    }

    fn root(&self) -> AbsPath {
        self.env.workspace.paths.root().clone()
    }

    /// Reads the instruction files and builds the system prompt.
    async fn build_prompt(&mut self) -> (Built, Discovered) {
        let (found, io, cap) = {
            let parts = &self.context.parts;
            (
                places(
                    &self.root(),
                    parts.user_dir.as_ref(),
                    &parts.settings.instruction_names,
                ),
                Arc::clone(&parts.instructions),
                parts.settings.instructions_cap,
            )
        };
        let discovered = run_blocking(&CancelToken::new(), move |_| discover(&*io, &found, cap))
            .await
            .unwrap_or_default();
        let parts = &self.context.parts;
        let built = build(&Sections {
            environment: Some(&parts.environment),
            instructions: &discovered.texts,
            append: parts.append.as_deref(),
        });
        (built, discovered)
    }

    /// Tells the user what the cap left out, or what could not be read.
    fn warn_instructions(&self, turn: &Turn, discovered: &Discovered) {
        for message in &discovered.warnings {
            self.notify(
                turn,
                Notice::Instructions(InstructionsNotice {
                    message: message.clone(),
                    files: discovered.warned.clone(),
                }),
            );
        }
    }

    /// The start of an epoch with this prompt, for the main model now.
    fn epoch_start(
        &self,
        built: &Built,
        discovered: &Discovered,
        reason: EpochReason,
    ) -> EpochStart {
        let info = &self.model.entry.info;
        EpochStart {
            provider: info.provider.clone(),
            protocol: info.protocol.clone(),
            model: info.id.clone(),
            effort: self.model.effort.clone(),
            system: built.system.clone(),
            tools: self.context.tools.clone(),
            reason: Some(reason),
            base: Some(built.base),
            instructions: discovered.marks.clone(),
            altered: false,
        }
    }

    /// Records an epoch, then begins it. A summary taken in the background
    /// is of the old epoch's history, which only a change of model keeps.
    async fn begin_epoch(
        &mut self,
        turn: &Turn,
        epoch: EpochStart,
        history: Option<History>,
    ) -> Result<(), StoreError> {
        self.store
            .append(&turn.id, Record::Epoch(epoch.clone()))
            .await?;
        if epoch.reason != Some(EpochReason::Model) {
            self.drop_background();
        }
        self.context.begin(epoch, history);
        self.new_file_epoch();
        Ok(())
    }

    /// At the start of a turn, before its prompt is recorded: the epoch,
    /// and changed instruction files.
    pub(super) async fn context_turn_start(&mut self, turn: &Turn) -> Result<(), StoreError> {
        self.refresh_main();
        let mut begun = false;
        if self.context.epoch.is_none() || self.context.unchecked {
            let (built, discovered) = self.build_prompt().await;
            self.warn_instructions(turn, &discovered);
            // An epoch whose prompt the log does not hold as it was sent
            // (the filter masked something in it) cannot go on: its next
            // request would not begin as the earlier ones did.
            let goes_on = self.context.epoch.as_ref().is_some_and(|epoch| {
                !epoch.altered
                    && epoch.tools == self.context.tools
                    && match epoch.base {
                        Some(base) => base == built.base,
                        None => epoch.system == built.system,
                    }
            });
            if goes_on {
                // The prompt is the one the harness would build, but for
                // the instruction files: what changed in them is told as a
                // notice, below. A session read again on a later day is
                // told the date.
                self.context.unchecked = false;
                let today = date_of(self.clock.now());
                if self
                    .context
                    .date
                    .as_deref()
                    .is_some_and(|date| date != today)
                {
                    self.record_notice(
                        turn,
                        NoticeSource::Harness,
                        crate::context::date_text(&today),
                    )
                    .await?;
                    self.context.date = Some(today);
                }
            } else {
                let reason = if self.context.epoch.is_none() && self.context.history.len() == 0 {
                    EpochReason::Start
                } else {
                    EpochReason::Resume
                };
                let epoch = self.epoch_start(&built, &discovered, reason);
                self.begin_epoch(turn, epoch, None).await?;
                begun = true;
            }
        }
        if !begun {
            self.instruction_changes(turn).await?;
        }
        Ok(())
    }

    /// Tells the model of each instruction file that changed, appeared or
    /// went since it was given it.
    async fn instruction_changes(&mut self, turn: &Turn) -> Result<(), StoreError> {
        let root = self.root();
        let parts = &self.context.parts;
        let mut checked = places(
            &root,
            parts.user_dir.as_ref(),
            &parts.settings.instruction_names,
        );
        // The nested files the model was given.
        for shown in self.context.given.files.keys() {
            if checked.iter().any(|place| &place.shown == shown) {
                continue;
            }
            if let Ok(path) = root.join(shown) {
                checked.push(Place {
                    dir: path.parent().unwrap_or_else(|| root.clone()),
                    path,
                    shown: shown.clone(),
                    user: false,
                });
            }
        }
        let io = Arc::clone(&parts.instructions);
        let cap = parts.settings.instructions_cap;
        let read = run_blocking(&CancelToken::new(), move |_| {
            checked
                .into_iter()
                .map(|place| {
                    let found = read_one(&*io, &place, cap);
                    (place, found)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();

        let mut changes = Vec::new();
        let mut in_dir: BTreeMap<String, BTreeSet<ContentHash>> = BTreeMap::new();
        for (place, outcome) in read {
            let given = self.context.given.files.get(&place.shown).copied();
            match outcome {
                // Said nothing about: the next turn looks again.
                Err(_) => {}
                Ok(Some(found)) => {
                    // A second name of a file of the same directory.
                    if !in_dir
                        .entry(place.dir.key().to_owned())
                        .or_default()
                        .insert(found.hash())
                    {
                        continue;
                    }
                    match given {
                        Some(Some(had)) if had == found.hash() => {}
                        Some(Some(_)) => changes.push(Change::Changed(found)),
                        _ => changes.push(Change::New(found)),
                    }
                }
                Ok(None) => {
                    if let Some(Some(_)) = given {
                        changes.push(Change::Removed(place.shown.clone()));
                    }
                }
            }
        }
        self.tell_changes(turn, changes).await
    }

    /// Appends a notice for each change, and tells front ends.
    async fn tell_changes(&mut self, turn: &Turn, changes: Vec<Change>) -> Result<(), StoreError> {
        let cap = self.context.parts.settings.instructions_cap;
        for change in changes {
            let text = notice_text(&change, cap);
            let (shown, hash, said) = match &change {
                Change::Changed(found) => (
                    found.place.shown.clone(),
                    Some(found.hash()),
                    "changed; the model was given the new version",
                ),
                Change::New(found) => (
                    found.place.shown.clone(),
                    Some(found.hash()),
                    "is new; the model was given it",
                ),
                Change::Removed(shown) => (shown.clone(), None, "was removed; the model was told"),
                Change::Nested { found, .. } => (
                    found.place.shown.clone(),
                    Some(found.hash()),
                    "applies to files a tool touched; the model was given it",
                ),
            };
            self.record_notice(turn, NoticeSource::Instructions, text)
                .await?;
            self.context.given.files.insert(shown.clone(), hash);
            self.notify(
                turn,
                Notice::Instructions(InstructionsNotice {
                    message: format!("The instruction file {shown} {said}."),
                    files: vec![shown],
                }),
            );
        }
        Ok(())
    }

    /// Before each request, at a safe boundary: a change of the main
    /// role's model starts an epoch, and instruction files under
    /// directories a tool has first touched are given.
    pub(super) async fn context_before_request(&mut self, turn: &Turn) -> Result<(), StoreError> {
        if let Some(epoch) = &self.context.epoch {
            let info = &self.model.entry.info;
            if epoch.provider != info.provider
                || epoch.protocol != info.protocol
                || epoch.model != info.id
                || epoch.effort != self.model.effort
            {
                let next = EpochStart {
                    provider: info.provider.clone(),
                    protocol: info.protocol.clone(),
                    model: info.id.clone(),
                    effort: self.model.effort.clone(),
                    reason: Some(EpochReason::Model),
                    ..epoch.clone()
                };
                self.begin_epoch(turn, next, None).await?;
            }
        }
        self.nested_instructions(turn).await
    }

    /// Gives the model, once, each instruction file of a directory between
    /// the workspace root and a file a tool has read or changed.
    async fn nested_instructions(&mut self, turn: &Turn) -> Result<(), StoreError> {
        let root = self.root();
        let mut dirs: Vec<AbsPath> = Vec::new();
        for file in self.env.workspace.files.carried() {
            for dir in nested_dirs(&root, &file.path) {
                let key = crate::context::shown_in(&root, &dir);
                if self.context.given.looked.insert(key) {
                    dirs.push(dir);
                }
            }
        }
        if dirs.is_empty() {
            return Ok(());
        }
        let names = self.context.parts.settings.instruction_names.clone();
        let mut candidates = Vec::new();
        for dir in &dirs {
            for name in &names {
                if let Ok(path) = dir.join(name) {
                    candidates.push(Place {
                        shown: crate::context::shown_in(&root, &path),
                        path,
                        user: false,
                        dir: dir.clone(),
                    });
                }
            }
        }
        let io = Arc::clone(&self.context.parts.instructions);
        let cap = self.context.parts.settings.instructions_cap;
        let read = run_blocking(&CancelToken::new(), move |_| {
            candidates
                .into_iter()
                .filter_map(|place| read_one(&*io, &place, cap).ok().flatten())
                .collect::<Vec<Found>>()
        })
        .await
        .unwrap_or_default();
        let mut changes = Vec::new();
        let mut in_dir: BTreeMap<String, BTreeSet<ContentHash>> = BTreeMap::new();
        for found in read {
            if self.context.given.files.contains_key(&found.place.shown) {
                continue;
            }
            if !in_dir
                .entry(found.place.dir.key().to_owned())
                .or_default()
                .insert(found.hash())
            {
                continue;
            }
            let dir = crate::context::shown_in(&root, &found.place.dir);
            changes.push(Change::Nested { found, dir });
        }
        self.tell_changes(turn, changes).await
    }

    /// The context window of a role's model: what its local server
    /// reported, or the catalog's.
    pub(super) fn window_of(&self, model: &RoleModel) -> u64 {
        let info = &model.entry.info;
        self.shared
            .lock()
            .windows
            .get(&(info.provider.clone(), info.id.clone()))
            .copied()
            .unwrap_or(info.context_window)
    }

    /// The window of a role's model, and the most a request of it may hold.
    pub(super) fn limit(&self, model: &RoleModel) -> (u64, u64) {
        let window = self.window_of(model);
        let max_output = model.max_output.unwrap_or(model.entry.info.max_output);
        (window, limit_of(window, max_output))
    }

    /// The estimate of the next request, in tokens.
    pub(super) fn estimate(&self) -> u64 {
        self.context.budget.estimate(self.context.content_bytes())
    }

    /// Where the next request stands against the thresholds.
    pub(super) fn pressure(&self) -> Pressure {
        let settings = &self.context.parts.settings;
        let (_, limit) = self.limit(&self.model);
        pressure(
            self.estimate(),
            limit,
            settings.prepare_at,
            settings.compact_at,
        )
    }

    /// The model a summary is asked of: the `fast` role's, when it has one
    /// whose window holds the epoch, and otherwise the main model.
    fn summary_model(&self) -> (ModelRole, RoleModel) {
        let estimate = self.estimate();
        let fast = self.shared.lock().models.role(&ModelRole::Fast).cloned();
        if let Some(fast) = fast {
            let (_, limit) = self.limit(&fast);
            if estimate <= limit {
                return (ModelRole::Fast, fast);
            }
        }
        (ModelRole::Main, self.model.clone())
    }

    /// A summary request over the first `covered` messages of the epoch:
    /// the epoch's own prefix, so that a summary on the same model reads
    /// the epoch's cache, then the instructions.
    ///
    /// After the provider said a request does not fit, the estimate is
    /// known to be low, so the request is always cut, to half the limit.
    fn summary_request(
        &self,
        model: &RoleModel,
        role: &ModelRole,
        covered: usize,
        focus: Option<&str>,
        overflowed: bool,
    ) -> ModelRequest {
        let assembled = self.context.assemble();
        let mut request = ModelRequest::new(Arc::clone(&model.entry), role.clone());
        request.system = assembled.system;
        request.tools = assembled.tools;
        request.cache = assembled.cache;
        request.reasoning = model.reasoning.clone();
        request.speed = self.shared.lock().speeds.of(role);
        let settings = &self.context.parts.settings;
        request.max_output = Some(settings.summary_max_output.min(model.entry.info.max_output));
        let messages = &self.context.history.messages()[..covered];
        let (_, limit) = self.limit(model);
        let budget = &self.context.budget;
        let whole = budget.tokens(self.context.content_bytes());
        // What the request holds besides the messages: the prompt, the
        // tools and the instructions. The messages get the rest.
        let fixed = budget.tokens(
            self.context
                .content_bytes()
                .saturating_sub(self.context.history.bytes())
                + crate::context::SUMMARY_INSTRUCTIONS.len() as u64,
        );
        request.messages = if overflowed {
            self.trimmed(messages, (limit / 2).saturating_sub(fixed))
        } else if whole <= limit {
            messages.to_vec()
        } else {
            self.trimmed(
                messages,
                (limit.saturating_mul(9) / 10).saturating_sub(fixed),
            )
        };
        request.messages.push(instructions_message(focus));
        request
    }

    /// The newest messages of `messages` that fit `room`, whole turns at a
    /// time and without their reasoning, after the epoch's summary if it
    /// begins with one: for a summary request the summarising model could
    /// not take whole (the provider said the epoch does not fit).
    fn trimmed(&self, messages: &[Message], room: u64) -> Vec<Message> {
        let budget = &self.context.budget;
        let starts_with_summary = self.context.history.starts_with_summary();
        let first = usize::from(starts_with_summary).min(messages.len());
        // A round begins at any message that is not a tool's results: the
        // cut is made only there, so that no call is left without them.
        let begins_round = |index: usize| messages[index].role != Role::Tool;
        let mut from = messages.len();
        for index in (first..messages.len()).rev() {
            let tokens = budget
                .tokens(self.context.history.bytes_from(index))
                .saturating_sub(budget.tokens(self.context.history.bytes_from(messages.len())));
            if tokens > room {
                break;
            }
            if begins_round(index) {
                from = index;
            }
        }
        // Nothing fitted: the last round is kept all the same, so that the
        // summary is of the work, and a request still too large fails
        // where it can be seen.
        if from == messages.len() {
            from = (first..messages.len())
                .rev()
                .find(|&index| begins_round(index))
                .unwrap_or(first);
        }
        let mut out = Vec::new();
        if starts_with_summary {
            out.push(messages[0].clone());
        }
        if from > first {
            out.push(Message {
                role: Role::User,
                parts: vec![Part::text(format!(
                    "{}The earlier part of this conversation is left out here: it did not fit.",
                    crate::context::NOTICE_LEAD
                ))],
                origin: None,
            });
        }
        out.extend(messages[from..].iter().map(project));
        out
    }

    /// Starts a summary in the background, when the settings allow one and
    /// none is under way or ready.
    pub(super) fn start_background(&mut self, turn: &Turn) {
        let settings = &self.context.parts.settings;
        if !settings.auto_compact
            || !settings.background
            || self.background.is_some()
            || self.background_failed == Some(self.context.epoch_number)
            || self.context.history.len() == 0
        {
            return;
        }
        // A summary is a request, and one prepared ahead of need is not
        // worth stopping the session for: with no request left, none.
        if self.ledger.take_request().is_err() {
            return;
        }
        let covered = self.context.history.len();
        let (role, model) = self.summary_model();
        let request = self.summary_request(&model, &role, covered, None, false);
        let cancel = self.shared.cancel.child();
        let token = cancel.clone();
        let client = Arc::clone(&model.client);
        let handle = tokio::spawn(async move {
            let (outcome, usage) = summarize(client, request, token).await;
            Summarized {
                model,
                role,
                outcome,
                usage,
            }
        });
        self.background = Some(Background::Running(Job {
            covered: Cover {
                messages: covered,
                through: self.context.history.end_of(covered),
            },
            cancel,
            handle,
        }));
        self.notify(
            turn,
            Notice::Compaction(CompactionNotice {
                message: "Preparing a summary of the conversation in the background.".to_owned(),
                state: CompactionState::Started,
                background: true,
                before_tokens: None,
                after_tokens: None,
            }),
        );
    }

    /// A summary that finished in the background has its usage counted
    /// now, and waits as ready.
    pub(super) async fn collect_background(&mut self, turn: &mut Turn) -> Result<(), StoreError> {
        // A summary that finished before it was given up was paid for.
        for handle in std::mem::take(&mut self.given_up) {
            if let Ok(summarized) = handle.await
                && let Some(usage) = summarized.usage
            {
                self.out_of_band(turn, &summarized.model, &summarized.role, usage)
                    .await?;
            }
        }
        let finished =
            matches!(&self.background, Some(Background::Running(job)) if job.handle.is_finished());
        if !finished {
            return Ok(());
        }
        let Some(Background::Running(job)) = self.background.take() else {
            return Ok(());
        };
        let covered = job.covered.clone();
        let summarized = job.handle.await;
        let outcome = self.counted(turn, summarized).await?;
        if outcome.is_err() {
            self.background_failed = Some(self.context.epoch_number);
        }
        self.background = Some(Background::Ready(Ready { covered, outcome }));
        Ok(())
    }

    /// Counts what a summary request used, and records it.
    async fn counted(
        &mut self,
        turn: &mut Turn,
        summarized: Result<Summarized, tokio::task::JoinError>,
    ) -> Result<Result<String, ErrorData>, StoreError> {
        let summarized = match summarized {
            Ok(summarized) => summarized,
            Err(_) => {
                return Ok(Err(ErrorData::new(
                    ErrorKind::Internal,
                    "summary_failed",
                    "The summary's task ended before it finished.",
                )));
            }
        };
        if let Some(usage) = summarized.usage {
            self.out_of_band(turn, &summarized.model, &summarized.role, usage)
                .await?;
        }
        Ok(summarized.outcome)
    }

    /// Counts what a request outside the conversation used, in the
    /// session's usage, the turn's, and every account of the ledger, and
    /// records it as a `usage` entry.
    async fn out_of_band(
        &mut self,
        turn: &mut Turn,
        model: &RoleModel,
        role: &ModelRole,
        usage: Usage,
    ) -> Result<RequestUsage, StoreError> {
        let record = self.usage_record_as(model, role.clone(), usage, None);
        self.usage.add(&record);
        self.ledger.charge(&record);
        add_request(&mut turn.usage, &record);
        self.shared.lock().usage = self.usage.snapshot();
        self.store
            .append(
                &turn.id,
                Record::Usage {
                    purpose: "compaction".to_owned(),
                    usage: record.clone(),
                },
            )
            .await?;
        self.tell_usage(turn, model, None, true);
        Ok(record)
    }

    /// Stops a summary that runs in the background, and forgets one that
    /// is ready: the history it was taken over is not the session's any
    /// more.
    pub(super) fn drop_background(&mut self) {
        if let Some(Background::Running(job)) = self.background.take() {
            self.give_up(job);
        }
        self.background_failed = None;
    }

    /// Stops a summary's request. One that already finished is kept, to be
    /// counted at the next boundary: what it used was spent.
    fn give_up(&mut self, job: Job) {
        if job.handle.is_finished() {
            self.given_up.push(job.handle);
        } else {
            job.cancel.cancel();
            job.handle.abort();
        }
    }

    /// Stops a summary that runs in the background: the turn was
    /// interrupted, and an interrupt means stop. One that is ready, or whose
    /// request has finished, stays.
    pub(super) fn stop_background(&mut self) {
        if matches!(&self.background, Some(Background::Running(job)) if !job.handle.is_finished()) {
            self.drop_background();
        }
    }

    /// How many messages a compaction made now summarises: all of them, or
    /// with `keep_recent_tokens`, all but the most recent turns, never
    /// leaving a round's results without their calls.
    fn covered_now(&self) -> usize {
        let history = &self.context.history;
        let keep = self.context.parts.settings.keep_recent_tokens;
        let count = history.len();
        if keep == 0 {
            return count;
        }
        let messages = history.messages();
        let mut covered = count;
        for index in (1..count).rev() {
            if self.context.budget.tokens(history.bytes_from(index)) > keep {
                break;
            }
            if messages[index].role != Role::Tool {
                covered = index;
            }
        }
        covered
    }

    /// `Compacting`: the summary, then the new epoch.
    pub(super) async fn compacting(&mut self, turn: &mut Turn, prep: Prep) -> Phase {
        self.enter(TurnState::Compacting);
        self.manual = false;
        let focus = self
            .manual_focus
            .take()
            .filter(|focus| !focus.trim().is_empty());
        let notice = |state, message: String, before: Option<u64>, after: Option<u64>| {
            Notice::Compaction(CompactionNotice {
                message,
                state,
                background: false,
                before_tokens: before,
                after_tokens: after,
            })
        };
        if self.context.epoch.is_none() || self.context.history.len() == 0 {
            let message = "There is nothing to compact yet.".to_owned();
            self.notify(
                turn,
                notice(CompactionState::Failed, message.clone(), None, None),
            );
            if prep.only {
                return Phase::Finishing(End::completed());
            }
            return Phase::Finishing(End::failed(ErrorData::new(
                ErrorKind::ContextOverflow,
                "context_overflow",
                message,
            )));
        }
        if prep.overflowed {
            let (window, _) = self.limit(&self.model);
            self.context
                .budget
                .overflowed(self.context.sent_bytes, window);
        }
        let before = self.estimate();
        self.notify(
            turn,
            notice(
                CompactionState::Started,
                "Compacting the conversation.".to_owned(),
                None,
                None,
            ),
        );
        if let Err(error) = self.collect_background(turn).await {
            return Phase::Finishing(End::store(&error));
        }
        // A summary prepared in the background is used when it is of the
        // history the session has now, and the user asked for no focus.
        let usable = |cover: &Cover| cover.holds_for(&self.context.history);
        let background = match self.background.take() {
            Some(Background::Ready(ready)) if focus.is_none() && usable(&ready.covered) => {
                Some((ready.covered, ready.outcome))
            }
            Some(Background::Running(job)) if focus.is_none() && usable(&job.covered) => {
                // The one case in which compaction stops the session: the
                // trigger came before the summary was ready.
                let covered = job.covered.clone();
                let cancel = job.cancel.clone();
                match turn.cancel.race(job.handle).await {
                    None => {
                        cancel.cancel();
                        return Phase::Interrupted(None);
                    }
                    Some(joined) => match self.counted(turn, joined).await {
                        Ok(outcome) => Some((covered, outcome)),
                        Err(error) => return Phase::Finishing(End::store(&error)),
                    },
                }
            }
            Some(Background::Running(job)) => {
                self.give_up(job);
                None
            }
            _ => None,
        };
        let mut attempt = background.map(|(covered, outcome)| (covered, outcome, true));
        loop {
            let (covered, outcome, from_background) = match attempt.take() {
                Some(taken) => taken,
                None => {
                    let messages = self.covered_now();
                    let cover = Cover {
                        messages,
                        through: self.context.history.end_of(messages),
                    };
                    let (role, model) = self.summary_model();
                    // A summary is a request: the ledger is asked first.
                    if let Err(reached) = self.ledger.take_request() {
                        self.notify(
                            turn,
                            notice(
                                CompactionState::Failed,
                                format!(
                                    "The conversation was not compacted: {}",
                                    reached.message()
                                ),
                                None,
                                None,
                            ),
                        );
                        return self.limit_reached(turn, &reached).await;
                    }
                    let request = self.summary_request(
                        &model,
                        &role,
                        messages,
                        focus.as_deref(),
                        prep.overflowed,
                    );
                    let client = Arc::clone(&model.client);
                    let Some((outcome, usage)) = turn
                        .cancel
                        .race(summarize(client, request, turn.cancel.child()))
                        .await
                    else {
                        return Phase::Interrupted(None);
                    };
                    let summarized = Summarized {
                        model,
                        role,
                        outcome,
                        usage,
                    };
                    match self.counted(turn, Ok(summarized)).await {
                        Ok(outcome) => (cover, outcome, false),
                        Err(error) => return Phase::Finishing(End::store(&error)),
                    }
                }
            };
            let failure = match outcome {
                Ok(text) => match self.finish_compaction(turn, &text, &covered).await {
                    Ok(after) => {
                        self.notify(
                            turn,
                            notice(
                                CompactionState::Finished,
                                format!(
                                    "The conversation was compacted: about {before} tokens became \
                                     {after}."
                                ),
                                Some(before),
                                Some(after),
                            ),
                        );
                        return if prep.only {
                            Phase::Finishing(End::completed())
                        } else {
                            Phase::Preparing(Prep {
                                compacted: true,
                                only: false,
                                ..prep
                            })
                        };
                    }
                    Err(Unfinished::Store(error)) => return Phase::Finishing(End::store(&error)),
                    Err(Unfinished::TooLarge(message)) => {
                        ErrorData::new(ErrorKind::ContextOverflow, "summary_too_large", message)
                    }
                },
                Err(error) => error,
            };
            if failure.kind == ErrorKind::Interrupted || turn.cancel.is_cancelled() {
                return Phase::Interrupted(None);
            }
            if from_background {
                // The summary from the background could not be used: one
                // is asked for now, over everything.
                continue;
            }
            self.notify(
                turn,
                notice(CompactionState::Failed, failure.message.clone(), None, None),
            );
            if prep.overflowed {
                turn.stop = Some(StopReason::ContextOverflow);
            }
            // The turn could not go on because the conversation does not
            // fit: that is what it ends with, and why is in the message.
            return Phase::Finishing(End::failed(ErrorData::new(
                ErrorKind::ContextOverflow,
                "compaction_failed",
                format!(
                    "The conversation could not be compacted: {}",
                    failure.message
                ),
            )));
        }
    }

    /// Records the summary and the new epoch, and begins it. Returns the
    /// estimate of the new epoch's first request.
    async fn finish_compaction(
        &mut self,
        turn: &Turn,
        text: &str,
        covered: &Cover,
    ) -> Result<u64, Unfinished> {
        let (built, discovered) = self.build_prompt().await;
        self.warn_instructions(turn, &discovered);
        let root = self.root();
        let today = date_of(self.clock.now());
        let carried = Carried {
            plan: self.context.parts.plan.as_ref().map(|board| board.get()),
            files: Carried::files_from(
                &root,
                &self.env.workspace.files.carried(),
                &self.context.touched,
                self.context.carried_before.as_ref(),
            ),
            last_user: self.context.last_user.clone(),
            artifacts: self.context.artifacts.iter().flatten().cloned().collect(),
        };
        let item = SummaryItem {
            id: self.item_ids.next_id(),
            text: carried.render(text, &today),
            covers_through: covered.through.clone(),
            usage: None,
            created_at: Some(self.clock.now().to_timestamp()),
        };
        // The new epoch's history: the summary, then what it did not cover,
        // without the reasoning that belongs to the old prefix.
        let mut history = History::default();
        history.push(&Item::Summary(item.clone()));
        history.extend_projected(&self.context.history, covered.messages);
        let tools_bytes: u64 = self
            .context
            .tools
            .iter()
            .map(|tool| {
                (tool.name.len() + tool.description.len() + tool.schema.to_string().len()) as u64
            })
            .sum();
        let after = self
            .context
            .budget
            .tokens(built.system.len() as u64 + tools_bytes + history.bytes());
        let (_, limit) = self.limit(&self.model);
        let settings = &self.context.parts.settings;
        if pressure(after, limit, settings.prepare_at, settings.compact_at) == Pressure::Compact {
            return Err(Unfinished::TooLarge(format!(
                "The summary, with what it carries and the turns it replays, is about {after} \
                 tokens, more than a new context of the model allows."
            )));
        }
        let carried_value = carried.to_value();
        self.store
            .append(
                &turn.id,
                Record::Summary(Summary {
                    item: item.clone(),
                    carried: carried_value.clone(),
                }),
            )
            .await
            .map_err(Unfinished::Store)?;
        let epoch = self.epoch_start(&built, &discovered, EpochReason::Compaction);
        self.begin_epoch(turn, epoch, Some(history))
            .await
            .map_err(Unfinished::Store)?;
        self.context.carried_before = Some(carried_value);
        self.context.touched = crate::context::Touched::default();
        self.shared.lock().history = true;
        self.emit(
            turn,
            Event::ItemCompleted(ItemCompleted {
                item: Item::Summary(item),
            }),
        );
        Ok(after)
    }

    /// Before a request is sent: the context record, and in a debug build
    /// the check that the request begins with the one before it.
    pub(super) fn context_request(
        &mut self,
        turn: &Turn,
        model: &RoleModel,
        request: &ModelRequest,
    ) {
        let (window, limit) = self.limit(model);
        let info = &model.entry.info;
        let settings = hash_of(&(
            info.provider.as_str(),
            info.id.as_str(),
            request.max_output,
            format!("{:?}", request.reasoning),
            request.speed.as_str(),
        ));
        let tools: Vec<(&str, &str, String)> = request
            .tools
            .iter()
            .map(|tool| {
                (
                    tool.name.as_str(),
                    tool.description.as_str(),
                    tool.schema.to_string(),
                )
            })
            .collect();
        let shape = Shape {
            settings,
            system: hash_of(&request.system),
            tools: hash_of(&tools),
            messages: self.context.history.fingerprints(),
        };
        let difference = difference(self.context.last_shape.as_ref(), &shape);
        self.context.last_shape = Some(shape);
        self.context.sent_bytes = self.context.content_bytes();
        self.pending_context = Some(ContextStat {
            session: self.shared.info.id.clone(),
            turn: turn.id.clone(),
            model: info.id.clone(),
            epoch: self.context.epoch_number,
            sources: self.context.sources(),
            estimate: self.estimate(),
            limit,
            window,
            difference,
            usage: None,
        });
        if cfg!(debug_assertions) {
            self.check_prefix(model, request, settings);
        }
    }

    /// The debug build's check: within an epoch, and with the same
    /// settings, a request's stable prefix begins with the one before it.
    /// A reload that changed nothing keeps the request before it, so the
    /// history read again from the log is held to the one built live.
    fn check_prefix(&mut self, model: &RoleModel, request: &ModelRequest, settings: u64) {
        let Some(bytes) = model.client.stable_prefix(request) else {
            return;
        };
        let key = hash_of(&(settings, self.context.epoch_number));
        if let Some((last_key, last)) = &self.context.last_prefix
            && *last_key == key
            && let Err(at) = check_prefix(last, &bytes)
        {
            panic!(
                "the request does not begin with the one before it in its context epoch: they \
                 part at byte {at}.\nbefore: {}\nnow:    {}",
                excerpt(last, at),
                excerpt(&bytes, at)
            );
        }
        self.context.last_prefix = Some((key, bytes));
    }

    /// The provider counted the request that has just been answered, and
    /// the answer is recorded: the estimate goes on from there.
    pub(super) fn context_counted(&mut self, usage: &RequestUsage) {
        let tokens = &usage.tokens;
        let input = tokens.input + tokens.cache_read + tokens.cache_write;
        let after = self.context.content_bytes();
        self.context
            .budget
            .counted(self.context.sent_bytes, input, tokens.output, after);
    }

    /// The turn ends because the trigger is reached and automatic
    /// compaction is off: the loop stops and asks.
    pub(super) fn compaction_off(&self) -> End {
        let (_, limit) = self.limit(&self.model);
        End::failed(ErrorData::new(
            ErrorKind::ContextOverflow,
            "compaction_needed",
            format!(
                "The conversation is about {} of the {limit} tokens a request may hold, and \
                 automatic compaction is off (`context.auto_compact`). Compact it \
                 (`session/compact`), or start a new session.",
                self.estimate()
            ),
        ))
    }
}
