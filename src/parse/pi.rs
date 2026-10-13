//! Pi coding agent sessions: `~/.pi/agent/sessions/--<cwd>--/<ts>_<id>.jsonl`.
//!
//! A `session` header carries the id and working directory. Each assistant
//! `message` entry carries Pi's normalized `usage`: `input`, `cacheRead` and
//! `cacheWrite` are disjoint, `output` includes `reasoning`, and
//! `cacheWrite1h` is a subset of `cacheWrite`.
//!
//! Entries form a tree; abandoned branches still consumed tokens, so every
//! assistant entry counts. `/fork` and `/clone` copy entries into a new file,
//! so events are keyed by the provider response id, or by entry id and
//! message time, and counted once across files.
//!
//! A message's `timestamp` (epoch ms) is set when the response starts and the
//! entry is written when it ends, so their difference is the response time.

use super::{MAX_RESPONSE_MS, clamp, contains, parse_ts, prompt_title};
use crate::model::{Event, ServiceTier, Session, Tokens};
use serde::Deserialize;

#[derive(Debug, Default)]
pub struct State {
    have_header: bool,
}

#[derive(Deserialize)]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    name: Option<String>,
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    role: Option<String>,
    model: Option<String>,
    usage: Option<Usage>,
    #[serde(rename = "responseId")]
    response_id: Option<String>,
    /// Epoch milliseconds when the message started.
    timestamp: Option<i64>,
    content: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    input: Option<i64>,
    output: Option<i64>,
    cache_read: Option<i64>,
    cache_write: Option<i64>,
    cache_write1h: Option<i64>,
    reasoning: Option<i64>,
}

impl Usage {
    fn tokens(&self) -> Tokens {
        let output = clamp(self.output);
        let reasoning = clamp(self.reasoning).min(output);
        let cache_write = clamp(self.cache_write);
        Tokens {
            input: clamp(self.input),
            output: output - reasoning,
            cache_read: clamp(self.cache_read),
            cache_write,
            cache_write_1h: clamp(self.cache_write1h).min(cache_write),
            reasoning,
        }
    }
}

impl State {
    pub fn line(&mut self, session: &mut Session, raw: &[u8]) {
        let wanted = (contains(raw, b"\"role\":\"assistant\"") && contains(raw, b"\"usage\""))
            || (!self.have_header && contains(raw, b"\"type\":\"session\""))
            || contains(raw, b"\"type\":\"session_info\"")
            || (session.prompt.is_none() && contains(raw, b"\"role\":\"user\""));
        if !wanted {
            return;
        }
        let Ok(line) = serde_json::from_slice::<Line>(raw) else {
            return;
        };
        match line.kind.as_deref() {
            Some("session") if !self.have_header => {
                self.have_header = true;
                if let Some(id) = line.id {
                    session.id = id;
                }
                session.project = line.cwd;
            }
            Some("session_info") => {
                // The latest rename wins; an empty name clears it.
                session.title = line
                    .name
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty());
            }
            Some("message") => {
                let Some(message) = line.message else { return };
                match message.role.as_deref() {
                    Some("assistant") => {
                        let ts = line.timestamp.as_deref().and_then(parse_ts);
                        assistant(session, ts, line.id, message);
                    }
                    Some("user") if session.prompt.is_none() => {
                        session.prompt = user_prompt(message.content);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

fn assistant(session: &mut Session, written: Option<i64>, entry: Option<String>, message: Message) {
    let Some(usage) = &message.usage else { return };
    let tokens = usage.tokens();
    let Some(ts) = written.or(message.timestamp) else {
        return;
    };
    if tokens.is_empty() {
        return;
    }
    let key = match (&message.response_id, &entry) {
        (Some(response), _) if !response.is_empty() => Some(format!("response:{response}")),
        (_, Some(entry)) => Some(format!("entry:{entry}:{}", message.timestamp.unwrap_or(ts))),
        _ => None,
    };
    let duration_ms = message
        .timestamp
        .zip(written)
        .map(|(start, end)| end - start)
        .filter(|ms| *ms > 0 && *ms <= MAX_RESPONSE_MS)
        .map_or(0, |ms| ms as u32);
    let model = message.model.as_deref().unwrap_or("unknown");
    let model = session.model_index(model);
    session.events.push(Event {
        ts,
        model,
        tokens,
        tier: ServiceTier::Standard,
        key: key.map(Into::into),
        input_is_split: false,
        cache_write_1h_raw: 0,
        duration_ms,
    });
}

fn user_prompt(content: Option<serde_json::Value>) -> Option<String> {
    match content? {
        serde_json::Value::String(text) => prompt_title(&text),
        serde_json::Value::Array(blocks) => blocks.iter().find_map(|block| {
            (block.get("type")?.as_str()? == "text")
                .then(|| prompt_title(block.get("text")?.as_str()?))
                .flatten()
        }),
        _ => None,
    }
}
