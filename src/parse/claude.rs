//! Claude Code transcripts: `~/.claude/projects/<project>/<session>.jsonl`.
//!
//! Each assistant response carries a `message.usage` object. Streaming writes
//! the same response (same `message.id` + `requestId`) several times with
//! growing counts, so duplicates are merged with a per-field maximum.
//! `output_tokens` already includes thinking, so `reasoning` stays zero.

use super::{clamp, contains, parse_ts};
use crate::model::{Event, Session, Tokens};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Default)]
pub struct State {
    /// Dedup key -> index into `Session::events`.
    seen: HashMap<Box<str>, usize>,
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
        let tokens = Tokens {
            input: clamp(usage.input_tokens),
            output: clamp(usage.output_tokens),
            cache_read: clamp(usage.cache_read_input_tokens),
            cache_write: clamp(usage.cache_creation_input_tokens),
            cache_write_1h: clamp(
                usage
                    .cache_creation
                    .and_then(|split| split.ephemeral_1h_input_tokens),
            ),
            reasoning: 0,
        };

        let key: Option<Box<str>> = match (&message.id, &line.request_id) {
            (Some(id), Some(request)) => Some(format!("{id}:{request}").into()),
            (Some(id), None) => Some(format!("message:{id}").into()),
            _ => None,
        };
        if let Some(&index) = key.as_ref().and_then(|key| self.seen.get(key)) {
            let event = &mut session.events[index];
            event.tokens.max_merge(&tokens);
            event.tokens.cache_write_1h = event.tokens.cache_write_1h.min(event.tokens.cache_write);
            return;
        }

        let Some(model) = message.model else { return };
        // Locally generated placeholders (errors, interruptions) carry no real usage.
        if model == "<synthetic>" || tokens.is_empty() {
            return;
        }
        let Some(ts) = line.timestamp.as_deref().and_then(parse_ts) else {
            return;
        };
        let mut tokens = tokens;
        tokens.cache_write_1h = tokens.cache_write_1h.min(tokens.cache_write);

        let model = session.model_index(&model);
        if let Some(key) = &key {
            self.seen.insert(key.clone(), session.events.len());
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
