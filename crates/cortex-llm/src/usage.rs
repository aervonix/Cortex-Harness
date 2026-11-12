//! Usage normalisation and cost (`docs/architecture/04-providers.md`,
//! "Usage and cost").
//!
//! Providers disagree on what "input tokens" includes. Each protocol turns
//! its provider's counts into the one neutral form,
//! [`TokenUsage`], whose classes do not overlap, and this module turns that
//! and a rate card into money.
//!
//! **A request is priced at the speed it ran at.** [`request_usage`] takes
//! the rate card of [`Usage::speed`], which is what the response reported,
//! not of the speed that was asked for. A request that asked for `fast` and
//! was served at `standard` costs the standard price, and its record says
//! both.
//!
//! Money is whole nano-dollars, so sums are exact. A cost that cannot be
//! worked out, because the catalog has no price for something that was
//! used, is absent. It is never a guess.

use cortex_protocol::Speed;
use cortex_protocol::model::{ModelInfo, ModelRole, Prices};
use cortex_protocol::usage::{Billing, Cost, CostBasis, RequestUsage, TokenUsage};

use crate::event::Usage;

/// Input tokens billed at the normal rate, for a provider whose input count
/// **includes** the tokens read from and written to the cache: the count
/// with those taken out. Never below zero, whatever the provider sent.
#[must_use]
pub fn uncached_input(input_including_cache: u64, cache_read: u64, cache_write: u64) -> u64 {
    input_including_cache
        .saturating_sub(cache_read)
        .saturating_sub(cache_write)
}

/// The rate card a model has for a speed: its own prices at `standard`, the
/// offer's at a tier it offers. `None` when no price is known, and for a
/// speed the model does not offer.
#[must_use]
pub fn prices_for<'a>(model: &'a ModelInfo, speed: &Speed) -> Option<&'a Prices> {
    if speed.is_standard() {
        model.prices.as_ref()
    } else {
        model.speed_offer(speed)?.prices.as_ref()
    }
}

/// What a request's tokens cost on a rate card, in nano-dollars. `None`
/// when the card has no price for a class of token that was used.
///
/// - **Size tiers.** A card may have dearer prices for large requests. The
///   size is every input token the request sent: fresh, read from the
///   cache and written to it. The last tier whose threshold the size is
///   over prices the **whole** request.
/// - **Cache writes** are priced per lifetime class. Tokens the provider
///   did not put in a class are priced at the first class the card lists,
///   which is the provider's default lifetime.
/// - The result is rounded to the nearest nano-dollar once, at the end.
#[must_use]
pub fn cost(tokens: &TokenUsage, prices: &Prices) -> Option<u64> {
    let size = tokens
        .input
        .saturating_add(tokens.cache_read)
        .saturating_add(tokens.cache_write);
    let tier = prices
        .tiers
        .iter()
        .rev()
        .find(|tier| size > tier.above_input_tokens);
    let (input, output, cache_read, cache_write) = match tier {
        Some(tier) => (tier.input, tier.output, tier.cache_read, &tier.cache_write),
        None => (
            prices.input,
            prices.output,
            prices.cache_read,
            &prices.cache_write,
        ),
    };

    // Token counts times nano-dollars per million tokens.
    let mut total: u128 = 0;
    let mut add = |count: u64, price: u64| total += u128::from(count) * u128::from(price);
    add(tokens.input, input);
    add(tokens.output, output);
    if tokens.cache_read > 0 {
        add(tokens.cache_read, cache_read?);
    }
    let mut unclassed = tokens.cache_write;
    for class in &tokens.cache_write_classes {
        if class.tokens == 0 {
            continue;
        }
        let price = cache_write
            .iter()
            .find(|price| price.class == class.class)?;
        add(class.tokens, price.price);
        unclassed = unclassed.saturating_sub(class.tokens);
    }
    if unclassed > 0 {
        add(unclassed, cache_write.first()?.price);
    }
    u64::try_from((total + 500_000) / 1_000_000).ok()
}

/// The record of one request: what it used, at which speed, and what it
/// cost.
///
/// The cost is the provider's own figure where the response stated one.
/// Otherwise it is the tokens priced on the rate card of the speed the
/// request **ran at**: `metered` when the usage is billed per token, an
/// `estimate` when it is not. `catalog_version` is the version of the
/// catalog whose prices were used, stored so that a later price change
/// does not rewrite history.
///
/// The two timings are left empty: they are the caller's to fill in.
#[must_use]
pub fn request_usage(
    usage: Usage,
    model: &ModelInfo,
    role: ModelRole,
    billing: Billing,
    catalog_version: &str,
) -> RequestUsage {
    let (cost, priced_by_catalog) = match usage.provider_cost {
        Some(usd_nanos) => (
            Some(Cost {
                usd_nanos,
                basis: CostBasis::Recorded,
            }),
            false,
        ),
        None => {
            let basis = if billing == Billing::Metered {
                CostBasis::Metered
            } else {
                CostBasis::Estimate
            };
            let cost = prices_for(model, &usage.speed)
                .and_then(|prices| cost(&usage.tokens, prices))
                .map(|usd_nanos| Cost { usd_nanos, basis });
            let priced = cost.is_some();
            (cost, priced)
        }
    };
    RequestUsage {
        provider: model.provider.clone(),
        model: model.id.clone(),
        role,
        speed: usage.speed,
        speed_requested: usage.speed_requested,
        tokens: usage.tokens,
        cost,
        catalog_version: priced_by_catalog.then(|| catalog_version.to_owned()),
        billing,
        time_to_first_token_ms: None,
        duration_ms: None,
        raw: usage.raw,
    }
}

#[cfg(test)]
mod tests {
    use cortex_protocol::model::{CacheWritePrice, PriceTier};
    use cortex_protocol::usage::CacheWriteTokens;

    use super::*;
    use crate::catalog::Catalog;

    /// Dollars per million tokens, as nano-dollars per million.
    const fn usd(dollars: u64) -> u64 {
        dollars * 1_000_000_000
    }

    fn write(class: &str, price: u64) -> CacheWritePrice {
        CacheWritePrice {
            class: class.to_owned(),
            price,
        }
    }

    fn card() -> Prices {
        Prices {
            input: usd(4),
            output: usd(20),
            cache_read: Some(200_000_000),
            cache_write: vec![write("5m", usd(5)), write("1h", usd(8))],
            tiers: Vec::new(),
        }
    }

    #[test]
    fn a_count_that_includes_cached_tokens_is_split_and_never_negative() {
        assert_eq!(uncached_input(1000, 600, 100), 300);
        assert_eq!(uncached_input(1000, 1000, 0), 0);
        assert_eq!(
            uncached_input(100, 600, 0),
            0,
            "a provider's bad arithmetic"
        );
    }

    #[test]
    fn each_class_of_token_is_priced_at_its_own_rate() {
        let tokens = TokenUsage {
            input: 1_000,
            cache_read: 50_000,
            cache_write: 3_000,
            cache_write_classes: vec![
                CacheWriteTokens {
                    class: "5m".to_owned(),
                    tokens: 2_000,
                },
                CacheWriteTokens {
                    class: "1h".to_owned(),
                    tokens: 1_000,
                },
            ],
            output: 500,
            reasoning: Some(200),
        };
        // 1000 × $4 + 500 × $20 + 50000 × $0.20 + 2000 × $5 + 1000 × $8,
        // all per million: $0.004 + $0.01 + $0.01 + $0.01 + $0.008.
        assert_eq!(cost(&tokens, &card()), Some(42_000_000));
    }

    #[test]
    fn reasoning_is_part_of_output_and_is_not_charged_twice() {
        let with = TokenUsage {
            output: 1_000,
            reasoning: Some(900),
            ..TokenUsage::default()
        };
        let without = TokenUsage {
            output: 1_000,
            ..TokenUsage::default()
        };
        assert_eq!(cost(&with, &card()), cost(&without, &card()));
        assert_eq!(cost(&with, &card()), Some(20_000_000));
    }

    #[test]
    fn cache_writes_with_no_class_take_the_first_class_listed() {
        let tokens = TokenUsage {
            cache_write: 1_000_000,
            ..TokenUsage::default()
        };
        assert_eq!(cost(&tokens, &card()), Some(usd(5)));
        // Part classed, the rest not.
        let tokens = TokenUsage {
            cache_write: 1_000_000,
            cache_write_classes: vec![CacheWriteTokens {
                class: "1h".to_owned(),
                tokens: 400_000,
            }],
            ..TokenUsage::default()
        };
        assert_eq!(
            cost(&tokens, &card()),
            Some(400_000 * 8_000 + 600_000 * 5_000)
        );
    }

    #[test]
    fn a_missing_price_for_something_used_gives_no_cost_not_a_guess() {
        let bare = Prices {
            input: usd(1),
            output: usd(2),
            cache_read: None,
            cache_write: Vec::new(),
            tiers: Vec::new(),
        };
        let plain = TokenUsage {
            input: 1_000_000,
            output: 1_000_000,
            ..TokenUsage::default()
        };
        assert_eq!(
            cost(&plain, &bare),
            Some(usd(3)),
            "what is priced is priced"
        );

        let read = TokenUsage {
            cache_read: 1,
            ..plain.clone()
        };
        assert_eq!(cost(&read, &bare), None);
        let written = TokenUsage {
            cache_write: 1,
            ..plain.clone()
        };
        assert_eq!(cost(&written, &bare), None);
        let odd_class = TokenUsage {
            cache_write: 10,
            cache_write_classes: vec![CacheWriteTokens {
                class: "24h".to_owned(),
                tokens: 10,
            }],
            ..plain
        };
        assert_eq!(cost(&odd_class, &card()), None);
    }

    #[test]
    fn a_large_request_is_priced_whole_at_its_size_tier() {
        let tiered = Prices {
            input: 100_000_000,
            output: 500_000_000,
            cache_read: Some(10_000_000),
            cache_write: vec![write("5m", 125_000_000)],
            tiers: vec![PriceTier {
                above_input_tokens: 100_000,
                input: 500_000_000,
                output: 2_500_000_000,
                cache_read: Some(50_000_000),
                cache_write: vec![write("5m", 625_000_000)],
            }],
        };
        let at = |input: u64, cache_read: u64| TokenUsage {
            input,
            cache_read,
            output: 1_000,
            ..TokenUsage::default()
        };
        // Exactly at the threshold is not over it.
        assert_eq!(cost(&at(100_000, 0), &tiered), Some(10_000_000 + 500_000));
        assert_eq!(cost(&at(100_001, 0), &tiered), Some(50_000_500 + 2_500_000));
        // Cache reads count towards the size, and are priced at the tier too.
        assert_eq!(
            cost(&at(1, 100_000), &tiered),
            Some(500 + 5_000_000 + 2_500_000)
        );

        // With several tiers, the last one passed applies.
        let mut two = tiered.clone();
        two.tiers.push(PriceTier {
            above_input_tokens: 500_000,
            input: usd(1),
            output: usd(5),
            cache_read: None,
            cache_write: Vec::new(),
        });
        assert_eq!(cost(&at(200_000, 0), &two), Some(100_000_000 + 2_500_000));
        assert_eq!(cost(&at(600_000, 0), &two), Some(600_000_000 + 5_000_000));
    }

    #[test]
    fn rounding_is_to_the_nearest_nano_dollar_and_huge_counts_do_not_overflow() {
        // $0.075 per million is 75 nano-dollars a token: exact.
        let fine = Prices {
            input: 75_000_000,
            output: 0,
            cache_read: None,
            cache_write: Vec::new(),
            tiers: Vec::new(),
        };
        let one = TokenUsage {
            input: 1,
            ..TokenUsage::default()
        };
        assert_eq!(cost(&one, &fine), Some(75));
        // A price finer than a nano-dollar a token rounds, half up.
        let finer = Prices {
            input: 1_499_999,
            ..fine.clone()
        };
        assert_eq!(cost(&one, &finer), Some(1));
        let half = Prices {
            input: 1_500_000,
            ..fine.clone()
        };
        assert_eq!(cost(&one, &half), Some(2));

        let huge = TokenUsage {
            input: u64::MAX,
            ..TokenUsage::default()
        };
        assert_eq!(
            cost(&huge, &card()),
            None,
            "more money than a u64 of nano-dollars"
        );
        let nearly_free = Prices {
            input: 500_000,
            ..fine
        };
        assert_eq!(cost(&huge, &nearly_free), Some(u64::MAX / 2 + 1));
    }

    /// A model with a fast tier at twice the price, from a catalog text.
    fn model() -> ModelInfo {
        let catalog = Catalog::from_json(
            r#"{"version": "test-1", "model": [{
                "provider": "p", "id": "m", "name": "M", "protocol": "test",
                "context_window": 1000, "max_output": 100,
                "prices": {"input": 4, "output": 20},
                "speed": [
                    {"name": "fast", "label": "Fast", "effect": "faster_output",
                     "when_unavailable": "error", "prices": {"input": 8, "output": 40}},
                    {"name": "flex", "label": "Flex", "effect": "deferred",
                     "when_unavailable": "error"}
                ]}]}"#,
        )
        .unwrap();
        catalog.models()[0].info.clone()
    }

    fn used(speed: Speed, requested: Option<Speed>) -> Usage {
        Usage {
            tokens: TokenUsage {
                input: 1_000_000,
                output: 1_000_000,
                ..TokenUsage::default()
            },
            speed,
            speed_requested: requested,
            provider_cost: None,
            raw: None,
        }
    }

    #[test]
    fn a_request_is_priced_at_the_speed_it_ran_at() {
        let model = model();
        let fast = request_usage(
            used(Speed::Fast, None),
            &model,
            ModelRole::Main,
            Billing::Metered,
            "test-1",
        );
        assert_eq!(fast.speed, Speed::Fast);
        assert_eq!(fast.speed_requested, None);
        assert_eq!(
            fast.cost,
            Some(Cost {
                usd_nanos: usd(48),
                basis: CostBasis::Metered
            })
        );
        assert_eq!(fast.catalog_version.as_deref(), Some("test-1"));
        assert_eq!((fast.provider.as_str(), fast.model.as_str()), ("p", "m"));

        // Asked for fast, served standard: the standard price, and both
        // speeds on the record.
        let downgraded = request_usage(
            used(Speed::Standard, Some(Speed::Fast)),
            &model,
            ModelRole::Main,
            Billing::Metered,
            "test-1",
        );
        assert_eq!(downgraded.speed, Speed::Standard);
        assert_eq!(downgraded.speed_requested, Some(Speed::Fast));
        assert_eq!(downgraded.cost.map(|cost| cost.usd_nanos), Some(usd(24)));
    }

    #[test]
    fn a_cost_says_what_kind_of_number_it_is() {
        let model = model();
        let subscription = request_usage(
            used(Speed::Standard, None),
            &model,
            ModelRole::Fast,
            Billing::Subscription,
            "test-1",
        );
        assert_eq!(
            subscription.cost,
            Some(Cost {
                usd_nanos: usd(24),
                basis: CostBasis::Estimate
            })
        );
        assert_eq!(subscription.role, ModelRole::Fast);

        // The provider's own figure wins, and no catalog priced it.
        let stated = request_usage(
            Usage {
                provider_cost: Some(123),
                ..used(Speed::Fast, None)
            },
            &model,
            ModelRole::Main,
            Billing::Metered,
            "test-1",
        );
        assert_eq!(
            stated.cost,
            Some(Cost {
                usd_nanos: 123,
                basis: CostBasis::Recorded
            })
        );
        assert_eq!(stated.catalog_version, None);
    }

    #[test]
    fn a_speed_with_no_price_shows_tokens_only() {
        let model = model();
        for speed in [
            Speed::Other("flex".to_owned()),
            Speed::Other("unheard-of".to_owned()),
        ] {
            let record = request_usage(
                used(speed.clone(), None),
                &model,
                ModelRole::Main,
                Billing::Metered,
                "test-1",
            );
            assert_eq!(record.cost, None, "{speed:?}");
            assert_eq!(record.catalog_version, None);
            assert_eq!(record.tokens.input, 1_000_000);
        }
        assert!(prices_for(&model, &Speed::Standard).is_some());
        assert!(prices_for(&model, &Speed::Fast).is_some());
    }
}
