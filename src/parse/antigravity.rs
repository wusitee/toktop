//! Antigravity conversations: `~/.gemini/antigravity{,-cli}/conversations/<uuid>.db`.
//!
//! Each `gen_metadata` row is one generation, a protobuf without a published
//! schema. Field numbers follow tokscale's reverse-engineered layout:
//!
//! - root `#1` chatModel: `#4` usage, `#9.#4` legacy timestamp, `#19` model id
//! - usage: `#1` system prompt + `#2` new input, `#5` cache read, `#9` output,
//!   `#10` thinking (disjoint from `#9`), `#11` responseId
//! - root `#2`: index into `steps`, whose `metadata.#1` is the step timestamp
//! - `trajectory_metadata_blob`: `#1.#1` workspace URI, `#2` created-at
//!
//! Titles come from `conversation_summaries.db` next to `conversations/`.

use super::{file_stem, proto};
use crate::model::{Event, Session, Tokens};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
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

    let mut created_ms = None;
    if let Ok(blob) = conn.query_row(
        "SELECT data FROM trajectory_metadata_blob LIMIT 1",
        [],
        |row| row.get::<_, Vec<u8>>(0),
    ) {
        session.project = proto::bytes(&blob, 1)
            .and_then(|folder| proto::string(folder, 1))
            .map(uri_to_path);
        created_ms = proto::bytes(&blob, 2).and_then(proto::timestamp_ms);
    }
    let fallback_ms = created_ms.or_else(|| mtime_ms(path)).unwrap_or(0);

    let step_times = step_timestamps(&conn).unwrap_or_default();

    let mut stmt = match conn.prepare("SELECT data FROM gen_metadata ORDER BY idx") {
        Ok(stmt) => stmt,
        // Not a generation database.
        Err(_) => return Ok(session),
    };
    let rows = stmt
        .query_map([], |row| row.get::<_, Vec<u8>>(0))
        .map_err(to_io)?;
    let mut seen = std::collections::HashSet::new();
    for blob in rows {
        let blob = blob.map_err(to_io)?;
        let Some(chat) = proto::bytes(&blob, 1) else {
            continue;
        };
        let Some(usage) = proto::bytes(chat, 4) else {
            continue;
        };

        let tokens = Tokens {
            input: proto::varint(usage, 1).unwrap_or(0) + proto::varint(usage, 2).unwrap_or(0),
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

        let ts = step_index(&blob)
            .and_then(|index| step_times.get(&index).copied())
            .or_else(|| {
                proto::bytes(chat, 9)
                    .and_then(|generation| proto::bytes(generation, 4))
                    .and_then(proto::timestamp_ms)
            })
            .unwrap_or(fallback_ms);
        let model = proto::string(chat, 19).unwrap_or("unknown").to_owned();
        let model = session.model_index(&model);
        session.events.push(Event {
            ts,
            model,
            tokens,
            key,
        });
    }
    Ok(session)
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

fn step_timestamps(conn: &Connection) -> rusqlite::Result<HashMap<u64, i64>> {
    let mut stmt = conn.prepare("SELECT idx, metadata FROM steps")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
    })?;
    let mut times = HashMap::new();
    for row in rows {
        let (index, metadata) = row?;
        if let Some(ts) = metadata
            .as_deref()
            .and_then(|meta| proto::bytes(meta, 1))
            .and_then(proto::timestamp_ms)
        {
            times.insert(index as u64, ts);
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
