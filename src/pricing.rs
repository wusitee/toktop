//! Model prices from LiteLLM's public price table.
//!
//! The upstream table (~3 MB) is downloaded at most once a day and reduced to
//! the few fields toktop prices with before it is cached on disk.

use crate::model::{ServiceTier, Tokens};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

pub(crate) mod aliases;

pub const SOURCE_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

/// Cached prices are refreshed when older than this.
pub const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// USD per token. Missing cache rates fall back to the input rate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    pub cache_write: Option<f64>,
    pub cache_write_1h: Option<f64>,
    /// Explicit priority-processing rates; absent rates use a 2x standard premium.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_priority: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_priority: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_priority: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_priority_above_272k: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_priority_above_272k: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_priority_above_272k: Option<f64>,
    /// Long-context tier: rates applied when a request's prompt exceeds `threshold` tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<Tier>,
}

/// Cost in USD by token kind.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Cost {
    pub input: f64,
    pub cache_read: f64,
    /// 5-minute and 1-hour cache writes.
    pub cache_write: f64,
    /// Output and reasoning tokens.
    pub output: f64,
}

impl Cost {
    pub fn total(&self) -> f64 {
        self.input + self.cache_read + self.cache_write + self.output
    }
}

impl std::ops::AddAssign for Cost {
    fn add_assign(&mut self, other: Cost) {
        self.input += other.input;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.output += other.output;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Tier {
    pub threshold: u64,
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
    #[serde(default)]
    pub cache_write_1h: Option<f64>,
}

impl Price {
    /// Cost of one request.
    pub fn cost(&self, tokens: &Tokens, service_tier: ServiceTier) -> Cost {
        let prompt = tokens
            .input
            .saturating_add(tokens.cache_read)
            .saturating_add(tokens.cache_write);
        // Tier rates the table omits fall back to the base rate of that kind.
        let (input, output, cache_read, cache_write, cache_write_1h) = match self.tier {
            Some(tier) if prompt > tier.threshold => (
                tier.input,
                tier.output,
                tier.cache_read.or(self.cache_read),
                tier.cache_write.or(self.cache_write),
                tier.cache_write_1h.or(self.cache_write_1h),
            ),
            _ => (
                self.input,
                self.output,
                self.cache_read,
                self.cache_write,
                self.cache_write_1h,
            ),
        };
        let cache_read = cache_read.unwrap_or(input);
        let cache_write_5m = cache_write.unwrap_or(input);
        let cache_write_1h = cache_write_1h.unwrap_or(cache_write_5m);
        let (input, output, cache_read, cache_write_5m, cache_write_1h) =
            if service_tier == ServiceTier::Priority {
                let (priority_input, priority_output, priority_cache_read) =
                    if self.tier.is_some_and(|tier| prompt > tier.threshold) {
                        (
                            self.input_priority_above_272k,
                            self.output_priority_above_272k,
                            self.cache_read_priority_above_272k,
                        )
                    } else {
                        (
                            self.input_priority,
                            self.output_priority,
                            self.cache_read_priority,
                        )
                    };
                // tokscale doubles the complete standard cost, including cached
                // tokens and long-context rates, when no priority tariff is given.
                (
                    priority_input.unwrap_or(input * 2.0),
                    priority_output.unwrap_or(output * 2.0),
                    priority_cache_read.unwrap_or(cache_read * 2.0),
                    cache_write_5m * 2.0,
                    cache_write_1h * 2.0,
                )
            } else {
                (input, output, cache_read, cache_write_5m, cache_write_1h)
            };
        let long_writes = tokens.cache_write_1h.min(tokens.cache_write);
        Cost {
            input: tokens.input as f64 * input,
            cache_read: tokens.cache_read as f64 * cache_read,
            cache_write: (tokens.cache_write - long_writes) as f64 * cache_write_5m
                + long_writes as f64 * cache_write_1h,
            output: tokens.output.saturating_add(tokens.reasoning) as f64 * output,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PriceTable {
    prices: HashMap<String, Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    /// Bare model name (after the last `/`) -> full key of the preferred entry.
    #[serde(skip)]
    bare: HashMap<String, String>,
}

/// Provider prefixes preferred when a model is listed only under prefixed keys.
const FIRST_PARTY: [&str; 7] = [
    "openai/",
    "anthropic/",
    "gemini/",
    "xai/",
    "vertex_ai/",
    "deepseek/",
    "moonshot/",
];

impl PriceTable {
    pub fn from_prices(prices: HashMap<String, Price>) -> Self {
        let mut table = PriceTable {
            prices,
            fetched_at: None,
            bare: HashMap::new(),
        };
        table.index();
        table
    }

    fn index(&mut self) {
        let rank = |key: &str| -> usize {
            if !key.contains('/') {
                0
            } else if let Some(position) = FIRST_PARTY.iter().position(|p| key.starts_with(p)) {
                1 + position
            } else {
                // Shorter keys are less likely to be regional or reseller variants.
                100 + key.matches('/').count()
            }
        };
        let mut bare: HashMap<String, String> = HashMap::new();
        for key in self.prices.keys() {
            let name = key.rsplit('/').next().unwrap_or(key).to_owned();
            match bare.get(&name) {
                Some(current) if (rank(current), current.as_str()) <= (rank(key), key.as_str()) => {
                }
                _ => {
                    bare.insert(name, key.clone());
                }
            }
        }
        self.bare = bare;
    }

    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }

    /// Apply complete per-model tariffs in USD per million tokens.
    pub fn apply_overrides(
        &mut self,
        path: &Path,
    ) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Overrides {
            models: HashMap<String, Rates>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Rates {
            input: f64,
            output: f64,
            cache_read: Option<f64>,
            cache_write: Option<f64>,
            cache_write_1h: Option<f64>,
        }
        let overrides: Overrides = toml::from_str(&std::fs::read_to_string(path)?)?;
        let mut names = Vec::new();
        for (name, rates) in overrides.models {
            if name.trim().is_empty()
                || [
                    Some(rates.input),
                    Some(rates.output),
                    rates.cache_read,
                    rates.cache_write,
                    rates.cache_write_1h,
                ]
                .into_iter()
                .flatten()
                .any(|rate| !rate.is_finite() || rate < 0.0)
            {
                return Err(format!(
                    "invalid price for `{name}`: rates must be finite and nonnegative"
                )
                .into());
            }
            self.prices.insert(
                aliases::canonical_id(&name),
                Price {
                    input: rates.input / 1_000_000.0,
                    output: rates.output / 1_000_000.0,
                    cache_read: rates.cache_read.map(|rate| rate / 1_000_000.0),
                    cache_write: rates.cache_write.map(|rate| rate / 1_000_000.0),
                    cache_write_1h: rates.cache_write_1h.map(|rate| rate / 1_000_000.0),
                    ..Price::default()
                },
            );
            names.push(name);
        }
        self.index();
        names.sort();
        Ok(names)
    }

    fn exact(&self, name: &str) -> Option<&Price> {
        self.prices
            .get(name)
            .or_else(|| self.bare.get(name).and_then(|key| self.prices.get(key)))
    }

    /// Price for a model id as written by a tool. Tool ids often add effort or
    /// variant suffixes (`-thinking`, `-high`, `-n`) to the catalog name, so
    /// trailing `-segments` are dropped until a match is found.
    pub fn lookup(&self, model: &str) -> Option<&Price> {
        let mut name = aliases::canonical_id(model);
        // A provider-qualified id names its own price.
        if let Some(price) = self.prices.get(&name) {
            return Some(price);
        }
        if let Some(stripped) = name.rsplit('/').next() {
            name = aliases::canonical_id(stripped);
        }
        loop {
            let canonical = aliases::resolve_alias(&name).unwrap_or(&name);
            if let Some(price) = self.exact(canonical) {
                return Some(price);
            }
            // LiteLLM publishes Gemini Pro's catalog entry with `-preview`.
            if matches!(canonical, "gemini-3-pro" | "gemini-3.1-pro")
                && let Some(price) = self.exact(&format!("{canonical}-preview"))
            {
                return Some(price);
            }
            let cut = name.rfind('-')?;
            name.truncate(cut);
            let canonical = aliases::resolve_alias(&name).unwrap_or(&name);
            // Stop before reducing an id to its bare family name ("gpt", "claude-opus").
            if !canonical.contains('-') || !canonical.bytes().any(|b| b.is_ascii_digit()) {
                return None;
            }
        }
    }

    pub fn load(path: &Path) -> Option<(Self, SystemTime)> {
        let modified = std::fs::metadata(path).ok()?.modified().ok()?;
        let data = std::fs::read(path).ok()?;
        let mut table: PriceTable = serde_json::from_slice(&data).ok()?;
        table.index();
        Some((table, modified))
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        crate::store::write_atomic(path, &serde_json::to_vec(self)?)
    }

    /// Reduces LiteLLM's table to chat-model prices.
    pub fn from_litellm(json: &[u8]) -> serde_json::Result<Self> {
        let raw: HashMap<String, RawEntry> = serde_json::from_slice(json)?;
        let prices = raw
            .into_iter()
            .filter_map(|(key, entry)| {
                if entry
                    .mode
                    .as_deref()
                    .is_some_and(|mode| mode != "chat" && mode != "responses")
                {
                    return None;
                }
                let tier = |threshold: u64,
                            input: Option<f64>,
                            output: Option<f64>,
                            cache_read: Option<f64>,
                            cache_write: Option<f64>,
                            cache_write_1h: Option<f64>| {
                    Some(Tier {
                        threshold,
                        input: input?,
                        output: output?,
                        cache_read,
                        cache_write,
                        cache_write_1h,
                    })
                };
                let price = Price {
                    input: entry.input_cost_per_token?,
                    output: entry.output_cost_per_token?,
                    cache_read: entry.cache_read_input_token_cost,
                    cache_write: entry.cache_creation_input_token_cost,
                    cache_write_1h: entry.cache_creation_input_token_cost_above_1hr,
                    input_priority: entry.input_cost_per_token_priority,
                    output_priority: entry.output_cost_per_token_priority,
                    cache_read_priority: entry.cache_read_input_token_cost_priority,
                    input_priority_above_272k: entry
                        .input_cost_per_token_above_272k_tokens_priority,
                    output_priority_above_272k: entry
                        .output_cost_per_token_above_272k_tokens_priority,
                    cache_read_priority_above_272k: entry
                        .cache_read_input_token_cost_above_272k_tokens_priority,
                    tier: tier(
                        200_000,
                        entry.input_cost_per_token_above_200k_tokens,
                        entry.output_cost_per_token_above_200k_tokens,
                        entry.cache_read_input_token_cost_above_200k_tokens,
                        entry.cache_creation_input_token_cost_above_200k_tokens,
                        entry.cache_creation_input_token_cost_above_1hr_above_200k_tokens,
                    )
                    .or_else(|| {
                        tier(
                            272_000,
                            entry.input_cost_per_token_above_272k_tokens,
                            entry.output_cost_per_token_above_272k_tokens,
                            entry.cache_read_input_token_cost_above_272k_tokens,
                            entry.cache_creation_input_token_cost_above_272k_tokens,
                            None,
                        )
                    }),
                };
                Some((key.to_ascii_lowercase(), price))
            })
            .collect();
        Ok(Self::from_prices(prices))
    }

    /// Downloads and reduces the upstream table. Blocking.
    pub fn fetch() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into();
        let body = agent
            .get(SOURCE_URL)
            .call()?
            .into_body()
            .with_config()
            .limit(32 * 1024 * 1024)
            .read_to_vec()?;
        let mut table = Self::from_litellm(&body)?;
        table.fetched_at =
            Some(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        Ok(table)
    }
}

#[derive(Deserialize)]
struct RawEntry {
    #[serde(default, deserialize_with = "lenient_f64")]
    input_cost_per_token_priority: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    output_cost_per_token_priority: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    cache_read_input_token_cost_priority: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    input_cost_per_token_above_272k_tokens_priority: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    output_cost_per_token_above_272k_tokens_priority: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    cache_read_input_token_cost_above_272k_tokens_priority: Option<f64>,
    #[serde(default, deserialize_with = "lenient_str")]
    mode: Option<String>,
    #[serde(default, deserialize_with = "lenient_f64")]
    input_cost_per_token: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    output_cost_per_token: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    cache_read_input_token_cost: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    cache_creation_input_token_cost: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    cache_creation_input_token_cost_above_1hr: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    input_cost_per_token_above_200k_tokens: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    output_cost_per_token_above_200k_tokens: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    cache_read_input_token_cost_above_200k_tokens: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    cache_creation_input_token_cost_above_200k_tokens: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    cache_creation_input_token_cost_above_1hr_above_200k_tokens: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    input_cost_per_token_above_272k_tokens: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    output_cost_per_token_above_272k_tokens: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    cache_read_input_token_cost_above_272k_tokens: Option<f64>,
    #[serde(default, deserialize_with = "lenient_f64")]
    cache_creation_input_token_cost_above_272k_tokens: Option<f64>,
}

/// The table mixes numbers with strings and documentation placeholders;
/// anything that is not a number is treated as missing.
fn lenient_f64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    })
}

fn lenient_str<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(deserializer)? {
        serde_json::Value::String(text) => Some(text),
        _ => None,
    })
}
