//! Speed tiers, as the loop handles them
//! (`docs/decisions/0027-speed-tiers.md`).
//!
//! The loop does three things with speed.
//!
//! - **It sends each request at the speed its role is set to.** The client
//!   decides what is really sent: `standard` when the model does not offer
//!   the tier or while the tier is cooling down. Because the request still
//!   says what was asked for, the usage records both, and the client can
//!   notice the end of a cooldown.
//! - **It turns what the stream says into notices.** A fallback becomes a
//!   `speed_fallback` notice and a cooldown's start and end a
//!   `speed_cooldown` notice. Every request sent at `standard` during a
//!   cooldown carries a fallback event, so a fallback is shown only when it
//!   is news ([`Moves`]).
//! - **It keeps the session state's speed in effect current,** from the
//!   same table of cooldowns the client obeys.

use std::collections::BTreeMap;

use cortex_base::UtcTime;
use cortex_llm::{CooldownChange, SpeedCooldowns, SpeedMove};
use cortex_protocol::model::{ModelInfo, ModelRole};
use cortex_protocol::session::RoleState;
use cortex_protocol::speed::{
    CooldownState, SpeedCooldown, SpeedFallback, SpeedFallbackBy, SpeedFallbackReason,
};
use cortex_protocol::Speed;

use crate::config::RoleModel;

/// The speed a session and its roles are set to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpeedSettings {
    /// The session's speed: what the `main` role runs at unless it has a
    /// speed of its own.
    pub session: Speed,
    /// The roles that have a speed of their own.
    pub roles: BTreeMap<ModelRole, Speed>,
}

impl SpeedSettings {
    /// The speed a role's requests ask for: its own; without one, the
    /// session's for `main` and `standard` for a utility role, which does
    /// not follow the session's speed.
    #[must_use]
    pub fn of(&self, role: &ModelRole) -> Speed {
        match self.roles.get(role) {
            Some(speed) => speed.clone(),
            None if *role == ModelRole::Main => self.session.clone(),
            None => Speed::Standard,
        }
    }
}

/// What a role is set to and what is in effect, for the session's state.
pub(crate) fn role_state(
    role: &ModelRole,
    model: &RoleModel,
    settings: &SpeedSettings,
    cooldowns: &SpeedCooldowns,
) -> RoleState {
    let info = &model.entry.info;
    let speed = settings.of(role);
    let in_effect = cooldowns.in_effect(info, &speed);
    RoleState {
        provider: info.provider.clone(),
        model: info.id.clone(),
        effort: model.effort.clone(),
        effort_in_effect: model.reasoning.level.clone(),
        speed,
        speed_in_effect: in_effect.speed,
        speed_cooldown_until: in_effect.cooldown_until.map(UtcTime::to_timestamp),
    }
}

/// What a tier is called: the provider's label for it where the model has
/// an offer, otherwise its name.
fn label(model: &ModelInfo, speed: &Speed) -> String {
    model
        .speed_offer(speed)
        .map_or_else(|| format!("{speed} speed"), |offer| offer.label.clone())
}

fn until(time: Option<UtcTime>) -> String {
    time.map_or_else(String::new, |time| format!(" until {time}"))
}

/// The `speed_fallback` notice for a request that runs, or ran, at another
/// speed than its role is set to.
pub(crate) fn fallback_notice(role: &ModelRole, model: &ModelInfo, moved: &SpeedMove) -> SpeedFallback {
    let tier = label(model, &moved.requested);
    let served = &moved.served;
    let message = if moved.by == SpeedFallbackBy::Provider {
        format!(
            "The provider served this request at {served} speed, not {}. It is billed at the \
             {served} rate.",
            moved.requested
        )
    } else {
        match moved.reason {
            SpeedFallbackReason::RateLimited => format!(
                "{tier} is rate limited. Running at {served} speed{}.",
                until(moved.cooldown_until)
            ),
            SpeedFallbackReason::Overloaded => format!(
                "{tier} has no capacity right now. Running at {served} speed{}.",
                until(moved.cooldown_until)
            ),
            SpeedFallbackReason::Cooldown => format!(
                "{tier} is paused{}. Running at {served} speed.",
                until(moved.cooldown_until)
            ),
            SpeedFallbackReason::NotOffered => format!(
                "{} does not offer {} speed. Running at {served} speed.",
                model.display_name, moved.requested
            ),
            _ => format!(
                "Running at {served} speed, not {}{}.",
                moved.requested,
                until(moved.cooldown_until)
            ),
        }
    };
    SpeedFallback {
        message,
        role: role.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        requested: moved.requested.clone(),
        served: moved.served.clone(),
        by: moved.by.clone(),
        reason: moved.reason.clone(),
        cooldown_until: moved.cooldown_until.map(UtcTime::to_timestamp),
    }
}

/// The `speed_cooldown` notice for a cooldown that began or ended.
pub(crate) fn cooldown_notice(model: &ModelInfo, change: &CooldownChange) -> SpeedCooldown {
    let tier = label(model, &change.speed);
    let message = if change.state == CooldownState::Ended {
        format!("{tier} is back.")
    } else {
        format!("{tier} is paused{}.", until(change.until))
    };
    SpeedCooldown {
        message,
        provider: model.provider.clone(),
        model: model.id.clone(),
        speed: change.speed.clone(),
        state: change.state.clone(),
        until: change.until.map(UtcTime::to_timestamp),
    }
}

/// What makes two fallbacks the same news. During a cooldown it is the
/// cooldown, whatever started it: the request that was refused and the
/// requests sent at `standard` after it say one thing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MoveKey {
    requested: Speed,
    served: Speed,
    by: SpeedFallbackBy,
    cooldown_until: Option<UtcTime>,
    /// Only for a move with no cooldown.
    reason: Option<SpeedFallbackReason>,
}

/// Remembers the last fallback shown for each role, so that the same one is
/// not shown for every request.
///
/// **A fallback is shown when it is news:** the first of a cooldown, the
/// first for a tier the model does not offer, the first of a run of
/// requests the provider served at another speed. It is news again after a
/// request of that role has run at the speed that was asked for, and after
/// the role's setting changes.
#[derive(Debug, Clone, Default)]
pub(crate) struct Moves {
    shown: BTreeMap<ModelRole, MoveKey>,
}

impl Moves {
    /// Whether to show this fallback. Remembers it if so.
    pub(crate) fn is_news(&mut self, role: &ModelRole, moved: &SpeedMove) -> bool {
        let key = MoveKey {
            requested: moved.requested.clone(),
            served: moved.served.clone(),
            by: moved.by.clone(),
            cooldown_until: moved.cooldown_until,
            reason: moved
                .cooldown_until
                .is_none()
                .then(|| moved.reason.clone()),
        };
        if self.shown.get(role) == Some(&key) {
            return false;
        }
        self.shown.insert(role.clone(), key);
        true
    }

    /// The role's request ran at the speed it asked for, or its setting
    /// changed: the next fallback is news.
    pub(crate) fn forget(&mut self, role: &ModelRole) {
        self.shown.remove(role);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moved(by: SpeedFallbackBy, reason: SpeedFallbackReason, until: Option<&str>) -> SpeedMove {
        SpeedMove {
            requested: Speed::Fast,
            served: Speed::Standard,
            by,
            reason,
            cooldown_until: until.map(|time| UtcTime::parse(time).unwrap()),
        }
    }

    #[test]
    fn a_utility_role_does_not_follow_the_sessions_speed() {
        let mut settings = SpeedSettings {
            session: Speed::Fast,
            roles: BTreeMap::new(),
        };
        assert_eq!(settings.of(&ModelRole::Main), Speed::Fast);
        assert_eq!(settings.of(&ModelRole::Fast), Speed::Standard);
        assert_eq!(settings.of(&ModelRole::Judge), Speed::Standard);

        settings.roles.insert(ModelRole::Fast, Speed::Fast);
        settings.roles.insert(ModelRole::Main, Speed::Standard);
        assert_eq!(settings.of(&ModelRole::Fast), Speed::Fast);
        assert_eq!(settings.of(&ModelRole::Main), Speed::Standard, "its own wins");
    }

    #[test]
    fn one_cooldown_is_one_piece_of_news() {
        let mut moves = Moves::default();
        let main = ModelRole::Main;
        let refused = moved(
            SpeedFallbackBy::Harness,
            SpeedFallbackReason::RateLimited,
            Some("2026-11-13T10:10:00Z"),
        );
        let cooling = moved(
            SpeedFallbackBy::Harness,
            SpeedFallbackReason::Cooldown,
            Some("2026-11-13T10:10:00Z"),
        );
        assert!(moves.is_news(&main, &refused));
        assert!(!moves.is_news(&main, &cooling), "the same cooldown");
        assert!(!moves.is_news(&main, &cooling));

        // Another role has not been told.
        assert!(moves.is_news(&ModelRole::Fast, &cooling));

        // A later cooldown is news.
        let later = moved(
            SpeedFallbackBy::Harness,
            SpeedFallbackReason::Cooldown,
            Some("2026-11-13T10:30:00Z"),
        );
        assert!(moves.is_news(&main, &later));
    }

    #[test]
    fn a_run_of_downgrades_is_told_once_until_the_tier_is_served_again() {
        let mut moves = Moves::default();
        let main = ModelRole::Main;
        let downgraded = moved(SpeedFallbackBy::Provider, SpeedFallbackReason::Unspecified, None);
        assert!(moves.is_news(&main, &downgraded));
        assert!(!moves.is_news(&main, &downgraded));
        moves.forget(&main);
        assert!(moves.is_news(&main, &downgraded));

        // Another reason with no cooldown is other news.
        let not_offered = moved(SpeedFallbackBy::Harness, SpeedFallbackReason::NotOffered, None);
        assert!(moves.is_news(&main, &not_offered));
        assert!(!moves.is_news(&main, &not_offered));
    }
}
