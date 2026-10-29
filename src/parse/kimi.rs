//! Kimi Code sessions:
//! `~/.kimi-code/sessions/<workspace>/session_<id>/agents/<agent>/wire.jsonl`.
//!
//! Each agent of a session writes its own wire log: `main` for the
//! conversation, one more per subagent. A subagent's file becomes a session of
//! its own, grouped under the main session.
//!
//! Every `usage.record` line is one increment of usage, as in Kimi's own
//! totals: `turn` records for model calls of a turn, `session` records for
//! work outside turns. `inputOther`, `inputCacheRead` and `inputCacheCreation`
//! are disjoint; `output` includes thinking, which is not reported apart.
//!
//! A fork copies the source's records verbatim into a new session, so events
//! are keyed by agent, write time, model and counts, and counted once across
//! files.
//!
//! The working directory comes from `environmentDisclosure` on `profile.bind`
//! and `config.update` lines. Titles live in each session's `state.json` and
//! are read by [`titles`].

use super::{MAX_RESPONSE_MS, clamp, contains, prompt_title};
use crate::model::{Event, ServiceTier, Session, Tokens};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Agent whose log is the conversation itself.
const MAIN_AGENT: &str = "main";

#[derive(Debug, Default)]
pub struct State {
    /// Write time of the first model call not yet answered by usage.
    request: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(rename = "type")]
    kind: Option<String>,
    agent_id: Option<String>,
    /// Epoch milliseconds.
    time: Option<i64>,
    model: Option<String>,
    usage: Option<Usage>,
    usage_scope: Option<String>,
    /// Kind of model call on `llm.request`.
    #[serde(rename = "kind")]
    request_kind: Option<String>,
    environment_disclosure: Option<Environment>,
}

#[derive(Deserialize)]
struct Environment {
    cwd: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Usage {
    input_other: Option<i64>,
    output: Option<i64>,
    input_cache_read: Option<i64>,
    input_cache_creation: Option<i64>,
}

/// Sets the session id from the file's location: the session directory's id
/// for the main agent, `<session>/<agent>` with the session as parent for a
/// subagent.
pub fn identify(session: &mut Session, path: &Path) {
    let name = |path: Option<&Path>| {
        path.and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
    };
    let agent_dir = path.parent();
    let (Some(agent), Some(id)) = (
        name(agent_dir),
        name(agent_dir.and_then(Path::parent).and_then(Path::parent)),
    ) else {
        return;
    };
    let id = session_id(&id);
    if agent == MAIN_AGENT {
        session.id = id;
    } else {
        session.id = format!("{id}/{agent}");
        session.parent = Some(id);
    }
}

/// Session id from a `session_<uuid>` directory or `state.json` id.
fn session_id(name: &str) -> String {
    name.strip_prefix("session_").unwrap_or(name).to_owned()
}

impl State {
    pub fn line(&mut self, session: &mut Session, raw: &[u8]) {
        let wanted = contains(raw, b"\"usage.record\"")
            || contains(raw, b"\"llm.request\"")
            || contains(raw, b"\"turn.prompt\"")
            || contains(raw, b"\"turn.ended\"")
            || (session.project.is_none() && contains(raw, b"\"environmentDisclosure\""));
        if !wanted {
            return;
        }
        let Ok(line) = serde_json::from_slice::<Line>(raw) else {
            return;
        };
        match line.kind.as_deref() {
            // Side calls (such as title generation) run alongside the loop.
            Some("llm.request")
                if line
                    .request_kind
                    .as_deref()
                    .is_none_or(|kind| kind == "loop") =>
            {
                self.request = self.request.or(line.time);
            }
            // A cancelled, failed or interrupted turn leaves its request
            // unanswered; a turn's boundaries clear it.
            Some("turn.prompt" | "turn.ended") => self.request = None,
            Some("usage.record") => {
                let Some(usage) = line.usage else { return };
                let Some(ts) = line.time else { return };
                // Only a turn's model call answers the pending loop request.
                let start = if line.usage_scope.as_deref() == Some("turn") {
                    self.request.take()
                } else {
                    None
                };
                let tokens = usage.tokens();
                if tokens.is_empty() {
                    return;
                }
                let duration_ms = start
                    .map(|start| ts - start)
                    .filter(|ms| *ms > 0 && *ms <= MAX_RESPONSE_MS)
                    .map_or(0, |ms| ms as u32);
                let model = line.model.as_deref().unwrap_or("unknown");
                let key = format!(
                    "{}:{ts}:{model}:{}:{}:{}:{}",
                    line.agent_id.as_deref().unwrap_or(""),
                    tokens.input,
                    tokens.output,
                    tokens.cache_read,
                    tokens.cache_write,
                );
                let model = session.model_index(model);
                session.events.push(Event {
                    effort: None,
                    ts,
                    model,
                    tokens,
                    tier: ServiceTier::Standard,
                    key: Some(key.into()),
                    input_is_split: false,
                    cache_write_1h_raw: 0,
                    duration_ms,
                });
            }
            _ => {
                if let Some(cwd) = line.environment_disclosure.and_then(|env| env.cwd) {
                    session.project.get_or_insert(cwd);
                }
            }
        }
    }
}

impl Usage {
    fn tokens(&self) -> Tokens {
        Tokens {
            input: clamp(self.input_other),
            output: clamp(self.output),
            cache_read: clamp(self.input_cache_read),
            cache_write: clamp(self.input_cache_creation),
            cache_write_1h: 0,
            reasoning: 0,
        }
    }
}

/// Session titles from `<sessions>/<workspace>/session_<id>/state.json`.
/// Custom and generated titles are kept as is; a title taken from the first
/// prompt is filtered like any prompt fallback.
pub fn titles(sessions: &Path) -> HashMap<String, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct State {
        id: Option<String>,
        title: Option<String>,
        title_kind: Option<String>,
    }
    let mut titles = HashMap::new();
    let Ok(workspaces) = fs::read_dir(sessions) else {
        return titles;
    };
    for workspace in workspaces.flatten() {
        let Ok(dirs) = fs::read_dir(workspace.path()) else {
            continue;
        };
        for dir in dirs.flatten() {
            let Ok(data) = fs::read(dir.path().join("state.json")) else {
                continue;
            };
            let Ok(state) = serde_json::from_slice::<State>(&data) else {
                continue;
            };
            let Some(title) = state.title else { continue };
            let title = match state.title_kind.as_deref() {
                Some("replaceable") | None => prompt_title(&title),
                Some(_) => Some(title.trim().to_owned()).filter(|title| !title.is_empty()),
            };
            let id = state
                .id
                .unwrap_or_else(|| dir.file_name().to_string_lossy().into_owned());
            if let Some(title) = title {
                titles.insert(session_id(&id), title);
            }
        }
    }
    titles
}
