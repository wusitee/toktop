//! Core data types shared by parsers, the store and aggregation.

use serde::{Deserialize, Serialize};
use std::ops::AddAssign;

/// An AI coding tool whose local logs toktop reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Tool {
    ClaudeCode,
    Codex,
    Antigravity,
}

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::ClaudeCode, Tool::Codex, Tool::Antigravity];

    pub fn label(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "Claude Code",
            Tool::Codex => "Codex",
            Tool::Antigravity => "Antigravity",
        }
    }
}

/// Disjoint token buckets. `total()` is their plain sum.
///
/// `cache_write_1h` is the exception: it is the subset of `cache_write` that
/// was written with a one-hour TTL (Claude), priced at its own rate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    /// Uncached input tokens.
    pub input: u64,
    /// Output tokens, excluding reasoning.
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write_1h: u64,
    /// Reasoning / thinking tokens, disjoint from `output`.
    pub reasoning: u64,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
            .saturating_add(self.reasoning)
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// Per-field maximum, used to merge streaming duplicates of one message.
    pub fn max_merge(&mut self, other: &Tokens) {
        self.input = self.input.max(other.input);
        self.output = self.output.max(other.output);
        self.cache_read = self.cache_read.max(other.cache_read);
        self.cache_write = self.cache_write.max(other.cache_write);
        self.cache_write_1h = self.cache_write_1h.max(other.cache_write_1h);
        self.reasoning = self.reasoning.max(other.reasoning);
    }

    /// Share of all prompt tokens that were served from cache.
    pub fn cache_hit_rate(&self) -> Option<f64> {
        let prompt = self
            .input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write);
        (prompt > 0).then(|| self.cache_read as f64 / prompt as f64)
    }
}

impl AddAssign<&Tokens> for Tokens {
    fn add_assign(&mut self, rhs: &Tokens) {
        self.input = self.input.saturating_add(rhs.input);
        self.output = self.output.saturating_add(rhs.output);
        self.cache_read = self.cache_read.saturating_add(rhs.cache_read);
        self.cache_write = self.cache_write.saturating_add(rhs.cache_write);
        self.cache_write_1h = self.cache_write_1h.saturating_add(rhs.cache_write_1h);
        self.reasoning = self.reasoning.saturating_add(rhs.reasoning);
    }
}

/// Processing tier recorded by Codex. Other tools use standard pricing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceTier {
    #[default]
    Standard,
    Priority,
}

impl ServiceTier {
    fn is_standard(&self) -> bool {
        *self == Self::Standard
    }
}

/// One model response with its token usage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Unix epoch milliseconds.
    pub ts: i64,
    /// Index into the owning [`Source::models`] list.
    pub model: u32,
    pub tokens: Tokens,
    #[serde(default, skip_serializing_if = "ServiceTier::is_standard")]
    pub tier: ServiceTier,
    /// Stable message identity, used to drop copies of one response that
    /// appear in several files (resumed or forked sessions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Box<str>>,
    /// Whether input came from a Claude copy reporting both input and cache fields.
    #[serde(default, skip_serializing_if = "is_false")]
    pub input_is_split: bool,
    /// Raw Claude 1-hour cache-write maximum, before clamping to the total.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write_1h_raw: u64,
}

impl Event {
    /// Merge copies of one response, preserving Claude's input/cache split.
    pub fn max_merge(&mut self, other: &Event) {
        let input = self.tokens.input;
        self.tokens.max_merge(&other.tokens);
        self.tokens.input = match (self.input_is_split, other.input_is_split) {
            (false, true) => other.tokens.input,
            (true, false) => input,
            _ => self.tokens.input,
        };
        self.input_is_split |= other.input_is_split;
        self.cache_write_1h_raw = self
            .cache_write_1h_raw
            .max(other.cache_write_1h_raw)
            .max(self.tokens.cache_write_1h);
        self.tokens.cache_write_1h = self.cache_write_1h_raw.min(self.tokens.cache_write);
    }
}

/// Everything parsed from one source file: a single session's usage.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// Working directory the session ran in.
    pub project: Option<String>,
    pub title: Option<String>,
    pub models: Vec<String>,
    pub events: Vec<Event>,
    /// Latest Codex rate-limit snapshot observed in this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<RateLimits>,
}

impl Session {
    /// Index of `model` in `models`, inserting it when new.
    pub fn model_index(&mut self, model: &str) -> u32 {
        if let Some(index) = self.models.iter().position(|known| known == model) {
            return index as u32;
        }
        self.models.push(model.to_owned());
        (self.models.len() - 1) as u32
    }

    pub fn last_activity(&self) -> Option<i64> {
        self.events.iter().map(|event| event.ts).max()
    }
}

/// A Codex rate-limit snapshot, as reported in its session logs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateLimits {
    /// When the snapshot was written (epoch ms).
    pub observed: i64,
    pub plan: Option<String>,
    pub primary: Option<LimitWindow>,
    pub secondary: Option<LimitWindow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LimitWindow {
    pub used_percent: f64,
    pub window_minutes: u64,
    /// Epoch seconds when the window resets.
    pub resets_at: Option<i64>,
}
