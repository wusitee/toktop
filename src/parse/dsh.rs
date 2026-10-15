//! DeepSeek Harness (`dsh`) sessions:
//! `~/.dsh/sessions/--<cwd>--/<session-id>/session[.vN].jsonl.zstd`.
//!
//! Each session is one append-only JSONL log, stored by default as
//! concatenated Zstandard frames: one for the header line, then one per
//! appended batch of lines. Complete frames are read as they land; a frame
//! still being written is left for the next read. With compression turned
//! off, dsh writes plain `session[.vN].jsonl` with the same lines.
//!
//! Each format version is a generation of its own: `session.jsonl.zstd` is
//! v0, `session.v4.jsonl.zstd` v4. Opening an older session migrates it into
//! a new generation and leaves the old file in place, so only the highest
//! generation of a session directory is read ([`generation`],
//! [`superseded`]).
//!
//! The first line is the `session` header with the id, working directory and,
//! for a child session, `parentSession`. A subagent child (`origin`
//! `subagent`) is grouped under its parent. A fork child starts with history
//! copied from its parent, which is skipped: up to `seedLength` events (v0),
//! or up to its last `session/end-seed` marker tagged `inherited` (v4).
//!
//! Usage comes from `assistant/message` lines (one per settled model call),
//! `assistant/attempt` lines (a failed, retried or cancelled call; v4) and
//! `compaction/summary` lines (the summarization call). `inputTokens`
//! (uncached), `cacheReadTokens` and `cacheWriteTokens` are disjoint;
//! `outputTokens` includes `reasoningTokens`. A message carries `usage`; an
//! attempt, or a message without it, uses the last `usage` chunk of its
//! embedded `stream`. In v0 logs, a step's `usage` chunk is a line of its own
//! and counts as soon as it is written, even when the request then fails; a
//! later sample of the same attempt, or its message's usage, replaces it.
//! An `llm/retry-started` line begins a new attempt, billed on its own. Usage
//! without a model of its own takes that of the latest `request/context`
//! (inherited ones included). Events are keyed by message or compaction id,
//! so a copy in another file counts once.
//!
//! A request starts at its `step/start` (or `llm/retry-started`) line and
//! ends when its message, attempt or usage sample is written; a compaction
//! runs from `compaction/start` to its summary.

use super::{MAX_RESPONSE_MS, clamp, contains, prompt_title};
use crate::model::{Event, ServiceTier, Session, Tokens};
use serde::Deserialize;
use std::path::Path;

/// Format generation of a dsh session log named `name`: 0 for
/// `session.jsonl[.zstd]`, N for `session.vN.jsonl[.zstd]`.
pub fn generation(name: &str) -> Option<u32> {
    let stem = name
        .strip_suffix(".zstd")
        .unwrap_or(name)
        .strip_suffix(".jsonl")?;
    match stem.strip_prefix("session") {
        Some("") => Some(0),
        Some(rest) => rest.strip_prefix(".v")?.parse().ok(),
        None => None,
    }
}

/// Whether a later generation of the same session sits beside `path`.
pub fn superseded(path: &Path) -> bool {
    let generation_of = |path: &Path| generation(path.file_name()?.to_str()?);
    let (Some(own), Some(dir)) = (generation_of(path), path.parent()) else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| generation_of(&entry.path()).is_some_and(|other| other > own))
}

#[derive(Debug, Default)]
pub struct State {
    have_header: bool,
    /// Lines with a smaller `seq` were copied from the parent.
    seed_length: u64,
    /// Time of the latest `step/start`.
    step_start: Option<i64>,
    /// Turn, step and event index of the current attempt's usage sample.
    sample: Option<(u64, u64, usize)>,
    /// Retries so far in the current step.
    attempt: u32,
    /// Model of the latest `request/context`.
    model: Option<String>,
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
    /// On `compaction/summary` and `request/context`.
    model: Option<String>,
    /// On `session/title`.
    title: Option<String>,
    /// On `user/message`.
    content: Option<serde_json::Value>,
    /// Origin of a `user/message`.
    source: Option<PromptSource>,
    /// On step lines and `assistant/*`.
    turn: Option<u64>,
    step: Option<u64>,
    /// On `assistant/chunk`.
    chunk: Option<Chunk>,
    /// Timed model stream embedded in `assistant/message` and
    /// `assistant/attempt` (v4).
    stream: Option<Vec<StreamRecord>>,
    /// On `session/end-seed`: the marker ends the fork-inherited prefix.
    inherited: Option<bool>,
}

/// One stream record; packed delta rows have no `chunk`.
#[derive(Deserialize)]
struct StreamRecord {
    chunk: Option<Chunk>,
}

impl Data {
    /// The last usage reported in the embedded stream.
    fn stream_usage(&self) -> Option<&Usage> {
        self.stream.as_ref()?.iter().rev().find_map(|record| {
            let chunk = record.chunk.as_ref()?;
            (chunk.kind.as_deref() == Some("usage"))
                .then_some(chunk.usage.as_ref())
                .flatten()
        })
    }
}

#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    source: Option<ModelSource>,
}

#[derive(Deserialize)]
struct PromptSource {
    kind: Option<String>,
}

#[derive(Deserialize)]
struct ModelSource {
    model: Option<String>,
}

#[derive(Deserialize)]
struct Chunk {
    #[serde(rename = "type")]
    kind: Option<String>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
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
            cache_write: clamp(self.cache_write_tokens),
            cache_write_1h: 0,
            reasoning,
        }
    }
}

/// Line types read; every line starts with its type.
const WANTED: [&[u8]; 11] = [
    b"{\"type\":\"session\"",
    b"{\"type\":\"assistant/message\"",
    b"{\"type\":\"assistant/attempt\"",
    b"{\"type\":\"step/start\"",
    b"{\"type\":\"llm/retry-started\"",
    b"{\"type\":\"session/end-seed\"",
    b"{\"type\":\"request/context\"",
    b"{\"type\":\"compaction/start\"",
    b"{\"type\":\"compaction/summary\"",
    b"{\"type\":\"session/title\"",
    b"{\"type\":\"user/message\"",
];

impl State {
    pub fn line(&mut self, session: &mut Session, raw: &[u8]) {
        let wanted = WANTED.iter().any(|prefix| raw.starts_with(prefix))
            && !(session.prompt.is_some() && raw.starts_with(WANTED[10]));
        // Only usage chunks among the many streaming ones.
        let usage_chunk = raw.starts_with(b"{\"type\":\"assistant/chunk\"")
            && contains(raw, b"\"chunk\":{\"type\":\"usage\"");
        if !wanted && !usage_chunk {
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
        let (Some(time), Some(data)) = (line.time, line.data) else {
            return;
        };
        // The route stays in effect after a fork, so inherited records set it.
        if line.kind == "request/context" {
            self.model = data.model;
            return;
        }
        // Inherited parent history.
        if line.seq.unwrap_or(0) < self.seed_length {
            return;
        }
        if line.kind == "session/end-seed" {
            if data.inherited == Some(true) {
                // Everything so far was copied from the parent. A fork of a
                // fork also holds its ancestor's marker; the last one counts.
                session.events.clear();
                session.title = None;
                session.prompt = None;
                self.sample = None;
                self.attempt = 0;
                self.step_start = None;
                self.compaction_start = None;
            }
            return;
        }
        match line.kind.as_str() {
            "step/start" => {
                self.step_start = Some(time);
                self.sample = None;
                self.attempt = 0;
            }
            "llm/retry-started" => {
                self.step_start = Some(time);
                self.sample = None;
                self.attempt += 1;
            }
            "assistant/attempt" => {
                let Some(usage) = data.stream_usage() else {
                    return;
                };
                let key = format!("attempt:{}:{}", session.id, line.seq.unwrap_or(0));
                let model = self.model.clone();
                record(
                    session,
                    None,
                    usage.tokens(),
                    model,
                    time,
                    self.step_start,
                    Some(key),
                );
            }
            "assistant/chunk" => {
                let (Some(turn), Some(step), Some(chunk)) = (data.turn, data.step, data.chunk)
                else {
                    return;
                };
                let (Some("usage"), Some(usage)) = (chunk.kind.as_deref(), chunk.usage) else {
                    return;
                };
                let slot = self.slot(turn, step);
                let key = format!("sample:{}:{turn}:{step}:{}", session.id, self.attempt);
                let model = self.model.clone();
                if let Some(index) = record(
                    session,
                    slot,
                    usage.tokens(),
                    model,
                    time,
                    self.step_start,
                    Some(key),
                ) {
                    self.sample = Some((turn, step, index));
                }
            }
            "assistant/message" => {
                // A message without usage leaves the step's sample counted.
                let Some(usage) = data.usage.as_ref().or_else(|| data.stream_usage()) else {
                    return;
                };
                let slot = match (data.turn, data.step) {
                    (Some(turn), Some(step)) => self.slot(turn, step),
                    _ => None,
                };
                self.sample = None;
                let message = data.message.as_ref();
                let model = message
                    .and_then(|message| message.source.as_ref())
                    .and_then(|source| source.model.clone());
                let key = message
                    .and_then(|message| message.id.as_deref())
                    .map(|id| format!("message:{id}"));
                record(
                    session,
                    slot,
                    usage.tokens(),
                    model,
                    time,
                    self.step_start,
                    key,
                );
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
                record(session, None, usage.tokens(), data.model, time, start, key);
            }
            "session/title" => {
                let fallback = data
                    .source
                    .as_ref()
                    .and_then(|source| source.kind.as_deref())
                    == Some("fallback");
                session.title = data.title.and_then(|title| {
                    if fallback {
                        // Cut from the first message, which may be injected context.
                        prompt_title(&title)
                    } else {
                        Some(title.trim().to_owned()).filter(|title| !title.is_empty())
                    }
                });
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

    /// Event index of the current attempt's sample of `turn`/`step`, if one
    /// was counted.
    fn slot(&self, turn: u64, step: u64) -> Option<usize> {
        self.sample
            .filter(|sample| (sample.0, sample.1) == (turn, step))
            .map(|sample| sample.2)
    }
}

/// Adds one usage event written at `ts`, or replaces the event at `slot`.
/// Returns the event's index.
fn record(
    session: &mut Session,
    slot: Option<usize>,
    tokens: Tokens,
    model: Option<String>,
    ts: i64,
    start: Option<i64>,
    key: Option<String>,
) -> Option<usize> {
    if tokens.is_empty() && slot.is_none() {
        return None;
    }
    let duration_ms = start
        .map(|start| ts - start)
        .filter(|ms| *ms > 0 && *ms <= MAX_RESPONSE_MS)
        .map_or(0, |ms| ms as u32);
    let model = session.model_index(model.as_deref().unwrap_or("unknown"));
    let event = Event {
        ts,
        model,
        tokens,
        tier: ServiceTier::Standard,
        key: key.map(Into::into),
        input_is_split: false,
        cache_write_1h_raw: 0,
        duration_ms,
    };
    match slot {
        Some(index) => {
            session.events[index] = event;
            Some(index)
        }
        None => {
            session.events.push(event);
            Some(session.events.len() - 1)
        }
    }
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
