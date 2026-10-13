//! Per-tool source parsers.
//!
//! Claude Code and Codex write append-only JSONL, so their parsers keep a
//! [`Cursor`] and consume only bytes appended since the previous read.
//! Antigravity writes SQLite databases, which are re-read whole on change.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod proto;

use crate::model::{Session, Tool};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

/// Resumable position in an append-only JSONL file, with the parser state
/// needed to continue from it.
#[derive(Debug)]
pub struct Cursor {
    /// Byte offset just past the last complete line consumed.
    pub offset: u64,
    /// Inode the offset refers to; a replaced file is re-read from the start.
    pub inode: u64,
    pub state: LineState,
}

#[derive(Debug)]
pub enum LineState {
    Claude(claude::State),
    Codex(codex::State),
}

impl LineState {
    fn new(tool: Tool) -> Option<Self> {
        match tool {
            Tool::ClaudeCode => Some(LineState::Claude(claude::State::default())),
            Tool::Codex => Some(LineState::Codex(codex::State::default())),
            Tool::Antigravity => None,
        }
    }

    fn line(&mut self, session: &mut Session, line: &[u8]) {
        match self {
            LineState::Claude(state) => state.line(session, line),
            LineState::Codex(state) => state.line(session, line),
        }
    }
}

/// Parses `path` from scratch.
pub fn parse_full(tool: Tool, path: &Path) -> io::Result<(Session, Option<Cursor>)> {
    match LineState::new(tool) {
        Some(state) => {
            let mut session = Session::default();
            let mut cursor = Cursor {
                offset: 0,
                inode: 0,
                state,
            };
            read_lines(path, &mut session, &mut cursor)?;
            if session.id.is_empty() {
                session.id = file_stem(path);
            }
            Ok((session, Some(cursor)))
        }
        None => Ok((antigravity::parse(path)?, None)),
    }
}

/// Consumes lines appended after `cursor.offset`. Returns `false` when the
/// file was truncated or replaced and must be parsed from scratch instead.
pub fn parse_appended(path: &Path, session: &mut Session, cursor: &mut Cursor) -> io::Result<bool> {
    let meta = std::fs::metadata(path)?;
    if inode(&meta) != cursor.inode || meta.len() < cursor.offset {
        return Ok(false);
    }
    read_lines(path, session, cursor)?;
    Ok(true)
}

fn read_lines(path: &Path, session: &mut Session, cursor: &mut Cursor) -> io::Result<()> {
    let mut file = File::open(path)?;
    cursor.inode = inode(&file.metadata()?);
    file.seek(SeekFrom::Start(cursor.offset))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line = Vec::with_capacity(4096);
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        // A trailing line without a newline may still be mid-write; leave it
        // for the next read.
        if read == 0 || line.last() != Some(&b'\n') {
            return Ok(());
        }
        cursor.offset += read as u64;
        cursor.state.line(session, &line);
    }
}

#[cfg(unix)]
pub(crate) fn inode(meta: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(meta)
}

#[cfg(not(unix))]
pub(crate) fn inode(_meta: &std::fs::Metadata) -> u64 {
    0
}

pub(crate) fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Cheap substring test used to skip lines before JSON parsing.
pub(crate) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(haystack, needle).is_some()
}

/// RFC 3339 timestamp to epoch milliseconds.
pub(crate) fn parse_ts(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis())
}

pub(crate) fn clamp(value: Option<i64>) -> u64 {
    value.unwrap_or(0).max(0) as u64
}
