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

use super::{clamp, contains, parse_ts};
use crate::model::{Event, Session, Tokens};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct State {
    seen: HashMap<Box<str>, Seen>,
    /// Whether the title came from a user rename, which outranks AI titles.
    custom_title: bool,
}

#[derive(Debug)]
struct Seen {
    /// Index into `Session::events`.
    index: usize,
    /// Whether the stored input came from a copy that reported the cache split.
    split: bool,
    /// Largest 1-hour cache-write split seen, before clamping to the total.
    cache_write_1h: u64,
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
    #[serde(default)]
    iterations: Vec<Iteration>,
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
struct CacheCreation {
    ephemeral_1h_input_tokens: Option<i64>,
}

impl State {
    pub fn line(&mut self, session: &mut Session, raw: &[u8]) {
        let has_usage = contains(raw, b"\"usage\"");
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

        if session.id.is_empty() {
            if let Some(id) = line.session_id.as_deref() {
                session.id = id.to_owned();
            }
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
        for (index, iteration) in usage.iterations.iter().enumerate() {
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
        if let Some(seen) = key.as_deref().and_then(|key| self.seen.get_mut(key)) {
            let event = &mut session.events[seen.index];
            let tokens = &mut event.tokens;
            let input = tokens.input;
            tokens.max_merge(&usage.tokens());
            tokens.input = match (usage.reports_split(), seen.split) {
                (true, false) => usage
                    .input_tokens
                    .map_or(input, |value| value.max(0) as u64),
                (false, true) => input,
                _ => tokens.input,
            };
            seen.split |= usage.reports_split();
            seen.cache_write_1h = seen.cache_write_1h.max(usage.cache_write_1h());
            tokens.cache_write_1h = seen.cache_write_1h.min(tokens.cache_write);
            return;
        }

        // Locally generated placeholders (errors, interruptions) carry no real usage.
        let tokens = usage.tokens();
        if model.trim().is_empty() || model == "<synthetic>" || tokens.is_empty() {
            return;
        }
        let model = session.model_index(model);
        let key: Option<Box<str>> = key.map(Into::into);
        if let Some(key) = &key {
            self.seen.insert(
                key.clone(),
                Seen {
                    index: session.events.len(),
                    split: usage.reports_split(),
                    cache_write_1h: usage.cache_write_1h(),
                },
            );
        }
        session.events.push(Event {
            ts,
            model,
            tokens,
            key,
        });
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}
