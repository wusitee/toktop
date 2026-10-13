//! Codex rollouts: `~/.codex/{sessions,archived_sessions}/**/rollout-*.jsonl`.
//!
//! `event_msg` rows of type `token_count` carry both the cumulative
//! `total_token_usage` and the per-request `last_token_usage`. Each request is
//! usually reported twice with an unchanged total, so a row is counted only
//! when the total moves. Counting `last_token_usage` rather than the total's
//! delta keeps forked threads, whose totals start from the parent's, correct.
//!
//! Codex buckets overlap: `cached_input_tokens` and `cache_write_input_tokens`
//! are subsets of `input_tokens`, and `reasoning_output_tokens` is a subset of
//! `output_tokens`. They are split into disjoint [`Tokens`] buckets here.
//!
//! A forked thread (a subagent, or a user fork) may start by replaying the
//! parent's turns, token counts included. UUIDv7 turn and session ids carry a
//! creation time, so older turns are replayed history. Legacy turns require a
//! child-local `task_started` announcement. Replayed counts set the baseline
//! without emitting usage; known inherited snapshots cannot reset it later.
//!
//! Events are keyed by session id, timestamp and cumulative total, so a
//! rollout present in both `sessions/` and `archived_sessions/` counts once.

use super::{clamp, contains, parse_ts};
use crate::model::{Event, LimitWindow, RateLimits, ServiceTier, Session, Tokens};
use serde::{Deserialize, Deserializer};
use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct State {
    model: Option<String>,
    /// Thread snapshots persist across turns; request metadata overrides only this turn.
    thread_tier: Option<ServiceTier>,
    request_tier: Option<ServiceTier>,
    /// Cumulative usage of the last accepted snapshot.
    last_total: Option<Usage>,
    /// Set after the first `session_meta`; later ones are replayed parent history.
    have_meta: bool,
    /// This fork's own session id, while its replayed parent history is skipped.
    replaying_before: Option<String>,
    /// Child-local turns announced by `task_started` while the gate is closed.
    task_started_turn_ids: HashSet<String>,
    /// Cumulative totals seen in parent replay, which may be re-reported later.
    inherited_totals: HashSet<u64>,
}

#[derive(Deserialize)]
struct Line {
    timestamp: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    payload: Option<Payload>,
}

#[derive(Deserialize)]
struct Payload {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    forked_from_id: Option<String>,
    parent_thread_id: Option<String>,
    source: Option<serde_json::Value>,
    turn_id: Option<String>,
    started_at: Option<serde_json::Value>,
    cwd: Option<String>,
    model: Option<String>,
    info: Option<Info>,
    rate_limits: Option<Limits>,
    #[serde(default, deserialize_with = "service_tier")]
    service_tier: Option<ServiceTier>,
    thread_settings: Option<TurnSettings>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct TurnSettings {
    #[serde(default, deserialize_with = "service_tier")]
    service_tier: Option<ServiceTier>,
}

impl Payload {
    fn request_tier(&self) -> Option<ServiceTier> {
        self.service_tier
            .or_else(|| {
                self.info.as_ref().and_then(|info| {
                    info.service_tier
                        .or_else(|| info.last_token_usage.and_then(|usage| usage.service_tier))
                        .or_else(|| info.total_token_usage.and_then(|usage| usage.service_tier))
                })
            })
            .or_else(|| self.usage.and_then(|usage| usage.service_tier))
    }
}

#[derive(Deserialize)]
struct Info {
    #[serde(default, deserialize_with = "service_tier")]
    service_tier: Option<ServiceTier>,
    total_token_usage: Option<Usage>,
    last_token_usage: Option<Usage>,
    model: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Copy)]
struct Usage {
    #[serde(default, deserialize_with = "service_tier")]
    service_tier: Option<ServiceTier>,
    input_tokens: Option<i64>,
    cached_input_tokens: Option<i64>,
    cache_write_input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    reasoning_output_tokens: Option<i64>,
    total_tokens: Option<i64>,
}

impl Usage {
    fn total(&self) -> u64 {
        match self.total_tokens {
            Some(total) => total.max(0) as u64,
            None => clamp(self.input_tokens).saturating_add(clamp(self.output_tokens)),
        }
    }

    /// Component-wise `self - previous`, or `None` if any component shrank.
    fn delta(&self, previous: &Usage) -> Option<Usage> {
        let sub = |now: Option<i64>, before: Option<i64>| -> Option<Option<i64>> {
            let (now, before) = (now.unwrap_or(0).max(0), before.unwrap_or(0).max(0));
            (now >= before).then_some(Some(now - before))
        };
        Some(Usage {
            service_tier: None,
            input_tokens: sub(self.input_tokens, previous.input_tokens)?,
            cached_input_tokens: sub(self.cached_input_tokens, previous.cached_input_tokens)?,
            cache_write_input_tokens: sub(
                self.cache_write_input_tokens,
                previous.cache_write_input_tokens,
            )?,
            output_tokens: sub(self.output_tokens, previous.output_tokens)?,
            reasoning_output_tokens: sub(
                self.reasoning_output_tokens,
                previous.reasoning_output_tokens,
            )?,
            total_tokens: None,
        })
    }

    /// Advance the cumulative baseline after a last-only fallback row.
    fn saturating_add(&self, last: &Usage) -> Usage {
        let add = |before: Option<i64>, last: Option<i64>| {
            Some(
                before
                    .unwrap_or(0)
                    .max(0)
                    .saturating_add(last.unwrap_or(0).max(0)),
            )
        };
        Usage {
            service_tier: None,
            input_tokens: add(self.input_tokens, last.input_tokens),
            cached_input_tokens: add(self.cached_input_tokens, last.cached_input_tokens),
            cache_write_input_tokens: add(
                self.cache_write_input_tokens,
                last.cache_write_input_tokens,
            ),
            output_tokens: add(self.output_tokens, last.output_tokens),
            reasoning_output_tokens: add(
                self.reasoning_output_tokens,
                last.reasoning_output_tokens,
            ),
            total_tokens: Some(
                self.total()
                    .saturating_add(last.total())
                    .min(i64::MAX as u64) as i64,
            ),
        }
    }

    fn tokens(&self) -> Tokens {
        let input = clamp(self.input_tokens);
        let cache_read = clamp(self.cached_input_tokens).min(input);
        let cache_write = clamp(self.cache_write_input_tokens).min(input - cache_read);
        let output = clamp(self.output_tokens);
        let reasoning = clamp(self.reasoning_output_tokens).min(output);
        Tokens {
            input: input - cache_read - cache_write,
            output: output - reasoning,
            cache_read,
            cache_write,
            cache_write_1h: 0,
            reasoning,
        }
    }
}

#[derive(Deserialize)]
struct Limits {
    limit_id: Option<String>,
    plan_type: Option<String>,
    primary: Option<Window>,
    secondary: Option<Window>,
}

#[derive(Deserialize)]
struct Window {
    used_percent: Option<f64>,
    window_minutes: Option<u64>,
    resets_at: Option<i64>,
}

impl Window {
    fn into_model(self) -> Option<LimitWindow> {
        Some(LimitWindow {
            used_percent: self.used_percent?,
            window_minutes: self.window_minutes.unwrap_or(0),
            resets_at: self.resets_at,
        })
    }
}

impl State {
    pub fn line(&mut self, session: &mut Session, raw: &[u8]) {
        let token_count = contains(raw, b"\"token_count\"");
        let turn_context = !token_count && contains(raw, b"\"turn_context\"");
        let meta = !token_count && !self.have_meta && contains(raw, b"\"session_meta\"");
        let task_started = self.replaying_before.is_some() && contains(raw, b"\"task_started\"");
        let tier_record = contains(raw, b"\"thread_settings_applied\"")
            || contains(raw, b"\"token_usage_record\"")
            || contains(raw, b"\"service_tier\"");
        if !token_count && !turn_context && !meta && !task_started && !tier_record {
            return;
        }
        let Ok(line) = serde_json::from_slice::<Line>(raw) else {
            return;
        };
        let Some(payload) = line.payload else { return };

        if !matches!(line.kind.as_deref(), Some("turn_context" | "session_meta"))
            && self.replaying_before.is_none()
        {
            if let Some(tier) = payload.request_tier() {
                self.request_tier = Some(tier);
            }
        }

        match line.kind.as_deref() {
            Some("session_meta") if !self.have_meta => {
                self.have_meta = true;
                let forked = payload.forked_from_id.is_some()
                    || payload.parent_thread_id.is_some()
                    || payload.source.as_ref().is_some_and(|source| {
                        source
                            .pointer("/subagent/thread_spawn/parent_thread_id")
                            .is_some()
                    });
                if forked {
                    self.replaying_before = payload.id.clone();
                }
                if let Some(id) = payload.id {
                    session.id = id;
                }
                session.project = payload.cwd;
            }
            Some("turn_context") => {
                if let (Some(own), Some(turn)) = (&self.replaying_before, &payload.turn_id) {
                    if uuid_ms(own)
                        .zip(uuid_ms(turn))
                        .is_some_and(|(own, turn)| turn >= own)
                        || self.task_started_turn_ids.contains(turn)
                    {
                        self.replaying_before = None;
                        self.task_started_turn_ids.clear();
                    }
                }
                if self.replaying_before.is_none() {
                    self.request_tier = payload.request_tier();
                }
                if payload.model.is_some() {
                    self.model = payload.model;
                }
            }
            Some("event_msg")
                if payload.kind.as_deref() == Some("thread_settings_applied")
                    && self.replaying_before.is_none() =>
            {
                if let Some(settings) = payload.thread_settings {
                    self.thread_tier = settings.service_tier;
                } else if let Some(tier) = payload.request_tier() {
                    self.thread_tier = Some(tier);
                }
            }
            Some("event_msg") if payload.kind.as_deref() == Some("task_started") => {
                if let (Some(own), Some(turn)) = (&self.replaying_before, payload.turn_id) {
                    let child_local = match (uuid_ms(own), uuid_ms(&turn)) {
                        (Some(own), Some(turn)) => turn >= own,
                        (Some(own), None) => payload
                            .started_at
                            .as_ref()
                            .and_then(serde_json::Value::as_u64)
                            .is_some_and(|started| started >= own / 1000),
                        (None, _) => true,
                    };
                    if child_local {
                        self.task_started_turn_ids.insert(turn);
                    }
                }
            }
            Some("event_msg") if payload.kind.as_deref() == Some("token_count") => {
                let Some(ts) = line.timestamp.as_deref().and_then(parse_ts) else {
                    return;
                };
                let replay = self.replaying_before.is_some();
                if let Some(limits) = payload.rate_limits.filter(|_| !replay) {
                    record_limits(session, ts, limits);
                }
                if let Some(info) = payload.info {
                    self.token_count(session, ts, info, replay);
                }
            }
            _ => {}
        }
    }

    fn token_count(&mut self, session: &mut Session, ts: i64, info: Info, replay: bool) {
        let total = info.total_token_usage;
        if replay {
            if let Some(total) = total {
                self.inherited_totals.insert(total.total());
                self.last_total = Some(total);
            }
            return;
        }
        if total.is_some_and(|total| {
            self.inherited_totals.contains(&total.total())
                && info
                    .last_token_usage
                    .is_none_or(|last| total.total() != last.total())
        }) {
            return;
        }
        let request = match (total, info.last_token_usage, self.last_total) {
            (Some(total), Some(last), Some(previous)) => {
                let (now, before, last_total) = (total.total(), previous.total(), last.total());
                if now == before {
                    return;
                }
                // A snapshot that lands slightly out of order regresses the
                // total by about one request; counting it would double-count.
                // A larger drop, or a total equal to this request alone (the
                // counter restarted after compaction), is a reset and is counted.
                if now < before
                    && now != last_total
                    && (u128::from(now) * 100 >= u128::from(before) * 98
                        || now.saturating_add(last_total.saturating_mul(2)) >= before)
                {
                    return;
                }
                if now < before {
                    self.inherited_totals.clear();
                }
                last
            }
            (None, Some(last), Some(previous)) => {
                self.last_total = Some(previous.saturating_add(&last));
                last
            }
            (_, Some(last), _) => last,
            // Degraded rows without per-request usage: diff the totals.
            (Some(total), None, Some(previous)) => {
                if total.total() == previous.total() {
                    return;
                }
                match total.delta(&previous) {
                    Some(delta) => delta,
                    None => {
                        self.last_total = Some(total);
                        self.inherited_totals.clear();
                        return;
                    }
                }
            }
            (Some(total), None, None) => total,
            (None, None, _) => return,
        };
        if let Some(total) = total {
            self.last_total = Some(total);
        }
        if info.model.is_some() {
            self.model = info.model;
        }
        let tokens = request.tokens();
        if tokens.is_empty() {
            return;
        }
        let model = session.model_index(self.model.as_deref().unwrap_or("unknown"));
        let key = total.map(|total| format!("{}:{ts}:{}", session.id, total.total()).into());
        session.events.push(Event {
            ts,
            model,
            tokens,
            tier: self.request_tier.or(self.thread_tier).unwrap_or_default(),
            key,
            input_is_split: false,
            cache_write_1h_raw: 0,
        });
    }
}

fn service_tier<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<ServiceTier>, D::Error> {
    let tier = Option::<String>::deserialize(deserializer)?;
    Ok(tier
        .as_deref()
        .map(str::trim)
        .filter(|tier| !tier.is_empty())
        .map(|tier| {
            if tier.eq_ignore_ascii_case("priority") || tier.eq_ignore_ascii_case("fast") {
                ServiceTier::Priority
            } else {
                ServiceTier::Standard
            }
        }))
}

/// Creation time of a UUIDv7; legacy random UUIDs cannot order turns.
fn uuid_ms(uuid: &str) -> Option<u64> {
    if !is_uuid(uuid) || uuid.as_bytes()[14] != b'7' {
        return None;
    }
    Some(
        (u64::from_str_radix(&uuid[..8], 16).ok()? << 16)
            | u64::from_str_radix(&uuid[9..13], 16).ok()?,
    )
}

/// Hyphenated UUID.
fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

fn record_limits(session: &mut Session, ts: i64, limits: Limits) {
    if limits.limit_id.as_deref().is_some_and(|id| id != "codex") {
        return;
    }
    if session
        .limits
        .as_ref()
        .is_some_and(|known| known.observed > ts)
    {
        return;
    }
    session.limits = Some(RateLimits {
        observed: ts,
        plan: limits.plan_type,
        primary: limits.primary.and_then(Window::into_model),
        secondary: limits.secondary.and_then(Window::into_model),
    });
}
