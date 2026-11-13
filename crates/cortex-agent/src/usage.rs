//! A session's running usage: what `usage.updated` carries
//! (`docs/architecture/15-efficiency.md`, "Live usage view").

use cortex_base::UtcTime;
use cortex_protocol::ProviderId;
use cortex_protocol::model::ModelRole;
use cortex_protocol::usage::{
    Cost, LimitSource, LimitWindow, ModelUsage, ProviderLimits, RequestUsage, SessionUsage,
    TokenUsage, UsageTotals,
};

/// Adds one request's tokens to a total, class by class.
fn add_tokens(total: &mut TokenUsage, tokens: &TokenUsage) {
    total.input = total.input.saturating_add(tokens.input);
    total.cache_read = total.cache_read.saturating_add(tokens.cache_read);
    total.cache_write = total.cache_write.saturating_add(tokens.cache_write);
    total.output = total.output.saturating_add(tokens.output);
    if let Some(reasoning) = tokens.reasoning {
        total.reasoning = Some(total.reasoning.unwrap_or(0).saturating_add(reasoning));
    }
    for class in &tokens.cache_write_classes {
        match total
            .cache_write_classes
            .iter_mut()
            .find(|known| known.class == class.class)
        {
            Some(known) => known.tokens = known.tokens.saturating_add(class.tokens),
            None => total.cache_write_classes.push(class.clone()),
        }
    }
}

/// Adds one request to a row of totals. Costs are kept one amount per
/// basis: amounts with different bases are never added together.
pub(crate) fn add_request(totals: &mut UsageTotals, usage: &RequestUsage) {
    add_tokens(&mut totals.tokens, &usage.tokens);
    totals.requests = totals.requests.saturating_add(1);
    if let Some(cost) = &usage.cost {
        match totals
            .costs
            .iter_mut()
            .find(|known| known.basis == cost.basis)
        {
            Some(known) => known.usd_nanos = known.usd_nanos.saturating_add(cost.usd_nanos),
            None => totals.costs.push(Cost {
                usd_nanos: cost.usd_nanos,
                basis: cost.basis.clone(),
            }),
        }
    }
}

/// Every input token a request sent: fresh, read from the cache and
/// written to it.
fn input_sent(tokens: &TokenUsage) -> u64 {
    tokens
        .input
        .saturating_add(tokens.cache_read)
        .saturating_add(tokens.cache_write)
}

/// The usage of one session, kept current request by request.
#[derive(Debug, Clone, Default)]
pub(crate) struct UsageBook {
    usage: SessionUsage,
}

impl UsageBook {
    /// The usage as it stands.
    pub(crate) fn snapshot(&self) -> SessionUsage {
        self.usage.clone()
    }

    /// The usage as it would stand with a request that is still running:
    /// for the updates sent while a response arrives.
    pub(crate) fn with_running(&self, running: &RequestUsage) -> SessionUsage {
        let mut book = self.clone();
        book.add(running);
        book.usage
    }

    /// Adds a request that has ended.
    pub(crate) fn add(&mut self, request: &RequestUsage) {
        add_request(&mut self.usage.totals, request);
        let row = self.usage.by_model.iter_mut().find(|row| {
            row.provider == request.provider
                && row.model == request.model
                && row.role == request.role
                && row.speed == request.speed
        });
        match row {
            Some(row) => add_request(&mut row.totals, request),
            None => {
                let mut totals = UsageTotals::default();
                add_request(&mut totals, request);
                self.usage.by_model.push(ModelUsage {
                    provider: request.provider.clone(),
                    model: request.model.clone(),
                    role: request.role.clone(),
                    speed: request.speed.clone(),
                    totals,
                });
            }
        }

        let sent = input_sent(&self.usage.totals.tokens);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a share of token counts, shown to two decimal places"
        )]
        {
            self.usage.cache_hit_rate =
                (sent > 0).then(|| self.usage.totals.tokens.cache_read as f64 / sent as f64);
            // Output speed, from the first token to the end of the response.
            self.usage.tokens_per_second =
                match (request.duration_ms, request.time_to_first_token_ms) {
                    (Some(total), Some(first)) if total > first && request.tokens.output > 0 => {
                        Some(request.tokens.output as f64 * 1000.0 / (total - first) as f64)
                    }
                    _ => None,
                };
        }
        if request.role == ModelRole::Main {
            self.usage.context_tokens =
                Some(input_sent(&request.tokens).saturating_add(request.tokens.output));
        }
        self.usage.last_request = Some(request.clone());
    }

    /// Records the limits a provider reported with a response.
    pub(crate) fn set_limits(
        &mut self,
        provider: &ProviderId,
        windows: Vec<LimitWindow>,
        seen_at: UtcTime,
    ) {
        let limits = ProviderLimits {
            provider: provider.clone(),
            windows,
            seen_at: seen_at.to_timestamp(),
            source: LimitSource::Headers,
        };
        match self
            .usage
            .limits
            .iter_mut()
            .find(|known| known.provider == *provider)
        {
            Some(known) => *known = limits,
            None => self.usage.limits.push(limits),
        }
    }
}

#[cfg(test)]
mod tests {
    use cortex_protocol::usage::{Billing, CacheWriteTokens, CostBasis, LimitMeasure};
    use cortex_protocol::{ModelId, Speed};

    use super::*;

    fn request(speed: Speed, basis: CostBasis, usd_nanos: u64) -> RequestUsage {
        RequestUsage {
            provider: ProviderId::new("test"),
            model: ModelId::new("m"),
            role: ModelRole::Main,
            speed,
            speed_requested: None,
            tokens: TokenUsage {
                input: 100,
                cache_read: 300,
                cache_write: 100,
                cache_write_classes: vec![CacheWriteTokens {
                    class: "5m".to_owned(),
                    tokens: 100,
                }],
                output: 50,
                reasoning: Some(10),
            },
            cost: Some(Cost { usd_nanos, basis }),
            catalog_version: None,
            billing: Billing::Metered,
            time_to_first_token_ms: Some(500),
            duration_ms: Some(1500),
            raw: None,
        }
    }

    #[test]
    fn requests_add_up_by_class_by_speed_and_by_cost_basis() {
        let mut book = UsageBook::default();
        book.add(&request(Speed::Standard, CostBasis::Metered, 1_000));
        book.add(&request(Speed::Fast, CostBasis::Metered, 2_000));
        book.add(&request(Speed::Fast, CostBasis::Estimate, 400));
        let usage = book.snapshot();

        assert_eq!(usage.totals.requests, 3);
        assert_eq!(usage.totals.tokens.input, 300);
        assert_eq!(usage.totals.tokens.reasoning, Some(30));
        assert_eq!(usage.totals.tokens.cache_write_classes[0].tokens, 300);
        let costs: Vec<(u64, &CostBasis)> = usage
            .totals
            .costs
            .iter()
            .map(|cost| (cost.usd_nanos, &cost.basis))
            .collect();
        assert_eq!(
            costs,
            [(3_000, &CostBasis::Metered), (400, &CostBasis::Estimate)]
        );

        let rows: Vec<(&Speed, u64)> = usage
            .by_model
            .iter()
            .map(|row| (&row.speed, row.totals.requests))
            .collect();
        assert_eq!(rows, [(&Speed::Standard, 1), (&Speed::Fast, 2)]);

        // 900 of the 1500 input tokens sent were read from the cache.
        assert_eq!(usage.cache_hit_rate, Some(0.6));
        // 50 output tokens in the second between the first token and the end.
        assert_eq!(usage.tokens_per_second, Some(50.0));
        assert_eq!(usage.context_tokens, Some(550));
        assert_eq!(usage.last_request.unwrap().speed, Speed::Fast);
    }

    #[test]
    fn a_running_request_shows_without_being_kept() {
        let mut book = UsageBook::default();
        book.add(&request(Speed::Standard, CostBasis::Metered, 1_000));
        let running = book.with_running(&request(Speed::Standard, CostBasis::Metered, 1_000));
        assert_eq!(running.totals.requests, 2);
        assert_eq!(book.snapshot().totals.requests, 1);
    }

    #[test]
    fn a_providers_limits_are_replaced_by_its_next_report() {
        let mut book = UsageBook::default();
        let window = |remaining| LimitWindow {
            measure: LimitMeasure::Requests,
            speed: None,
            limit: Some(100),
            remaining: Some(remaining),
            resets_at: None,
        };
        let now = UtcTime::parse("2026-11-13T10:00:00Z").unwrap();
        let provider = ProviderId::new("test");
        book.set_limits(&provider, vec![window(90)], now);
        book.set_limits(&provider, vec![window(89)], now);
        book.set_limits(&ProviderId::new("other"), vec![window(5)], now);
        let limits = book.snapshot().limits;
        assert_eq!(limits.len(), 2);
        assert_eq!(limits[0].windows[0].remaining, Some(89));
        assert_eq!(limits[0].source, LimitSource::Headers);
    }
}
