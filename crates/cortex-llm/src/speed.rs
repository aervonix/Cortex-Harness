//! Speed-tier cooldowns (`docs/decisions/0027-speed-tiers.md`, "Fallback
//! and cooldown").
//!
//! When a provider refuses a speed tier and the request falls back to
//! standard, the tier goes into a **cooldown** for that provider and model.
//! Until it ends, requests that ask for the tier are sent at standard, and
//! only then is the tier asked for again. The harness does not flip between
//! speeds request by request, because where speeds do not share a prompt
//! cache every flip re-writes the cache for the whole conversation.
//!
//! [`SpeedCooldowns`] is the table of running cooldowns. The
//! [client](crate::ProviderClient) starts them and obeys them. The agent
//! loop holds the same table to show the speed in effect
//! ([`SpeedCooldowns::in_effect`]). It lives in memory for the life of the
//! process: a cooldown is not worth a file.
//!
//! A cooldown is measured on the monotonic clock, so correcting the
//! machine's clock neither ends one early nor stretches it. The date it
//! shows for its end is worked out once, when it starts.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use cortex_base::{SharedClock, UtcTime};
use cortex_protocol::speed::SpeedFallbackReason;
use cortex_protocol::{ModelId, ModelInfo, ProviderId, Speed};

/// The cooldowns that are running, by provider, model and tier.
///
/// Clones share one table.
#[derive(Debug, Clone)]
pub struct SpeedCooldowns {
    clock: SharedClock,
    entries: Arc<Mutex<Vec<Entry>>>,
}

#[derive(Debug)]
struct Entry {
    provider: ProviderId,
    model: ModelId,
    speed: Speed,
    /// When it ends, on the monotonic clock.
    ends: Duration,
    /// The same moment as a date, for display.
    until: UtcTime,
}

impl Entry {
    fn is(&self, provider: &ProviderId, model: &ModelId, speed: &Speed) -> bool {
        self.provider == *provider && self.model == *model && self.speed == *speed
    }
}

/// What a request that asks for a tier finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CooldownStatus {
    /// No cooldown. Ask for the tier.
    Clear,
    /// A cooldown is running. Send at standard.
    Cooling {
        /// When it ends.
        until: UtcTime,
    },
    /// A cooldown was running and its time has passed. Ask for the tier
    /// again. This is reported once, to the first request that finds it,
    /// so that the end can be announced once.
    Ended,
}

/// The speed a request for some speed will really ask for, and why it is
/// not the one that was set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeedInEffect {
    /// The speed the next request will be sent at.
    pub speed: Speed,
    /// Why it differs from the speed that is set. `None` when it does not.
    pub reason: Option<SpeedFallbackReason>,
    /// When the cooldown ends, while one is running.
    pub cooldown_until: Option<UtcTime>,
}

impl SpeedCooldowns {
    /// An empty table that reads this clock.
    #[must_use]
    pub fn new(clock: SharedClock) -> Self {
        Self {
            clock,
            entries: Arc::default(),
        }
    }

    /// Starts a cooldown of `length` for a tier, or restarts the one that is
    /// running. Returns when it ends.
    pub fn start(
        &self,
        provider: &ProviderId,
        model: &ModelId,
        speed: &Speed,
        length: Duration,
    ) -> UtcTime {
        let now = self.clock.now();
        // A length no date can hold ends at the last date there is.
        let until = now
            .checked_add(length)
            .or_else(|| UtcTime::from_unix(253_402_300_799, 0))
            .unwrap_or(now);
        let ends = self.clock.monotonic().saturating_add(length);
        let mut entries = self.lock();
        entries.retain(|entry| !entry.is(provider, model, speed));
        entries.push(Entry {
            provider: provider.clone(),
            model: model.clone(),
            speed: speed.clone(),
            ends,
            until,
        });
        until
    }

    /// What a request that asks for the tier finds. A cooldown whose time
    /// has passed is removed and reported as [`CooldownStatus::Ended`],
    /// once.
    #[must_use]
    pub fn check(&self, provider: &ProviderId, model: &ModelId, speed: &Speed) -> CooldownStatus {
        let now = self.clock.monotonic();
        let mut entries = self.lock();
        let Some(index) = entries
            .iter()
            .position(|entry| entry.is(provider, model, speed))
        else {
            return CooldownStatus::Clear;
        };
        if now < entries[index].ends {
            return CooldownStatus::Cooling {
                until: entries[index].until,
            };
        }
        entries.swap_remove(index);
        CooldownStatus::Ended
    }

    /// When the tier's cooldown ends, while one is running. Unlike
    /// [`check`](Self::check) this changes nothing, so it is the one to use
    /// for display.
    #[must_use]
    pub fn until(&self, provider: &ProviderId, model: &ModelId, speed: &Speed) -> Option<UtcTime> {
        let now = self.clock.monotonic();
        self.lock()
            .iter()
            .find(|entry| entry.is(provider, model, speed) && now < entry.ends)
            .map(|entry| entry.until)
    }

    /// The speed a request to `model` that is set to `speed` will ask for:
    /// the speed itself, or `standard` when the model does not offer it or
    /// while it is cooling down. Changes nothing.
    #[must_use]
    pub fn in_effect(&self, model: &ModelInfo, speed: &Speed) -> SpeedInEffect {
        let unchanged = |speed: &Speed| SpeedInEffect {
            speed: speed.clone(),
            reason: None,
            cooldown_until: None,
        };
        if speed.is_standard() {
            return unchanged(speed);
        }
        if !model.offers(speed) {
            return SpeedInEffect {
                speed: Speed::Standard,
                reason: Some(SpeedFallbackReason::NotOffered),
                cooldown_until: None,
            };
        }
        match self.until(&model.provider, &model.id, speed) {
            Some(until) => SpeedInEffect {
                speed: Speed::Standard,
                reason: Some(SpeedFallbackReason::Cooldown),
                cooldown_until: Some(until),
            },
            None => unchanged(speed),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        // The table is valid after every statement that changes it.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use cortex_base::FakeClock;

    use super::*;

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    fn table() -> (FakeClock, SpeedCooldowns) {
        let clock = FakeClock::starting_at("2026-11-12T10:00:00Z").unwrap();
        let cooldowns = SpeedCooldowns::new(Arc::new(clock.clone()));
        (clock, cooldowns)
    }

    fn ids() -> (ProviderId, ModelId) {
        (ProviderId::new("anthropic"), ModelId::new("a-model"))
    }

    #[test]
    fn a_cooldown_runs_until_its_time_and_ends_once() {
        let (clock, cooldowns) = table();
        let (provider, model) = ids();
        assert_eq!(
            cooldowns.check(&provider, &model, &Speed::Fast),
            CooldownStatus::Clear
        );

        let until = cooldowns.start(&provider, &model, &Speed::Fast, secs(600));
        assert_eq!(until.to_string(), "2026-11-12T10:10:00Z");
        assert_eq!(
            cooldowns.check(&provider, &model, &Speed::Fast),
            CooldownStatus::Cooling { until }
        );
        assert_eq!(cooldowns.until(&provider, &model, &Speed::Fast), Some(until));

        clock.advance(secs(599));
        assert_eq!(
            cooldowns.check(&provider, &model, &Speed::Fast),
            CooldownStatus::Cooling { until }
        );

        clock.advance(secs(1));
        assert_eq!(
            cooldowns.until(&provider, &model, &Speed::Fast),
            None,
            "looking does not report the end"
        );
        assert_eq!(
            cooldowns.check(&provider, &model, &Speed::Fast),
            CooldownStatus::Ended
        );
        assert_eq!(
            cooldowns.check(&provider, &model, &Speed::Fast),
            CooldownStatus::Clear,
            "the end is reported once"
        );
    }

    #[test]
    fn cooldowns_are_kept_apart_by_provider_model_and_tier() {
        let (_clock, cooldowns) = table();
        let (provider, model) = ids();
        cooldowns.start(&provider, &model, &Speed::Fast, secs(60));

        let other_provider = ProviderId::new("elsewhere");
        let other_model = ModelId::new("another-model");
        let other_speed = Speed::Other("priority".to_owned());
        assert_eq!(
            cooldowns.check(&other_provider, &model, &Speed::Fast),
            CooldownStatus::Clear
        );
        assert_eq!(
            cooldowns.check(&provider, &other_model, &Speed::Fast),
            CooldownStatus::Clear
        );
        assert_eq!(
            cooldowns.check(&provider, &model, &other_speed),
            CooldownStatus::Clear
        );
        assert!(matches!(
            cooldowns.check(&provider, &model, &Speed::Fast),
            CooldownStatus::Cooling { .. }
        ));
    }

    #[test]
    fn starting_again_restarts_the_cooldown() {
        let (clock, cooldowns) = table();
        let (provider, model) = ids();
        cooldowns.start(&provider, &model, &Speed::Fast, secs(60));
        clock.advance(secs(50));
        let until = cooldowns.start(&provider, &model, &Speed::Fast, secs(60));
        assert_eq!(until.to_string(), "2026-11-12T10:01:50Z");
        clock.advance(secs(59));
        assert!(matches!(
            cooldowns.check(&provider, &model, &Speed::Fast),
            CooldownStatus::Cooling { .. }
        ));
        clock.advance(secs(1));
        assert_eq!(
            cooldowns.check(&provider, &model, &Speed::Fast),
            CooldownStatus::Ended
        );
    }

    #[test]
    fn a_corrected_wall_clock_does_not_move_the_end() {
        let (clock, cooldowns) = table();
        let (provider, model) = ids();
        let until = cooldowns.start(&provider, &model, &Speed::Fast, secs(600));
        // The machine's clock jumps a day ahead, then back a year.
        clock.set_now(UtcTime::parse("2026-11-13T10:00:00Z").unwrap());
        assert_eq!(
            cooldowns.check(&provider, &model, &Speed::Fast),
            CooldownStatus::Cooling { until }
        );
        clock.set_now(UtcTime::parse("2025-11-12T10:00:00Z").unwrap());
        clock.advance(secs(600));
        assert_eq!(
            cooldowns.check(&provider, &model, &Speed::Fast),
            CooldownStatus::Ended
        );
    }

    #[test]
    fn clones_share_the_table_and_a_huge_length_does_not_overflow() {
        let (_clock, cooldowns) = table();
        let (provider, model) = ids();
        let other = cooldowns.clone();
        let until = other.start(&provider, &model, &Speed::Fast, Duration::MAX);
        assert_eq!(until.to_string(), "9999-12-31T23:59:59Z");
        assert!(matches!(
            cooldowns.check(&provider, &model, &Speed::Fast),
            CooldownStatus::Cooling { .. }
        ));
    }
}
