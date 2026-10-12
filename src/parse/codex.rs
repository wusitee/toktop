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
//! A forked thread (`session_meta.forked_from_id`, e.g. a subagent) may start
//! by replaying the parent's turns, token counts included. Turn and session
//! ids are UUIDv7, which sort by creation time, so a turn older than the
//! fork's own id is replayed parent history and is skipped.

use super::{clamp, contains, parse_ts};
use crate::model::{Event, LimitWindow, RateLimits, Session, Tokens};
use serde::Deserialize;

#[derive(Debug, Default)]
pub struct State {
    model: Option<String>,
    /// `total_tokens` of the last counted snapshot.
    last_total: Option<u64>,
    /// Set after the first `session_meta`; later ones are replayed parent history.
    have_meta: bool,
    /// This fork's own session id, while its replayed parent history is skipped.
    replaying_before: Option<String>,
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
    turn_id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    info: Option<Info>,
    rate_limits: Option<Limits>,
}

#[derive(Deserialize)]
struct Info {
    total_token_usage: Option<Usage>,
    last_token_usage: Option<Usage>,
    model: Option<String>,
}

#[derive(Deserialize, Clone, Copy)]
struct Usage {
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
            None => clamp(self.input_tokens) + clamp(self.output_tokens),
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
        if !token_count && !turn_context && !meta {
            return;
        }
        let Ok(line) = serde_json::from_slice::<Line>(raw) else {
            return;
        };
        let Some(payload) = line.payload else { return };

        match line.kind.as_deref() {
            Some("session_meta") if !self.have_meta => {
                self.have_meta = true;
                if payload.forked_from_id.is_some() {
                    self.replaying_before = payload.id.clone().filter(|id| is_uuid(id));
                }
                if let Some(id) = payload.id {
                    session.id = id;
                }
                session.project = payload.cwd;
            }
            Some("turn_context") => {
                if let (Some(own), Some(turn)) = (&self.replaying_before, &payload.turn_id) {
                    if is_uuid(turn) && turn.as_str() >= own.as_str() {
                        self.replaying_before = None;
                    }
                }
                if payload.model.is_some() {
                    self.model = payload.model;
                }
            }
            Some("event_msg")
                if payload.kind.as_deref() == Some("token_count")
                    && self.replaying_before.is_none() =>
            {
                let Some(ts) = line.timestamp.as_deref().and_then(parse_ts) else {
                    return;
                };
                if let Some(limits) = payload.rate_limits {
                    record_limits(session, ts, limits);
                }
                if let Some(info) = payload.info {
                    self.token_count(session, ts, info);
                }
            }
            _ => {}
        }
    }

    fn token_count(&mut self, session: &mut Session, ts: i64, info: Info) {
        let Some(last) = info.last_token_usage else {
            return;
        };
        if let Some(total) = info.total_token_usage.map(|usage| usage.total()) {
            if let Some(previous) = self.last_total {
                if total == previous {
                    return;
                }
                // A snapshot that lands slightly out of order regresses the
                // total by about one request; counting it would double-count.
                // A larger drop, or a total equal to this request alone (the
                // counter restarted after compaction), is a reset and is counted.
                if total < previous
                    && total != last.total()
                    && (total * 100 >= previous * 98 || total + 2 * last.total() >= previous)
                {
                    return;
                }
            }
            self.last_total = Some(total);
        }

        let tokens = last.tokens();
        if tokens.is_empty() {
            return;
        }
        if info.model.is_some() {
            self.model = info.model;
        }
        let model = session.model_index(self.model.as_deref().unwrap_or("unknown"));
        session.events.push(Event {
            ts,
            model,
            tokens,
            key: None,
        });
    }
}

/// Lowercase hyphenated UUID, whose string order matches UUIDv7 time order.
fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
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
