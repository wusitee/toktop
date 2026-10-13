//! Antigravity conversations: `~/.gemini/antigravity{,-cli}/conversations/<uuid>.db`.
//!
//! Each `gen_metadata` row is one generation, a protobuf without a published
//! schema. Field numbers follow tokscale's reverse-engineered layout:
//!
//! - root `#1` chatModel: `#4` usage, `#9.#4` legacy timestamp, `#19` model id
//! - usage: `#1` system prompt + `#2` new input, `#5` cache read, `#9` output,
//!   `#10` thinking (disjoint from `#9`), `#11` responseId
//! - root `#2`: index into `steps`, whose `metadata.#1` is the step timestamp;
//!   planner responses also join by `metadata.#9.#11` or `metadata.#20.#3`
//! - `trajectory_metadata_blob`: `#1.#1` workspace URI, `#2` created-at
//!
//! Titles come from `conversation_summaries.db` next to `conversations/`.

use super::{file_stem, proto};
use crate::model::{Event, ServiceTier, Session, Tokens};
use rusqlite::{Connection, OpenFlags};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_millis(200))?;
    Ok(conn)
}

fn to_io(error: rusqlite::Error) -> io::Error {
    io::Error::other(error)
}

pub fn parse(path: &Path) -> io::Result<Session> {
    let conn = open(path).map_err(to_io)?;
    // One read transaction so all tables come from the same snapshot.
    conn.execute_batch("BEGIN").map_err(to_io)?;

    let mut session = Session {
        id: file_stem(path),
        ..Session::default()
    };

    // Locking and read errors propagate so the caller keeps its previous
    // snapshot; only a database without these tables is legitimately empty.
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .and_then(|mut stmt| stmt.query_map([], |row| row.get(0))?.collect())
        .map_err(to_io)?;
    let has = |name: &str| tables.iter().any(|table| table == name);
    if !has("gen_metadata") {
        return Ok(session);
    }

    let mut created_ms = None;
    if has("trajectory_metadata_blob") {
        let blob = conn
            .query_row(
                "SELECT data FROM trajectory_metadata_blob LIMIT 1",
                [],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                error => Err(error),
            })
            .map_err(to_io)?;
        if let Some(blob) = blob {
            session.project = proto::bytes(&blob, 1)
                .and_then(|folder| proto::string(folder, 1))
                .map(uri_to_path);
            created_ms = proto::bytes(&blob, 2).and_then(proto::timestamp_ms);
        }
    }
    let fallback_ms = created_ms.or_else(|| mtime_ms(path)).unwrap_or(0);

    let step_times = if has("steps") {
        step_timestamps(&conn).map_err(to_io)?
    } else {
        StepTimestamps::default()
    };

    let mut stmt = conn
        .prepare("SELECT idx, data FROM gen_metadata ORDER BY idx")
        .map_err(to_io)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .map_err(to_io)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(to_io)?;
    let models = SessionModels::from_blobs(rows.iter().map(|(_, blob)| blob.as_slice()));
    let mut seen = HashSet::new();
    for (gen_index, blob) in rows {
        let Some(chat) = proto::bytes(&blob, 1) else {
            continue;
        };
        let Some(usage) = proto::bytes(chat, 4) else {
            continue;
        };

        let tokens = Tokens {
            input: proto::varint(usage, 1)
                .unwrap_or(0)
                .saturating_add(proto::varint(usage, 2).unwrap_or(0)),
            output: proto::varint(usage, 9).unwrap_or(0),
            cache_read: proto::varint(usage, 5).unwrap_or(0),
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: proto::varint(usage, 10).unwrap_or(0),
        };
        if tokens.is_empty() {
            continue;
        }
        let key: Option<Box<str>> = proto::string(usage, 11).map(Into::into);
        if let Some(key) = &key {
            if !seen.insert(key.clone()) {
                continue;
            }
        }

        let ts = proto::bytes(chat, 9)
            .and_then(|generation| proto::bytes(generation, 4))
            .and_then(proto::timestamp_ms)
            .filter(|ts| *ts > 0)
            .or_else(|| step_index(&blob).and_then(|index| step_times.by_step.get(&index).copied()))
            .or_else(|| {
                key.as_deref()
                    .and_then(|id| step_times.by_response.get(id).copied())
            })
            .or_else(|| step_times.by_generation.get(&gen_index).copied())
            .unwrap_or(fallback_ms);
        let response_model = non_empty_string(chat, 19);
        let model = response_model
            .filter(|model| !is_routing_label(model))
            .or_else(|| models.recover(chat))
            .or(response_model)
            .unwrap_or("unknown");
        let model = session.model_index(model);
        session.events.push(Event {
            ts,
            model,
            tokens,
            tier: ServiceTier::Standard,
            key,
            input_is_split: false,
            cache_write_1h_raw: 0,
        });
    }
    Ok(session)
}

/// Display labels only join rows to machine ids observed in this conversation.
#[derive(Default)]
struct SessionModels {
    by_display: HashMap<String, String>,
    sole_model: Option<String>,
}

impl SessionModels {
    fn from_blobs<'a>(blobs: impl IntoIterator<Item = &'a [u8]>) -> Self {
        let mut by_display: HashMap<&str, Option<&str>> = HashMap::new();
        let mut distinct = HashSet::new();
        let mut labels = HashSet::new();
        for blob in blobs {
            let Some(chat) = proto::bytes(blob, 1) else {
                continue;
            };
            let label = non_empty_string(chat, 21);
            labels.extend(label);
            let Some(model) = non_empty_string(chat, 19) else {
                continue;
            };
            if is_routing_label(model) {
                continue;
            }
            distinct.insert(model);
            if let Some(label) = label {
                by_display
                    .entry(label)
                    .and_modify(|resolved| {
                        if let Some(existing) = *resolved {
                            if crate::pricing::aliases::canonical_id(existing)
                                != crate::pricing::aliases::canonical_id(model)
                            {
                                *resolved = None;
                            }
                        }
                    })
                    .or_insert(Some(model));
            }
        }
        let by_display: HashMap<String, String> = by_display
            .into_iter()
            .filter_map(|(label, model)| Some((label.to_owned(), model?.to_owned())))
            .collect();
        // An unidentified label is evidence of a model the file never names.
        let every_label_identified = labels.iter().all(|label| by_display.contains_key(*label));
        let sole_model = if distinct.len() == 1 && every_label_identified {
            distinct.into_iter().next().map(str::to_owned)
        } else {
            None
        };
        Self {
            by_display,
            sole_model,
        }
    }

    fn recover(&self, chat: &[u8]) -> Option<&str> {
        match non_empty_string(chat, 21) {
            Some(label) => self.by_display.get(label).map(String::as_str),
            None => self.sole_model.as_deref(),
        }
    }
}

/// tokscale's generic Antigravity router id carries no concrete model identity.
fn is_routing_label(model: &str) -> bool {
    model.trim().eq_ignore_ascii_case("gemini-default")
}

fn non_empty_string(blob: &[u8], number: u64) -> Option<&str> {
    proto::string(blob, number)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// Root `#2` of a generation: a varint, or a packed list whose first entry is
/// the step that issued the request.
fn step_index(blob: &[u8]) -> Option<u64> {
    proto::fields(blob).find_map(|(number, value)| match (number, value) {
        (2, proto::Value::Varint(index)) => Some(index),
        (2, proto::Value::Bytes(packed)) => proto::fields_packed_first(packed),
        _ => None,
    })
}

#[derive(Default)]
struct StepTimestamps {
    by_step: HashMap<u64, i64>,
    by_response: HashMap<String, i64>,
    by_generation: HashMap<i64, i64>,
}

fn step_timestamps(conn: &Connection) -> rusqlite::Result<StepTimestamps> {
    let query = if conn.column_exists(None, "steps", "step_type")? {
        "SELECT idx, step_type, metadata FROM steps"
    } else {
        "SELECT idx, 15, metadata FROM steps"
    };
    let mut stmt = conn.prepare(query)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<i64>>(1)?,
            row.get::<_, Option<Vec<u8>>>(2)?,
        ))
    })?;
    let mut times = StepTimestamps::default();
    for row in rows {
        let (index, step_type, metadata) = row?;
        let Some(metadata) = metadata else { continue };
        let Some(ts) = proto::bytes(&metadata, 1)
            .and_then(proto::timestamp_ms)
            .filter(|ts| *ts > 0)
        else {
            continue;
        };
        times.by_step.insert(index as u64, ts);
        // Response and generation joins belong to planner response steps.
        if step_type != Some(15) {
            continue;
        }
        if let Some(response) = proto::bytes(&metadata, 9).and_then(|meta| proto::string(meta, 11))
        {
            times.by_response.insert(response.trim().to_owned(), ts);
        }
        if let Some(index) = proto::bytes(&metadata, 20)
            .and_then(|meta| proto::varint(meta, 3))
            .and_then(|index| i64::try_from(index).ok())
        {
            times.by_generation.insert(index, ts);
        }
    }
    Ok(times)
}

fn mtime_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64)
}

fn uri_to_path(uri: &str) -> String {
    uri.strip_prefix("file://").unwrap_or(uri).to_owned()
}

/// Conversation titles from every Antigravity home's summary database.
pub fn titles(homes: &[std::path::PathBuf]) -> HashMap<String, String> {
    let mut titles = HashMap::new();
    for home in homes {
        let path = home.join("conversation_summaries.db");
        if !path.exists() {
            continue;
        }
        let Ok(conn) = open(&path) else { continue };
        let Ok(mut stmt) = conn
            .prepare("SELECT conversation_id, title FROM conversation_summaries WHERE title != ''")
        else {
            continue;
        };
        let Ok(rows) = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?))) else {
            continue;
        };
        titles.extend(rows.flatten());
    }
    titles
}
