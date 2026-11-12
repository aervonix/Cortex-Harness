//! The model catalog: what the harness knows about each model that no
//! provider's API serves (`docs/architecture/04-providers.md`, "Model
//! catalog", and `docs/decisions/0017-model-catalog.md`).
//!
//! The source of truth is `catalog/models.toml` in the repository. The
//! build embeds it, and [`Catalog::builtin`] reads it on first use: no
//! network, at build time or at start-up. A user adds or replaces entries
//! with [`Catalog::overlay`].
//!
//! # The file
//!
//! A `version`, then one `[[model]]` table per provider and model id. The
//! file itself documents every key. Three things are worth knowing here:
//!
//! - **Prices are US dollars per million tokens**, as people read them on a
//!   provider's pricing page. The loader turns them into whole nano-dollars
//!   and refuses a price that is not a whole number of them.
//! - **A speed tier is a `[[model.speed]]` table** with everything that
//!   makes it different from another provider's tier of the same name:
//!   what it buys, its own full rate card, what the provider does when it
//!   cannot serve it, whether it has a rate limit of its own, whether it
//!   shares the prompt cache with standard, and whether it is a preview.
//!   A key that is left out because the provider does not say stays
//!   unknown; it is never filled in.
//! - **An unknown key is an error**, here and in a user's entries. A
//!   misspelt `cache_raed` must not silently become "no cache price".

use std::sync::{Arc, OnceLock};

use cortex_base::{Classified, ErrorKind};
use cortex_protocol::model::{
    CacheWritePrice, Modality, ModelCapabilities, ModelInfo, PriceTier, Prices, ReasoningControl,
    ReasoningControlKind, SpeedOffer,
};
use cortex_protocol::speed::{SpeedEffect, SpeedUnavailable};
use cortex_protocol::{ModelId, ProviderId, Speed};
use serde::Deserialize;

use crate::protocol::Flags;

/// The catalog file, as the build embedded it.
const EMBEDDED: &str = include_str!(concat!(env!("OUT_DIR"), "/catalog.json"));

/// The edit formats a model can be asked to use
/// (`docs/architecture/14-adaptation.md`, "Adapting to the model").
pub const EDIT_FORMATS: &[&str] = &["replace"];
/// The prompt styles a model can be addressed in.
pub const PROMPT_STYLES: &[&str] = &["concise", "guided"];

/// How tools are declared to a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolSchemaStyle {
    /// The provider checks the arguments against the schema.
    Strict,
    /// Only the harness checks them.
    Loose,
}

impl ToolSchemaStyle {
    /// The style as the catalog and the statistics write it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Loose => "loose",
        }
    }
}

/// The variants in effect for a model: one choice at each variant point.
/// The catalog names a default for every model. A request carries the ones
/// in effect, fixed for the context epoch, and they are recorded with every
/// outcome so that their success rates can be compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variants {
    /// The edit format: one of [`EDIT_FORMATS`].
    pub edit_format: String,
    /// How tools are declared.
    pub tool_schema: ToolSchemaStyle,
    /// The prompt style: one of [`PROMPT_STYLES`].
    pub prompt_style: String,
}

/// One model of the catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry {
    /// What the protocol says about the model: limits, prices, reasoning
    /// control, capabilities and the speed tiers it offers. This is what
    /// `models/list` returns.
    pub info: ModelInfo,
    /// The default at each variant point.
    pub variants: Variants,
    /// What the model's protocol needs to know about it that the neutral
    /// fields do not say.
    pub flags: Flags,
    /// Where the entry's facts were read, and when.
    pub source: Option<String>,
}

/// The models the harness knows.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    version: String,
    models: Vec<Arc<ModelEntry>>,
}

/// A catalog, or an entry of one, is not valid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct CatalogError {
    /// What is wrong and where, for a person.
    pub message: String,
}

impl Classified for CatalogError {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Config
    }

    fn code(&self) -> &str {
        "invalid_catalog"
    }
}

impl Catalog {
    /// The catalog built into this binary, read on first use.
    ///
    /// An error here is a mistake in `catalog/models.toml`. A test loads
    /// the built-in catalog, so one cannot be merged.
    pub fn builtin() -> Result<&'static Self, &'static CatalogError> {
        static BUILTIN: OnceLock<Result<Catalog, CatalogError>> = OnceLock::new();
        BUILTIN.get_or_init(|| Self::from_json(EMBEDDED)).as_ref()
    }

    /// Reads a catalog from JSON text with the structure of the catalog
    /// file.
    pub fn from_json(text: &str) -> Result<Self, CatalogError> {
        let mut json = serde_json::Deserializer::from_str(text);
        let catalog = Self::read(&mut json)?;
        json.end().map_err(|error| CatalogError {
            message: format!("the catalog could not be read: {error}"),
        })?;
        Ok(catalog)
    }

    /// Reads a catalog from any self-describing format with the structure
    /// of the catalog file. This is how a user's entries arrive: the
    /// configuration crate reads them as part of its own file and hands
    /// them over.
    pub fn read<'de, D: serde::Deserializer<'de>>(source: D) -> Result<Self, CatalogError> {
        let file = File::deserialize(source).map_err(|error| CatalogError {
            message: format!("the catalog could not be read: {error}"),
        })?;
        if file.version.trim().is_empty() {
            return Err(CatalogError {
                message: "the catalog has no `version`".to_owned(),
            });
        }
        let mut models: Vec<Arc<ModelEntry>> = Vec::with_capacity(file.model.len());
        for raw in file.model {
            let name = format!("model {}/{}", raw.provider, raw.id);
            let entry = raw.into_entry().map_err(|problem| CatalogError {
                message: format!("{name}: {problem}"),
            })?;
            if models.iter().any(|other| same_model(other, &entry)) {
                return Err(CatalogError {
                    message: format!("{name}: is listed twice"),
                });
            }
            models.push(Arc::new(entry));
        }
        Ok(Self {
            version: file.version,
            models,
        })
    }

    /// The catalog's version: what a usage record stores beside a cost
    /// worked out from these prices.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Every model, in the file's order, overlaid entries last.
    #[must_use]
    pub fn models(&self) -> &[Arc<ModelEntry>] {
        &self.models
    }

    /// A model, by provider and id.
    #[must_use]
    pub fn get(&self, provider: &ProviderId, id: &ModelId) -> Option<&Arc<ModelEntry>> {
        self.models
            .iter()
            .find(|entry| entry.info.provider == *provider && entry.info.id == *id)
    }

    /// Adds the entries of `other`. An entry for a provider and model this
    /// catalog already has **replaces** it whole: nothing is merged field
    /// by field, so an overlaid entry says everything about its model. The
    /// version becomes `<this>+<other>`, because prices may now come from
    /// either.
    pub fn overlay(&mut self, other: &Self) {
        for entry in &other.models {
            match self
                .models
                .iter_mut()
                .find(|existing| same_model(existing, entry))
            {
                Some(existing) => *existing = Arc::clone(entry),
                None => self.models.push(Arc::clone(entry)),
            }
        }
        self.version = format!("{}+{}", self.version, other.version);
    }
}

fn same_model(a: &ModelEntry, b: &ModelEntry) -> bool {
    a.info.provider == b.info.provider && a.info.id == b.info.id
}

// The file's structure. Every table refuses a key it does not know.

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: String,
    #[serde(default)]
    model: Vec<RawModel>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawModel {
    provider: String,
    id: String,
    name: String,
    protocol: String,
    context_window: u64,
    max_output: u64,
    #[serde(default)]
    input: Vec<String>,
    prices: Option<RawPrices>,
    reasoning: Option<RawReasoning>,
    #[serde(default)]
    reads_reasoning_from: Vec<String>,
    #[serde(default)]
    capabilities: RawCapabilities,
    #[serde(default)]
    variants: RawVariants,
    #[serde(default)]
    flags: Flags,
    #[serde(default)]
    speed: Vec<RawSpeed>,
    source: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPrices {
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    #[serde(default)]
    cache_write: Vec<RawCacheWrite>,
    #[serde(default)]
    above: Vec<RawPriceTier>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCacheWrite {
    class: String,
    price: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPriceTier {
    input_tokens: u64,
    input: f64,
    output: f64,
    cache_read: Option<f64>,
    #[serde(default)]
    cache_write: Vec<RawCacheWrite>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReasoning {
    #[serde(default)]
    levels: Vec<String>,
    default: Option<String>,
    min_tokens: Option<u64>,
    max_tokens: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCapabilities {
    #[serde(default)]
    strict_tools: bool,
    #[serde(default)]
    mid_conversation_system: bool,
    #[serde(default)]
    structured_output: bool,
    min_cacheable_tokens: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVariants {
    edit_format: Option<String>,
    tool_schema: Option<String>,
    prompt_style: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSpeed {
    name: String,
    label: String,
    effect: String,
    when_unavailable: String,
    prices: Option<RawPrices>,
    #[serde(default)]
    separate_limit: bool,
    shares_cache: Option<bool>,
    #[serde(default)]
    preview: bool,
}

/// What is wrong with an entry.
type Problem = String;

/// Dollars per million tokens to nano-dollars per million tokens. A price
/// is refused when it is negative, not a number, or finer than a
/// nano-dollar: such a figure is a typing mistake.
fn nanos(dollars: f64, what: &str) -> Result<u64, Problem> {
    let scaled = dollars * 1e9;
    let whole = scaled.round();
    let representable = dollars.is_finite() && dollars >= 0.0 && whole < 1.8e19;
    if !representable || (scaled - whole).abs() > 1e-3 {
        return Err(format!(
            "the price `{what}` is {dollars}, which is not an amount of dollars per million tokens"
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked just above: whole, not negative, and below 1.8e19"
    )]
    Ok(whole as u64)
}

fn cache_writes(raw: Vec<RawCacheWrite>, what: &str) -> Result<Vec<CacheWritePrice>, Problem> {
    let mut prices: Vec<CacheWritePrice> = Vec::with_capacity(raw.len());
    for write in raw {
        if write.class.is_empty() || prices.iter().any(|price| price.class == write.class) {
            return Err(format!(
                "{what}: the cache-write class `{}` is empty or listed twice",
                write.class
            ));
        }
        prices.push(CacheWritePrice {
            price: nanos(write.price, &format!("{what}.cache_write.{}", write.class))?,
            class: write.class,
        });
    }
    Ok(prices)
}

impl RawPrices {
    fn into_prices(self, what: &str) -> Result<Prices, Problem> {
        let mut tiers = Vec::with_capacity(self.above.len());
        let mut last = 0;
        for tier in self.above {
            if tier.input_tokens <= last {
                return Err(format!(
                    "{what}: the size tiers must each be above the one before, and above zero"
                ));
            }
            last = tier.input_tokens;
            let at = format!("{what}.above[{last}]");
            tiers.push(PriceTier {
                above_input_tokens: tier.input_tokens,
                input: nanos(tier.input, &format!("{at}.input"))?,
                output: nanos(tier.output, &format!("{at}.output"))?,
                cache_read: tier
                    .cache_read
                    .map(|price| nanos(price, &format!("{at}.cache_read")))
                    .transpose()?,
                cache_write: cache_writes(tier.cache_write, &at)?,
            });
        }
        Ok(Prices {
            input: nanos(self.input, &format!("{what}.input"))?,
            output: nanos(self.output, &format!("{what}.output"))?,
            cache_read: self
                .cache_read
                .map(|price| nanos(price, &format!("{what}.cache_read")))
                .transpose()?,
            cache_write: cache_writes(self.cache_write, what)?,
            tiers,
        })
    }
}

impl RawReasoning {
    fn into_control(self) -> Result<ReasoningControl, Problem> {
        let none = || ReasoningControl {
            kind: ReasoningControlKind::None,
            levels: Vec::new(),
            default: None,
            min_tokens: None,
            max_tokens: None,
        };
        let has_budget = self.min_tokens.is_some() || self.max_tokens.is_some();
        if !self.levels.is_empty() {
            if has_budget {
                return Err("reasoning has both levels and a token budget".to_owned());
            }
            let mut seen: Vec<&str> = Vec::new();
            for level in &self.levels {
                if level.is_empty() || seen.contains(&level.as_str()) {
                    return Err(format!(
                        "the reasoning level `{level}` is empty or listed twice"
                    ));
                }
                seen.push(level);
            }
            if let Some(default) = &self.default
                && !self.levels.contains(default)
            {
                return Err(format!(
                    "the default reasoning level `{default}` is not one of the model's levels"
                ));
            }
            return Ok(ReasoningControl {
                kind: ReasoningControlKind::Levels,
                levels: self.levels,
                default: self.default,
                ..none()
            });
        }
        if self.default.is_some() {
            return Err("reasoning has a default level and no levels".to_owned());
        }
        if has_budget {
            let (Some(min), Some(max)) = (self.min_tokens, self.max_tokens) else {
                return Err(
                    "a reasoning budget needs both `min_tokens` and `max_tokens`".to_owned(),
                );
            };
            if min > max {
                return Err(
                    "the reasoning budget's `min_tokens` is above its `max_tokens`".to_owned(),
                );
            }
            return Ok(ReasoningControl {
                kind: ReasoningControlKind::Budget,
                min_tokens: Some(min),
                max_tokens: Some(max),
                ..none()
            });
        }
        Ok(none())
    }
}

impl RawSpeed {
    fn into_offer(self) -> Result<SpeedOffer, Problem> {
        let speed = Speed::from_wire(&self.name);
        if self.name.is_empty() || speed.is_standard() {
            return Err(
                "a speed tier needs a name, and `standard` is not one: every model has it, at \
                 its own prices"
                    .to_owned(),
            );
        }
        let what = format!("speed `{}`", self.name);
        if self.label.is_empty() {
            return Err(format!("{what} has no `label`"));
        }
        let effect = SpeedEffect::from_wire(&self.effect);
        if !effect.is_known() {
            return Err(format!(
                "{what}: `effect` is `{}`, and must be one of {}",
                self.effect,
                SpeedEffect::KNOWN.join(", ")
            ));
        }
        let when_unavailable = SpeedUnavailable::from_wire(&self.when_unavailable);
        if !when_unavailable.is_known() {
            return Err(format!(
                "{what}: `when_unavailable` is `{}`, and must be one of {}",
                self.when_unavailable,
                SpeedUnavailable::KNOWN.join(", ")
            ));
        }
        Ok(SpeedOffer {
            speed,
            label: self.label,
            effect,
            prices: self
                .prices
                .map(|prices| prices.into_prices(&format!("{what}.prices")))
                .transpose()?,
            when_unavailable,
            separate_limit: self.separate_limit,
            shares_cache: self.shares_cache,
            preview: self.preview,
        })
    }
}

impl RawModel {
    fn into_entry(self) -> Result<ModelEntry, Problem> {
        for (key, value) in [
            ("provider", &self.provider),
            ("id", &self.id),
            ("name", &self.name),
            ("protocol", &self.protocol),
        ] {
            if value.trim().is_empty() {
                return Err(format!("`{key}` is empty"));
            }
        }
        if self.context_window == 0 || self.max_output == 0 {
            return Err("`context_window` and `max_output` must be above zero".to_owned());
        }
        if self.max_output > self.context_window {
            return Err("`max_output` is larger than `context_window`".to_owned());
        }

        let mut input = Vec::with_capacity(self.input.len());
        for modality in &self.input {
            let modality = Modality::from_wire(modality);
            if !modality.is_known() || input.contains(&modality) {
                return Err(format!(
                    "`input` has `{}`, which is listed twice or is not one of {}",
                    modality.as_str(),
                    Modality::KNOWN.join(", ")
                ));
            }
            input.push(modality);
        }

        if self.reads_reasoning_from.contains(&self.id) {
            return Err("`reads_reasoning_from` lists the model itself".to_owned());
        }

        let mut speeds: Vec<SpeedOffer> = Vec::with_capacity(self.speed.len());
        for raw in self.speed {
            let offer = raw.into_offer()?;
            if speeds.iter().any(|other| other.speed == offer.speed) {
                return Err(format!("speed `{}` is listed twice", offer.speed.as_str()));
            }
            speeds.push(offer);
        }

        let capabilities = ModelCapabilities {
            strict_tools: self.capabilities.strict_tools,
            mid_conversation_system: self.capabilities.mid_conversation_system,
            structured_output: self.capabilities.structured_output,
            min_cacheable_tokens: self.capabilities.min_cacheable_tokens,
        };
        let variants = self.variants.into_variants(&capabilities)?;

        Ok(ModelEntry {
            info: ModelInfo {
                id: ModelId::new(self.id),
                provider: ProviderId::new(self.provider),
                display_name: self.name,
                protocol: self.protocol,
                context_window: self.context_window,
                max_output: self.max_output,
                prices: self
                    .prices
                    .map(|prices| prices.into_prices("prices"))
                    .transpose()?,
                reasoning: self.reasoning.unwrap_or_default().into_control()?,
                reads_reasoning_from: self
                    .reads_reasoning_from
                    .into_iter()
                    .map(ModelId::new)
                    .collect(),
                input,
                capabilities,
                speeds,
            },
            variants,
            flags: self.flags,
            source: self.source,
        })
    }
}

impl RawVariants {
    /// The variants an entry names, and for the ones it does not: the
    /// `replace` edit format, strict tool schemas where the model has them,
    /// and the `concise` prompt style.
    fn into_variants(self, capabilities: &ModelCapabilities) -> Result<Variants, Problem> {
        let one_of = |key: &str, value: Option<String>, known: &[&str], default: &str| {
            let value = value.unwrap_or_else(|| default.to_owned());
            if known.contains(&value.as_str()) {
                Ok(value)
            } else {
                Err(format!(
                    "the variant `{key}` is `{value}`, and must be one of {}",
                    known.join(", ")
                ))
            }
        };
        let tool_schema = match self.tool_schema.as_deref() {
            None if capabilities.strict_tools => ToolSchemaStyle::Strict,
            None => ToolSchemaStyle::Loose,
            Some("strict") if capabilities.strict_tools => ToolSchemaStyle::Strict,
            Some("strict") => {
                return Err(
                    "the variant `tool_schema` is `strict`, and the model's capabilities do not \
                     include `strict_tools`"
                        .to_owned(),
                );
            }
            Some("loose") => ToolSchemaStyle::Loose,
            Some(other) => {
                return Err(format!(
                    "the variant `tool_schema` is `{other}`, and must be one of strict, loose"
                ));
            }
        };
        Ok(Variants {
            edit_format: one_of("edit_format", self.edit_format, EDIT_FORMATS, "replace")?,
            tool_schema,
            prompt_style: one_of("prompt_style", self.prompt_style, PROMPT_STYLES, "concise")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A catalog with one model, to which `extra` adds or replaces keys.
    fn with(extra: &str) -> Result<Catalog, CatalogError> {
        let mut model = serde_json::json!({
            "provider": "p", "id": "m", "name": "M", "protocol": "test",
            "context_window": 200_000, "max_output": 8_000,
        });
        let extra: serde_json::Value = serde_json::from_str(extra).unwrap();
        for (key, value) in extra.as_object().unwrap() {
            model[key] = value.clone();
        }
        let file = serde_json::json!({"version": "t", "model": [model]});
        Catalog::from_json(&file.to_string())
    }

    fn entry(extra: &str) -> ModelEntry {
        let catalog = with(extra).unwrap_or_else(|error| panic!("{error}"));
        ModelEntry::clone(&catalog.models()[0])
    }

    fn problem(extra: &str) -> String {
        match with(extra) {
            Ok(_) => panic!("accepted: {extra}"),
            Err(error) => error.message,
        }
    }

    #[test]
    fn the_built_in_catalog_loads() {
        let catalog = Catalog::builtin().unwrap_or_else(|error| panic!("{error}"));
        assert!(!catalog.version().is_empty());
        assert!(!catalog.models().is_empty());
        // The same catalog every time, read once.
        assert!(std::ptr::eq(catalog, Catalog::builtin().unwrap()));
        for model in catalog.models() {
            let info = &model.info;
            let name = format!("{}/{}", info.provider, info.id);
            assert!(
                model.source.is_some(),
                "{name}: every built-in entry says where it was read"
            );
            assert!(info.prices.is_some(), "{name}: has prices");
            assert!(
                EDIT_FORMATS.contains(&model.variants.edit_format.as_str()),
                "{name}"
            );
            assert!(
                PROMPT_STYLES.contains(&model.variants.prompt_style.as_str()),
                "{name}"
            );
            for offer in &info.speeds {
                assert!(info.offers(&offer.speed), "{name}");
                assert!(
                    offer.prices.is_some(),
                    "{name}: the tier `{}` has its own rate card",
                    offer.speed.as_str()
                );
            }
        }
    }

    #[test]
    fn the_built_in_fast_tiers_are_described_in_full() {
        let catalog = Catalog::builtin().unwrap();
        let fast: Vec<_> = catalog
            .models()
            .iter()
            .filter_map(|model| Some((model, model.info.speed_offer(&Speed::Fast)?)))
            .collect();
        assert!(!fast.is_empty(), "the catalog has a model with a fast tier");
        for (model, offer) in fast {
            let name = model.info.id.as_str();
            let standard = model.info.prices.as_ref().unwrap();
            let prices = offer.prices.as_ref().unwrap();
            assert_eq!(offer.effect, SpeedEffect::FasterOutput, "{name}");
            assert_eq!(offer.when_unavailable, SpeedUnavailable::Error, "{name}");
            assert!(offer.separate_limit, "{name}");
            assert_eq!(offer.shares_cache, Some(false), "{name}");
            // A full card: every class standard prices, the tier prices too.
            assert!(prices.input > standard.input, "{name}");
            assert!(prices.output > standard.output, "{name}");
            assert_eq!(
                prices.cache_read.is_some(),
                standard.cache_read.is_some(),
                "{name}"
            );
            assert_eq!(
                prices.cache_write.len(),
                standard.cache_write.len(),
                "{name}"
            );
        }
    }

    #[test]
    fn a_minimal_entry_gets_the_defaults() {
        let entry = entry("{}");
        assert_eq!(entry.info.id.as_str(), "m");
        assert_eq!(entry.info.provider.as_str(), "p");
        assert_eq!(entry.info.display_name, "M");
        assert_eq!(
            entry.info.prices, None,
            "no prices is `not known`, not zero"
        );
        assert_eq!(entry.info.reasoning.kind, ReasoningControlKind::None);
        assert!(entry.info.speeds.is_empty());
        assert!(entry.info.input.is_empty());
        assert_eq!(
            entry.variants,
            Variants {
                edit_format: "replace".to_owned(),
                tool_schema: ToolSchemaStyle::Loose,
                prompt_style: "concise".to_owned(),
            }
        );
        assert!(entry.flags.is_empty());
        assert_eq!(entry.source, None);
    }

    #[test]
    fn prices_are_dollars_per_million_and_become_whole_nano_dollars() {
        let entry = entry(
            r#"{"prices": {
                "input": 0.1, "output": 2.5, "cache_read": 0.01,
                "cache_write": [{"class": "5m", "price": 0.125}, {"class": "1h", "price": 0.2}],
                "above": [{"input_tokens": 100000, "input": 0.5, "output": 2.5,
                           "cache_read": 0.05,
                           "cache_write": [{"class": "5m", "price": 0.625}]}]
            }}"#,
        );
        let prices = entry.info.prices.unwrap();
        assert_eq!(prices.input, 100_000_000);
        assert_eq!(prices.output, 2_500_000_000);
        assert_eq!(prices.cache_read, Some(10_000_000));
        assert_eq!(
            prices.cache_write,
            [
                CacheWritePrice {
                    class: "5m".to_owned(),
                    price: 125_000_000
                },
                CacheWritePrice {
                    class: "1h".to_owned(),
                    price: 200_000_000
                },
            ],
            "in the order written: the first is the default lifetime"
        );
        assert_eq!(prices.tiers.len(), 1);
        assert_eq!(prices.tiers[0].above_input_tokens, 100_000);
        assert_eq!(prices.tiers[0].input, 500_000_000);
        assert_eq!(prices.tiers[0].cache_write[0].price, 625_000_000);
    }

    #[test]
    fn prices_that_cannot_be_amounts_are_refused() {
        for prices in [
            r#"{"input": -1, "output": 1}"#,
            r#"{"input": 1e30, "output": 1}"#,
            r#"{"input": 0.00000000001, "output": 1}"#,
            r#"{"input": 1, "output": 1, "cache_read": -0.5}"#,
            r#"{"input": 1, "output": 1, "cache_write": [{"class": "", "price": 1}]}"#,
            r#"{"input": 1, "output": 1,
                "cache_write": [{"class": "5m", "price": 1}, {"class": "5m", "price": 2}]}"#,
            r#"{"input": 1, "output": 1, "above": [{"input_tokens": 0, "input": 1, "output": 1}]}"#,
            r#"{"input": 1, "output": 1, "above": [
                {"input_tokens": 200, "input": 1, "output": 1},
                {"input_tokens": 100, "input": 1, "output": 1}]}"#,
        ] {
            problem(&format!(r#"{{"prices": {prices}}}"#));
        }
        assert!(problem(r#"{"prices": {"input": -1, "output": 1}}"#).contains("model p/m"));
    }

    #[test]
    fn a_speed_tier_carries_everything_that_tells_it_apart() {
        let entry = entry(
            r#"{"prices": {"input": 2, "output": 10},
                "speed": [
                  {"name": "fast", "label": "Fast mode", "effect": "faster_output",
                   "when_unavailable": "error", "separate_limit": true, "shares_cache": false,
                   "prices": {"input": 4, "output": 20, "cache_read": 0.2}},
                  {"name": "priority", "label": "Priority processing", "effect": "priority",
                   "when_unavailable": "downgrade", "preview": true},
                  {"name": "flex", "label": "Flex processing", "effect": "deferred",
                   "when_unavailable": "error", "shares_cache": true,
                   "prices": {"input": 1, "output": 5}}
                ]}"#,
        );
        let info = &entry.info;
        assert!(info.offers(&Speed::Standard));
        assert!(info.offers(&Speed::Fast));
        assert!(info.offers(&Speed::Other("flex".to_owned())));
        assert!(!info.offers(&Speed::Other("ultrafast".to_owned())));

        let fast = info.speed_offer(&Speed::Fast).unwrap();
        assert_eq!(fast.label, "Fast mode");
        assert_eq!(fast.effect, SpeedEffect::FasterOutput);
        assert_eq!(fast.when_unavailable, SpeedUnavailable::Error);
        assert!(fast.separate_limit);
        assert_eq!(fast.shares_cache, Some(false));
        assert!(!fast.preview);
        let prices = fast.prices.as_ref().unwrap();
        assert_eq!(
            (prices.input, prices.output),
            (4_000_000_000, 20_000_000_000)
        );
        assert_eq!(prices.cache_read, Some(200_000_000));

        let priority = info
            .speed_offer(&Speed::Other("priority".to_owned()))
            .unwrap();
        assert_eq!(priority.effect, SpeedEffect::Priority);
        assert_eq!(priority.when_unavailable, SpeedUnavailable::Downgrade);
        assert!(!priority.separate_limit);
        assert_eq!(priority.shares_cache, None, "not stated stays not stated");
        assert!(priority.preview);
        assert_eq!(priority.prices, None, "an unknown price stays unknown");

        let flex = info.speed_offer(&Speed::Other("flex".to_owned())).unwrap();
        assert_eq!(flex.effect, SpeedEffect::Deferred);
        assert_eq!(flex.shares_cache, Some(true));
    }

    #[test]
    fn a_speed_tier_that_is_not_fully_said_is_refused() {
        let base = r#""label": "L", "effect": "priority", "when_unavailable": "error""#;
        for speed in [
            format!(r#"{{"name": "standard", {base}}}"#),
            format!(r#"{{"name": "", {base}}}"#),
            r#"{"name": "fast", "label": "", "effect": "priority", "when_unavailable": "error"}"#
                .to_owned(),
            r#"{"name": "fast", "label": "L", "effect": "quicker", "when_unavailable": "error"}"#
                .to_owned(),
            r#"{"name": "fast", "label": "L", "effect": "priority", "when_unavailable": "shrug"}"#
                .to_owned(),
            r#"{"name": "fast", "label": "L", "effect": "priority"}"#.to_owned(),
            r#"{"name": "fast", "label": "L", "when_unavailable": "error"}"#.to_owned(),
            format!(r#"{{"name": "fast", {base}, "prices": {{"input": -2, "output": 1}}}}"#),
            format!(r#"{{"name": "fast", {base}, "share_cache": true}}"#),
        ] {
            problem(&format!(r#"{{"speed": [{speed}]}}"#));
        }
        let twice =
            format!(r#"{{"speed": [{{"name": "fast", {base}}}, {{"name": "fast", {base}}}]}}"#);
        assert!(problem(&twice).contains("listed twice"));
        let unknown = problem(
            r#"{"speed": [{"name": "fast", "label": "L", "effect": "quicker",
                           "when_unavailable": "error"}]}"#,
        );
        assert!(
            unknown.contains("faster_output"),
            "the message lists what is allowed: {unknown}"
        );
    }

    #[test]
    fn reasoning_is_levels_a_budget_or_nothing() {
        let levels = entry(r#"{"reasoning": {"levels": ["low", "high"], "default": "high"}}"#);
        assert_eq!(levels.info.reasoning.kind, ReasoningControlKind::Levels);
        assert_eq!(levels.info.reasoning.levels, ["low", "high"]);
        assert_eq!(levels.info.reasoning.default.as_deref(), Some("high"));

        let budget = entry(r#"{"reasoning": {"min_tokens": 1024, "max_tokens": 32000}}"#);
        assert_eq!(budget.info.reasoning.kind, ReasoningControlKind::Budget);
        assert_eq!(budget.info.reasoning.min_tokens, Some(1024));
        assert_eq!(budget.info.reasoning.max_tokens, Some(32000));

        assert_eq!(
            entry(r#"{"reasoning": {}}"#).info.reasoning.kind,
            ReasoningControlKind::None
        );

        for reasoning in [
            r#"{"levels": ["low", "high"], "default": "medium"}"#,
            r#"{"levels": ["low", "low"]}"#,
            r#"{"levels": [""]}"#,
            r#"{"levels": ["low"], "max_tokens": 5}"#,
            r#"{"default": "low"}"#,
            r#"{"min_tokens": 5}"#,
            r#"{"min_tokens": 9, "max_tokens": 5}"#,
        ] {
            problem(&format!(r#"{{"reasoning": {reasoning}}}"#));
        }
    }

    #[test]
    fn variants_default_from_the_models_capabilities_and_are_checked() {
        let strict =
            entry(r#"{"capabilities": {"strict_tools": true, "min_cacheable_tokens": 512}}"#);
        assert_eq!(strict.variants.tool_schema, ToolSchemaStyle::Strict);
        assert_eq!(strict.info.capabilities.min_cacheable_tokens, Some(512));
        assert!(strict.info.capabilities.strict_tools);
        assert!(!strict.info.capabilities.structured_output);

        let chosen = entry(
            r#"{"capabilities": {"strict_tools": true},
                "variants": {"tool_schema": "loose", "prompt_style": "guided"}}"#,
        );
        assert_eq!(chosen.variants.tool_schema, ToolSchemaStyle::Loose);
        assert_eq!(chosen.variants.prompt_style, "guided");
        assert_eq!(chosen.variants.edit_format, "replace");
        assert_eq!(ToolSchemaStyle::Strict.as_str(), "strict");
        assert_eq!(ToolSchemaStyle::Loose.as_str(), "loose");

        for variants in [
            r#"{"edit_format": "patch"}"#,
            r#"{"prompt_style": "chatty"}"#,
            r#"{"tool_schema": "strict"}"#,
            r#"{"tool_schema": "relaxed"}"#,
        ] {
            problem(&format!(r#"{{"variants": {variants}}}"#));
        }
    }

    #[test]
    fn the_rest_of_an_entry_is_checked_too() {
        for extra in [
            r#"{"id": " "}"#,
            r#"{"name": ""}"#,
            r#"{"protocol": ""}"#,
            r#"{"context_window": 0}"#,
            r#"{"max_output": 0}"#,
            r#"{"max_output": 300000}"#,
            r#"{"input": ["text", "smell"]}"#,
            r#"{"input": ["text", "text"]}"#,
            r#"{"reads_reasoning_from": ["m"]}"#,
            r#"{"contxt_window": 5}"#,
            r#"{"prices": {"input": 1, "output": 1, "cache_raed": 0.1}}"#,
            r#"{"capabilities": {"strict_tool": true}}"#,
        ] {
            problem(extra);
        }
        let entry = entry(
            r#"{"input": ["text", "image"], "reads_reasoning_from": ["older"],
                "flags": {"beta_header": "x-1", "usage_in_stream": false},
                "source": "the provider's documentation, read 2026-11-12"}"#,
        );
        assert_eq!(entry.info.input, [Modality::Text, Modality::Image]);
        assert_eq!(entry.info.reads_reasoning_from, [ModelId::new("older")]);
        assert_eq!(entry.flags.text("beta_header"), Some("x-1"));
        assert_eq!(entry.flags.bool("usage_in_stream"), Some(false));
        assert!(entry.source.unwrap().contains("2026-11-12"));
    }

    #[test]
    fn a_catalog_needs_a_version_and_lists_no_model_twice() {
        let no_version = Catalog::from_json(r#"{"model": []}"#).unwrap_err();
        assert!(no_version.message.contains("version"), "{no_version}");
        assert!(Catalog::from_json(r#"{"version": " ", "model": []}"#).is_err());
        assert!(Catalog::from_json("not json").is_err());
        assert!(Catalog::from_json(r#"{"version": "1"} and more"#).is_err());
        assert!(Catalog::from_json(r#"{"version": "1", "models": []}"#).is_err());
        assert_eq!(no_version.kind(), ErrorKind::Config);
        assert_eq!(no_version.code(), "invalid_catalog");

        let empty = Catalog::from_json(r#"{"version": "1"}"#).unwrap();
        assert!(empty.models().is_empty());

        let model = r#"{"provider": "p", "id": "m", "name": "M", "protocol": "t",
                        "context_window": 10, "max_output": 5}"#;
        let twice = Catalog::from_json(&format!(
            r#"{{"version": "1", "model": [{model}, {model}]}}"#
        ));
        assert!(twice.unwrap_err().message.contains("listed twice"));
    }

    #[test]
    fn an_overlay_adds_models_and_replaces_whole_entries() {
        let model = |provider: &str, id: &str, name: &str, extra: &str| {
            format!(
                r#"{{"provider": "{provider}", "id": "{id}", "name": "{name}", "protocol": "t",
                    "context_window": 100, "max_output": 10{extra}}}"#
            )
        };
        let mut catalog = Catalog::from_json(&format!(
            r#"{{"version": "base", "model": [{}, {}]}}"#,
            model(
                "p",
                "one",
                "One",
                r#", "prices": {"input": 1, "output": 2}"#
            ),
            model("p", "two", "Two", ""),
        ))
        .unwrap();
        let user = Catalog::from_json(&format!(
            r#"{{"version": "user", "model": [{}, {}]}}"#,
            model("p", "one", "One, mine", ""),
            model("local", "llama", "A local model", ""),
        ))
        .unwrap();

        catalog.overlay(&user);
        assert_eq!(catalog.version(), "base+user");
        let names: Vec<&str> = catalog
            .models()
            .iter()
            .map(|entry| entry.info.display_name.as_str())
            .collect();
        assert_eq!(names, ["One, mine", "Two", "A local model"]);
        let one = catalog
            .get(&ProviderId::new("p"), &ModelId::new("one"))
            .unwrap();
        assert_eq!(
            one.info.prices, None,
            "the entry is replaced whole, not merged"
        );
        assert!(
            catalog
                .get(&ProviderId::new("local"), &ModelId::new("llama"))
                .is_some()
        );
        assert!(
            catalog
                .get(&ProviderId::new("local"), &ModelId::new("one"))
                .is_none()
        );
    }
}
