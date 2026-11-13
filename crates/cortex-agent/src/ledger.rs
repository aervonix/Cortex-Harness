//! Limits as a ledger (`docs/architecture/03-agent-loop.md`, "Loop guards
//! and limits").
//!
//! A limit is an [`Account`]: a budget of money, tokens, requests and time,
//! and what has been spent of it. A session's [`Ledger`] is the accounts it
//! draws on. The first is its lease's budget; another may be shared with
//! other sessions, which is how a group of sessions lives under one hard
//! cap: every clone of an account is the same account.
//!
//! **Every request is charged before it is sent.** In `Preparing` the loop
//! asks each account for one request. An account that is spent refuses, and
//! the turn ends with a limit notice. What a request then uses (tokens and
//! money) is charged when its response ends, so a cap on tokens or money
//! can be passed by the one request that was in flight when it was reached,
//! and by no more.
//!
//! Each turn also has an account of its own, with the limits one turn may
//! not pass. The request limit per turn is one of them.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use cortex_base::SharedClock;
use cortex_protocol::event::LimitKind;
use cortex_protocol::lease::{Budget, Lease};
use cortex_protocol::usage::{RequestUsage, TokenUsage};

/// What has been drawn from an account.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Spent {
    /// Money, in nano-dollars: every cost recorded, whatever its basis.
    pub usd_nanos: u64,
    /// Tokens, input and output together.
    pub tokens: u64,
    /// Requests to a model.
    pub requests: u64,
}

/// One limit: a budget and what has been spent of it.
///
/// Clones are the same account. Give one to several sessions and they
/// spend from one budget.
#[derive(Debug, Clone)]
pub struct Account {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    name: String,
    budget: Budget,
    clock: SharedClock,
    /// When the account was opened, on the monotonic clock. Its wall-clock
    /// budget runs from here.
    opened: Duration,
    spent: Mutex<Spent>,
}

/// An account refused a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitReached {
    /// Which limit.
    pub kind: LimitKind,
    /// The name of the account that refused.
    pub account: String,
    /// The limit, in the unit of its kind: requests, tokens, nano-dollars
    /// or milliseconds.
    pub limit: u64,
}

impl LimitReached {
    /// A stable name for the refusal.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self.kind {
            LimitKind::Requests => "request_limit",
            LimitKind::Cost => "cost_limit",
            LimitKind::Tokens => "token_limit",
            LimitKind::Time => "time_limit",
            _ => "limit",
        }
    }

    /// The refusal in words.
    #[must_use]
    pub fn message(&self) -> String {
        let (what, limit) = match self.kind {
            LimitKind::Requests => ("request", format!("{} requests", self.limit)),
            LimitKind::Cost => ("cost", format_usd(self.limit)),
            LimitKind::Tokens => ("token", format!("{} tokens", self.limit)),
            LimitKind::Time => ("time", format_duration(self.limit)),
            _ => ("configured", self.limit.to_string()),
        };
        format!(
            "The {what} limit of {} was reached ({limit}). Nothing more was sent.",
            self.account
        )
    }
}

fn format_usd(usd_nanos: u64) -> String {
    let cents = usd_nanos / 10_000_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

fn format_duration(millis: u64) -> String {
    let seconds = millis / 1000;
    if seconds >= 60 {
        format!("{} min {} s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds} s")
    }
}

impl Account {
    /// An account with this budget. Its wall-clock time starts now.
    #[must_use]
    pub fn new(name: impl Into<String>, budget: Budget, clock: SharedClock) -> Self {
        let opened = clock.monotonic();
        Self {
            inner: Arc::new(Inner {
                name: name.into(),
                budget,
                clock,
                opened,
                spent: Mutex::new(Spent::default()),
            }),
        }
    }

    /// The account of a lease: its budget, under the lease's id.
    #[must_use]
    pub fn for_lease(lease: &Lease, clock: SharedClock) -> Self {
        Self::new(
            format!("lease {}", lease.id),
            lease.grant.budget.clone(),
            clock,
        )
    }

    /// The account's name, as a limit notice gives it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The budget.
    #[must_use]
    pub fn budget(&self) -> &Budget {
        &self.inner.budget
    }

    /// What has been spent.
    #[must_use]
    pub fn spent(&self) -> Spent {
        *self.lock()
    }

    /// How long the account has been open.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.inner
            .clock
            .monotonic()
            .saturating_sub(self.inner.opened)
    }

    /// Takes one request from the account, unless a limit has been reached.
    /// The check and the charge are one step, so two sessions cannot both
    /// take the last request.
    fn take_request(&self) -> Result<(), LimitReached> {
        let budget = &self.inner.budget;
        let refuse = |kind, limit| LimitReached {
            kind,
            account: self.inner.name.clone(),
            limit,
        };
        let mut spent = self.lock();
        if let Some(limit) = budget.requests
            && spent.requests >= limit
        {
            return Err(refuse(LimitKind::Requests, limit));
        }
        if let Some(limit) = budget.usd_nanos
            && spent.usd_nanos >= limit
        {
            return Err(refuse(LimitKind::Cost, limit));
        }
        if let Some(limit) = budget.tokens
            && spent.tokens >= limit
        {
            return Err(refuse(LimitKind::Tokens, limit));
        }
        if let Some(limit) = budget.wall_clock_ms
            && self.elapsed() >= Duration::from_millis(limit)
        {
            return Err(refuse(LimitKind::Time, limit));
        }
        spent.requests = spent.requests.saturating_add(1);
        Ok(())
    }

    /// Gives back a request that was taken and then not sent.
    fn return_request(&self) {
        let mut spent = self.lock();
        spent.requests = spent.requests.saturating_sub(1);
    }

    fn charge(&self, tokens: u64, usd_nanos: u64) {
        let mut spent = self.lock();
        spent.tokens = spent.tokens.saturating_add(tokens);
        spent.usd_nanos = spent.usd_nanos.saturating_add(usd_nanos);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Spent> {
        // Three numbers, each changed in one statement.
        self.inner
            .spent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Every token a request sent or produced.
fn total_tokens(tokens: &TokenUsage) -> u64 {
    tokens
        .input
        .saturating_add(tokens.cache_read)
        .saturating_add(tokens.cache_write)
        .saturating_add(tokens.output)
}

/// The accounts one session draws on.
#[derive(Debug, Clone)]
pub struct Ledger {
    clock: SharedClock,
    accounts: Vec<Account>,
    turn_budget: Budget,
    turn: Option<Account>,
}

/// The most requests one turn makes, unless configured otherwise.
pub const DEFAULT_REQUESTS_PER_TURN: u64 = 200;

impl Ledger {
    /// A ledger with no account but the turn's own, which allows
    /// [`DEFAULT_REQUESTS_PER_TURN`] requests a turn.
    #[must_use]
    pub fn new(clock: SharedClock) -> Self {
        Self {
            clock,
            accounts: Vec::new(),
            turn_budget: Budget {
                requests: Some(DEFAULT_REQUESTS_PER_TURN),
                ..Budget::default()
            },
            turn: None,
        }
    }

    /// Adds an account the session draws on: its lease's, or one it shares
    /// with other sessions.
    #[must_use]
    pub fn with_account(mut self, account: Account) -> Self {
        self.accounts.push(account);
        self
    }

    /// Sets what one turn may spend. A field that is absent is not limited
    /// per turn.
    #[must_use]
    pub fn with_turn_budget(mut self, budget: Budget) -> Self {
        self.turn_budget = budget;
        self
    }

    /// The accounts, without the turn's own.
    #[must_use]
    pub fn accounts(&self) -> &[Account] {
        &self.accounts
    }

    /// Opens the account of a new turn.
    pub(crate) fn begin_turn(&mut self) {
        self.turn = Some(Account::new(
            "this turn",
            self.turn_budget.clone(),
            std::sync::Arc::clone(&self.clock),
        ));
    }

    /// Charges one request to every account, before it is sent. When an
    /// account refuses, nothing is charged to any.
    pub(crate) fn take_request(&self) -> Result<(), LimitReached> {
        let mut taken: Vec<&Account> = Vec::new();
        for account in self.turn.iter().chain(&self.accounts) {
            if let Err(reached) = account.take_request() {
                for earlier in taken {
                    earlier.return_request();
                }
                return Err(reached);
            }
            taken.push(account);
        }
        Ok(())
    }

    /// Charges what a request used to every account.
    pub(crate) fn charge(&self, usage: &RequestUsage) {
        let tokens = total_tokens(&usage.tokens);
        let usd_nanos = usage.cost.as_ref().map_or(0, |cost| cost.usd_nanos);
        for account in self.turn.iter().chain(&self.accounts) {
            account.charge(tokens, usd_nanos);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cortex_base::FakeClock;
    use cortex_protocol::model::ModelRole;
    use cortex_protocol::usage::{Billing, Cost, CostBasis};
    use cortex_protocol::{ModelId, ProviderId, Speed};

    use super::*;

    fn usage(input: u64, output: u64, usd_nanos: Option<u64>) -> RequestUsage {
        RequestUsage {
            provider: ProviderId::new("test"),
            model: ModelId::new("m"),
            role: ModelRole::Main,
            speed: Speed::Standard,
            speed_requested: None,
            tokens: TokenUsage {
                input,
                output,
                ..TokenUsage::default()
            },
            cost: usd_nanos.map(|usd_nanos| Cost {
                usd_nanos,
                basis: CostBasis::Metered,
            }),
            catalog_version: None,
            billing: Billing::Metered,
            time_to_first_token_ms: None,
            duration_ms: None,
            raw: None,
        }
    }

    fn clock() -> (FakeClock, SharedClock) {
        let clock = FakeClock::default();
        (clock.clone(), Arc::new(clock))
    }

    #[test]
    fn a_turn_has_a_request_limit_of_its_own_that_starts_again() {
        let (_fake, clock) = clock();
        let mut ledger = Ledger::new(clock).with_turn_budget(Budget {
            requests: Some(2),
            ..Budget::default()
        });
        ledger.begin_turn();
        assert_eq!(ledger.take_request(), Ok(()));
        assert_eq!(ledger.take_request(), Ok(()));
        let reached = ledger.take_request().unwrap_err();
        assert_eq!(
            (reached.kind.clone(), reached.limit, reached.code()),
            (LimitKind::Requests, 2, "request_limit")
        );
        assert!(reached.message().contains("this turn"), "{}", reached.message());

        ledger.begin_turn();
        assert_eq!(ledger.take_request(), Ok(()), "a new turn starts at zero");
    }

    #[test]
    fn tokens_and_money_are_charged_after_and_refuse_the_next_request() {
        let (_fake, clock) = clock();
        let account = Account::new(
            "the session",
            Budget {
                usd_nanos: Some(2_000_000_000),
                tokens: Some(1_000),
                ..Budget::default()
            },
            Arc::clone(&clock),
        );
        let mut ledger = Ledger::new(clock).with_account(account.clone());
        ledger.begin_turn();

        assert_eq!(ledger.take_request(), Ok(()));
        ledger.charge(&usage(300, 200, Some(1_500_000_000)));
        assert_eq!(ledger.take_request(), Ok(()));
        // This one passes the cost limit while in flight.
        ledger.charge(&usage(100, 100, Some(1_000_000_000)));
        assert_eq!(
            account.spent(),
            Spent {
                usd_nanos: 2_500_000_000,
                tokens: 700,
                requests: 2
            }
        );
        let reached = ledger.take_request().unwrap_err();
        assert_eq!(reached.kind, LimitKind::Cost);
        assert!(reached.message().contains("$2.00"), "{}", reached.message());

        // A request with no known price costs tokens only.
        let free = Account::new(
            "tokens",
            Budget {
                tokens: Some(500),
                ..Budget::default()
            },
            Arc::new(FakeClock::default()),
        );
        let ledger = Ledger::new(Arc::new(FakeClock::default())).with_account(free.clone());
        ledger.charge(&usage(400, 100, None));
        assert_eq!(free.spent().usd_nanos, 0);
        assert_eq!(ledger.take_request().unwrap_err().kind, LimitKind::Tokens);
    }

    #[test]
    fn sessions_that_share_an_account_share_its_cap() {
        let (_fake, clock) = clock();
        let shared = Account::new(
            "the group",
            Budget {
                requests: Some(3),
                ..Budget::default()
            },
            Arc::clone(&clock),
        );
        let mut first = Ledger::new(Arc::clone(&clock)).with_account(shared.clone());
        let mut second = Ledger::new(clock).with_account(shared.clone());
        first.begin_turn();
        second.begin_turn();

        assert_eq!(first.take_request(), Ok(()));
        assert_eq!(second.take_request(), Ok(()));
        assert_eq!(first.take_request(), Ok(()));
        let reached = second.take_request().unwrap_err();
        assert_eq!(reached.account, "the group");
        assert_eq!(first.take_request().unwrap_err().kind, LimitKind::Requests);
        assert_eq!(shared.spent().requests, 3);
        assert_eq!(first.accounts().len(), 1);
    }

    #[test]
    fn a_refusal_charges_nothing_to_the_accounts_before_it() {
        let (_fake, clock) = clock();
        let open = Account::new("open", Budget::default(), Arc::clone(&clock));
        let spent = Account::new(
            "spent",
            Budget {
                requests: Some(0),
                ..Budget::default()
            },
            Arc::clone(&clock),
        );
        let mut ledger = Ledger::new(clock)
            .with_account(open.clone())
            .with_account(spent);
        ledger.begin_turn();
        assert!(ledger.take_request().is_err());
        assert_eq!(open.spent().requests, 0, "the request was not sent");
    }

    #[test]
    fn a_wall_clock_budget_runs_on_the_monotonic_clock() {
        let (fake, clock) = clock();
        let account = Account::new(
            "the run",
            Budget {
                wall_clock_ms: Some(90_000),
                ..Budget::default()
            },
            Arc::clone(&clock),
        );
        let ledger = Ledger::new(clock).with_account(account.clone());
        assert_eq!(ledger.take_request(), Ok(()));
        fake.advance(Duration::from_secs(89));
        assert_eq!(ledger.take_request(), Ok(()));
        fake.advance(Duration::from_secs(1));
        let reached = ledger.take_request().unwrap_err();
        assert_eq!((reached.kind.clone(), reached.code()), (LimitKind::Time, "time_limit"));
        assert!(reached.message().contains("1 min 30 s"), "{}", reached.message());
        assert_eq!(account.elapsed(), Duration::from_secs(90));
    }

    #[test]
    fn a_lease_gives_its_budget_to_its_account() {
        let lease = Lease::from_wire(serde_json::json!({
            "id": "l_1",
            "grant": {
                "level": "supervised",
                "sandbox": {"read": {"all": true}, "write": {"roots": ["."]}, "network": "off"},
                "budget": {"requests": 7}
            },
            "expires": {"kind": "end_of_session"},
            "granted_by": {"kind": "user"}
        }))
        .unwrap();
        let account = Account::for_lease(&lease, Arc::new(FakeClock::default()));
        assert_eq!(account.name(), "lease l_1");
        assert_eq!(account.budget().requests, Some(7));
    }
}
