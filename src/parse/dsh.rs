//! DeepSeek Harness (`dsh`) sessions:
//! `~/.dsh/sessions/--<cwd>--/<session-id>/session.jsonl.zstd`.
//!
//! Each session is one append-only JSONL log, stored by default as
//! concatenated Zstandard frames: one for the header line, then one per
//! appended batch of lines. Complete frames are read as they land; a frame
//! still being written is left for the next read. With compression turned
//! off, dsh writes plain `session.jsonl` with the same lines.
//!
//! The first line is the `session` header with the id, working directory and,
//! for a child session, `parentSession`. A subagent child (`origin`
//! `subagent`) is grouped under its parent. A fork child starts with
//! `seedLength` events copied from its parent, which are skipped.
//!
//! Usage comes from `assistant/message` lines (one per model step) and
//! `compaction/summary` lines (the summarization call). `inputTokens`
//! (uncached) and `cacheReadTokens` are disjoint; `outputTokens` includes
//! `reasoningTokens`. Events are keyed by message or compaction id, so a copy
//! in another file counts once.
//!
//! A step's request starts at its `step/start` line and ends when its
//! `assistant/message` is written; a compaction runs from `compaction/start`
//! to its summary.

use super::{MAX_RESPONSE_MS, clamp, prompt_title};
use crate::model::{Event, ServiceTier, Session, Tokens};
use serde::Deserialize;

#[derive(Debug, Default)]
pub struct State {
    have_header: bool,
    /// Lines with a smaller `seq` were copied from the parent.
    seed_length: u64,
    /// Time of the latest `step/start`, until its message.
    step_start: Option<i64>,
    /// Id and time of the latest `compaction/start`, until its summary.
    compaction_start: Option<(String, i64)>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(rename = "type")]
    kind: String,
    seq: Option<u64>,
    /// Epoch milliseconds.
    time: Option<i64>,
    data: Option<Data>,
    // Header fields.
    id: Option<String>,
    cwd: Option<String>,
    parent_session: Option<String>,
    seed_length: Option<u64>,
    origin: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    usage: Option<Usage>,
    /// On `assistant/message`.
    message: Option<Message>,
    /// On `compaction/*`.
    compaction_id: Option<String>,
    /// On `compaction/summary`.
    model: Option<String>,
    /// On `session/title`.
    title: Option<String>,
    /// On `user/message`.
    content: Option<serde_json::Value>,
    source: Option<Source>,
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    source: Option<Source>,
}

#[derive(Deserialize)]
struct Source {
    kind: Option<String>,
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
}

impl Usage {
    fn tokens(&self) -> Tokens {
        let output = clamp(self.output_tokens);
        let reasoning = clamp(self.reasoning_tokens).min(output);
        Tokens {
            input: clamp(self.input_tokens),
            output: output - reasoning,
            cache_read: clamp(self.cache_read_tokens),
            reasoning,
            ..Tokens::default()
        }
    }
}

/// Line types read; every line starts with its type.
const WANTED: [&[u8]; 7] = [
    b"{\"type\":\"session\"",
    b"{\"type\":\"assistant/message\"",
    b"{\"type\":\"step/start\"",
    b"{\"type\":\"compaction/start\"",
    b"{\"type\":\"compaction/summary\"",
    b"{\"type\":\"session/title\"",
    b"{\"type\":\"user/message\"",
];

impl State {
    pub fn line(&mut self, session: &mut Session, raw: &[u8]) {
        if !WANTED.iter().any(|prefix| raw.starts_with(prefix))
            || (session.prompt.is_some() && raw.starts_with(WANTED[6]))
        {
            return;
        }
        let Ok(line) = serde_json::from_slice::<Line>(raw) else {
            return;
        };
        if line.kind == "session" {
            if !self.have_header {
                self.have_header = true;
                if let Some(id) = line.id {
                    session.id = id;
                }
                session.project = line.cwd;
                if line.origin.as_deref() == Some("subagent") {
                    session.parent = line.parent_session;
                }
                self.seed_length = line.seed_length.unwrap_or(0);
            }
            return;
        }
        // Inherited parent history.
        if line.seq.unwrap_or(0) < self.seed_length {
            return;
        }
        let (Some(time), Some(data)) = (line.time, line.data) else {
            return;
        };
        match line.kind.as_str() {
            "step/start" => self.step_start = Some(time),
            "assistant/message" => {
                let start = self.step_start.take();
                let Some(usage) = &data.usage else { return };
                let message = data.message.as_ref();
                let model = message
                    .and_then(|message| message.source.as_ref())
                    .and_then(|source| source.model.clone());
                let key = message
                    .and_then(|message| message.id.as_deref())
                    .map(|id| format!("message:{id}"));
                record(session, usage, model, time, start, key);
            }
            "compaction/start" => {
                if let Some(id) = data.compaction_id {
                    self.compaction_start = Some((id, time));
                }
            }
            "compaction/summary" => {
                let Some(usage) = &data.usage else { return };
                let start = match (&self.compaction_start, &data.compaction_id) {
                    (Some((started, start)), Some(id)) if started == id => Some(*start),
                    _ => None,
                };
                self.compaction_start = None;
                let key = data.compaction_id.map(|id| format!("compaction:{id}"));
                record(session, usage, data.model, time, start, key);
            }
            "session/title" => {
                session.title = data
                    .title
                    .map(|title| title.trim().to_owned())
                    .filter(|title| !title.is_empty());
            }
            // Typed prompts only, not plugin notices.
            "user/message"
                if data
                    .source
                    .as_ref()
                    .and_then(|source| source.kind.as_deref())
                    == Some("user") =>
            {
                session.prompt = user_prompt(data.content);
            }
            _ => {}
        }
    }
}

fn record(
    session: &mut Session,
    usage: &Usage,
    model: Option<String>,
    ts: i64,
    start: Option<i64>,
    key: Option<String>,
) {
    let tokens = usage.tokens();
    if tokens.is_empty() {
        return;
    }
    let duration_ms = start
        .map(|start| ts - start)
        .filter(|ms| *ms > 0 && *ms <= MAX_RESPONSE_MS)
        .map_or(0, |ms| ms as u32);
    let model = session.model_index(model.as_deref().unwrap_or("unknown"));
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
    let serde_json::Value::Array(blocks) = content? else {
        return None;
    };
    blocks.iter().find_map(|block| {
        (block.get("type")?.as_str()? == "text")
            .then(|| prompt_title(block.get("text")?.as_str()?))
            .flatten()
    })
}
