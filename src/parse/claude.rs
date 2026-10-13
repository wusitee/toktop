//! Claude Code transcripts: `~/.claude/projects/<project>/<session>.jsonl`.
//!
//! Each assistant response carries a `message.usage` object. Streaming writes
//! the same response (same `message.id` + `requestId`) several times with
//! growing counts, so duplicates are merged with a per-field maximum. The
//! exception is `input_tokens`: a copy without cache fields reports the whole
//! prompt, while a copy with them reports only the uncached remainder, so a
//! copy that reports the split replaces a bare one instead of being maxed.
//! `output_tokens` already includes thinking, so `reasoning` stays zero.
//!
//! Top-level usage covers the main model only. Advisor calls listed in
//! `usage.iterations` are separate events under their own model.
//!
//! A response's duration runs from the latest `user` or `attachment` line
//! before it (the prompt or tool result that triggered the request) to the
//! last line written for it.

use super::{clamp, contains, parse_ts, prompt_title};
use crate::model::{Event, ServiceTier, Session, Tokens};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct State {
    /// Event index and request start of each keyed response.
    seen: HashMap<Box<str>, (usize, Option<i64>)>,
    /// Timestamp of the latest prompt or tool result line.
    last_input: Option<i64>,
    /// Whether the title came from a user rename, which outranks AI titles.
    custom_title: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    session_id: Option<String>,
    cwd: Option<String>,
    request_id: Option<String>,
    message: Option<Message>,
    ai_title: Option<String>,
    custom_title: Option<String>,
    summary: Option<String>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    cache_creation: Option<CacheCreation>,
    iterations: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Iteration {
    #[serde(rename = "type")]
    kind: Option<String>,
    model: Option<String>,
    #[serde(flatten)]
    usage: Usage,
}

impl Usage {
    fn reports_split(&self) -> bool {
        self.cache_read_input_tokens.is_some() || self.cache_creation_input_tokens.is_some()
    }

    fn cache_write_1h(&self) -> u64 {
        clamp(
            self.cache_creation
                .as_ref()
                .and_then(|split| split.ephemeral_1h_input_tokens),
        )
    }

    fn tokens(&self) -> Tokens {
        let cache_write = clamp(self.cache_creation_input_tokens);
        Tokens {
            input: clamp(self.input_tokens),
            output: clamp(self.output_tokens),
            cache_read: clamp(self.cache_read_input_tokens),
            cache_write,
            cache_write_1h: self.cache_write_1h().min(cache_write),
            reasoning: 0,
        }
    }
}

#[derive(Deserialize)]
struct UserLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "isMeta", default)]
    is_meta: bool,
    message: Option<UserMessage>,
}

#[derive(Deserialize)]
struct UserMessage {
    content: Option<serde_json::Value>,
}

/// Text the user typed, from a `user` line; tool results and injected
/// context yield `None`.
fn user_prompt(raw: &[u8]) -> Option<String> {
    let line: UserLine = serde_json::from_slice(raw).ok()?;
    if line.kind.as_deref() != Some("user") || line.is_meta {
        return None;
    }
    match line.message?.content? {
        serde_json::Value::String(text) => prompt_title(&text),
        serde_json::Value::Array(blocks) => blocks.iter().find_map(|block| {
            (block.get("type")?.as_str()? == "text")
                .then(|| prompt_title(block.get("text")?.as_str()?))
                .flatten()
        }),
        _ => None,
    }
}

#[derive(Deserialize)]
struct CacheCreation {
    ephemeral_1h_input_tokens: Option<i64>,
}

impl State {
    pub fn line(&mut self, session: &mut Session, raw: &[u8]) {
        let has_usage = contains(raw, b"\"usage\"");
        if !has_usage {
            let user = contains(raw, b"\"type\":\"user\"");
            if user || contains(raw, b"\"type\":\"attachment\"") {
                // The first timestamp key is the line's own; nested objects come later.
                if let Some(ts) = first_timestamp(raw) {
                    self.last_input = Some(ts);
                }
            }
            if user && session.prompt.is_none() {
                session.prompt = user_prompt(raw);
            }
        }
        let wants_project = session.project.is_none() && contains(raw, b"\"cwd\"");
        if !has_usage
            && !wants_project
            && !contains(raw, b"-title\"")
            && !contains(raw, b"\"summary\"")
        {
            return;
        }
        let Ok(line) = serde_json::from_slice::<Line>(raw) else {
            return;
        };

        if session.id.is_empty()
            && let Some(id) = line.session_id.as_deref()
        {
            session.id = id.to_owned();
        }
        if session.project.is_none() {
            session.project = line.cwd.clone();
        }

        match line.kind.as_deref() {
            Some("custom-title") => {
                if let Some(title) = non_empty(line.custom_title) {
                    session.title = Some(title);
                    self.custom_title = true;
                }
            }
            Some("ai-title") if !self.custom_title => {
                if let Some(title) = non_empty(line.ai_title) {
                    session.title = Some(title);
                }
            }
            Some("summary") if session.title.is_none() => {
                session.title = non_empty(line.summary);
            }
            Some("assistant") if has_usage => self.assistant(session, line),
            _ => {}
        }
    }

    fn assistant(&mut self, session: &mut Session, line: Line) {
        let Some(message) = line.message else { return };
        let Some(usage) = message.usage else { return };
        let Some(ts) = line.timestamp.as_deref().and_then(parse_ts) else {
            return;
        };
        let key: Option<String> = match (&message.id, &line.request_id) {
            (Some(id), Some(request)) => Some(format!("{id}:{request}")),
            (Some(id), None) => Some(format!("message:{id}")),
            _ => None,
        };
        if let Some(model) = message.model.as_deref() {
            self.record(session, ts, model, &usage, key.clone());
        }
        let iterations = usage
            .iterations
            .as_ref()
            .and_then(serde_json::Value::as_array);
        for (index, value) in iterations.into_iter().flatten().enumerate() {
            let Ok(iteration) = serde_json::from_value::<Iteration>(value.clone()) else {
                continue;
            };
            if iteration.kind.as_deref() != Some("advisor_message") {
                continue;
            }
            let Some(model) = iteration.model.as_deref() else {
                continue;
            };
            // The array position identifies the same advisor call across copies.
            let advisor_key = key.as_ref().map(|key| format!("{key}:advisor:{index}"));
            self.record(session, ts, model, &iteration.usage, advisor_key);
        }
    }

    fn record(
        &mut self,
        session: &mut Session,
        ts: i64,
        model: &str,
        usage: &Usage,
        key: Option<String>,
    ) {
        let mut event = Event {
            ts,
            model: 0,
            tokens: usage.tokens(),
            tier: ServiceTier::Standard,
            key: key.map(Into::into),
            input_is_split: usage.input_tokens.is_some() && usage.reports_split(),
            cache_write_1h_raw: usage.cache_write_1h(),
            duration_ms: 0,
        };
        if let Some(&(index, start)) = event.key.as_deref().and_then(|key| self.seen.get(key)) {
            let known = &mut session.events[index];
            known.max_merge(&event);
            known.duration_ms = known.duration_ms.max(duration(start, ts));
            return;
        }

        // Locally generated placeholders (errors, interruptions) carry no real usage.
        if model.trim().is_empty()
            || model == "<synthetic>"
            || (event.tokens.is_empty() && (event.key.is_none() || event.cache_write_1h_raw == 0))
        {
            return;
        }
        event.model = session.model_index(model);
        // A copy written before any prompt (a resumed session) has no start.
        let start = self.last_input.filter(|start| *start <= ts);
        event.duration_ms = duration(start, ts);
        if let Some(key) = &event.key {
            self.seen.insert(key.clone(), (session.events.len(), start));
        }
        session.events.push(event);
    }
}

/// Milliseconds from `start` to `end`, or 0 when unknown or implausible.
fn duration(start: Option<i64>, end: i64) -> u32 {
    start
        .map(|start| end - start)
        .filter(|ms| *ms > 0 && *ms <= super::MAX_RESPONSE_MS)
        .map_or(0, |ms| ms as u32)
}

/// Value of the first `"timestamp"` key in a raw line.
fn first_timestamp(raw: &[u8]) -> Option<i64> {
    const KEY: &[u8] = b"\"timestamp\":\"";
    let start = memchr::memmem::find(raw, KEY)? + KEY.len();
    let len = memchr::memchr(b'"', &raw[start..])?;
    parse_ts(std::str::from_utf8(&raw[start..start + len]).ok()?)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}
