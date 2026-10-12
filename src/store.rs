//! In-memory index of every source file and the session parsed from it.

use crate::model::{Session, Tool};
use crate::parse::{self, Cursor};
use crate::paths;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct FileEntry {
    pub tool: Tool,
    pub session: Session,
    cursor: Option<Cursor>,
    size: u64,
    modified: Option<SystemTime>,
}

#[derive(Default)]
pub struct Store {
    pub files: HashMap<PathBuf, FileEntry>,
    /// Session titles kept by the tools outside their transcripts.
    pub titles: HashMap<String, String>,
}

/// Which tool owns `path`, if it is a source file toktop reads.
pub fn classify(path: &Path) -> Option<Tool> {
    let name = path.file_name()?.to_str()?;
    for tool in Tool::ALL {
        if !paths::source_roots(tool)
            .iter()
            .any(|root| path.starts_with(root))
        {
            continue;
        }
        let matches = match tool {
            Tool::ClaudeCode => name.ends_with(".jsonl"),
            Tool::Codex => name.starts_with("rollout-") && name.ends_with(".jsonl"),
            Tool::Antigravity => name.ends_with(".db"),
        };
        return matches.then_some(tool);
    }
    None
}

/// Maps SQLite sidecar files (`-wal`, `-shm`, `-journal`) to their database.
pub fn source_path(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    for suffix in ["-wal", "-shm", "-journal"] {
        if let Some(base) = text.strip_suffix(suffix) {
            return PathBuf::from(base);
        }
    }
    path.to_path_buf()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk(&entry.path(), out);
        } else if kind.is_file() {
            out.push(entry.path());
        }
    }
}

impl Store {
    /// Adds an already-parsed session, as if read from `path`.
    pub fn insert_session(&mut self, path: PathBuf, tool: Tool, session: Session) {
        self.files.insert(
            path,
            FileEntry {
                tool,
                session,
                cursor: None,
                size: 0,
                modified: None,
            },
        );
    }

    /// Every source file currently on disk.
    pub fn discover() -> Vec<(PathBuf, Tool)> {
        let mut found = Vec::new();
        for tool in Tool::ALL {
            for root in paths::source_roots(tool) {
                let mut files = Vec::new();
                walk(&root, &mut files);
                found.extend(
                    files
                        .into_iter()
                        .filter(|path| classify(path) == Some(tool))
                        .map(|path| (path, tool)),
                );
            }
        }
        found
    }

    /// Brings the store in line with the disk: parses new or changed files
    /// and drops deleted ones. Returns whether anything changed.
    pub fn sync_all(&mut self) -> bool {
        let found = Self::discover();
        let mut changed = false;
        let present: std::collections::HashSet<&PathBuf> =
            found.iter().map(|(path, _)| path).collect();
        let before = self.files.len();
        self.files.retain(|path, _| present.contains(path));
        changed |= self.files.len() != before;

        // Parse in parallel: the initial scan reads every transcript once.
        let pending: Vec<(PathBuf, Tool)> = found
            .into_iter()
            .filter(|(path, _)| self.is_stale(path))
            .collect();
        if pending.is_empty() {
            if changed {
                self.reload_titles();
            }
            return changed;
        }
        let threads = std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .min(8);
        let chunk = pending.len().div_ceil(threads);
        let results: Vec<(PathBuf, Option<FileEntry>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = pending
                .chunks(chunk.max(1))
                .map(|batch| {
                    scope.spawn(move || {
                        batch
                            .iter()
                            .map(|(path, tool)| (path.clone(), read_file(*tool, path).ok()))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|handle| handle.join().unwrap_or_default())
                .collect()
        });
        for (path, entry) in results {
            if let Some(entry) = entry {
                self.files.insert(path, entry);
            }
        }
        self.reload_titles();
        true
    }

    fn is_stale(&self, path: &Path) -> bool {
        let Some(entry) = self.files.get(path) else {
            return true;
        };
        let Ok(meta) = fs::metadata(path) else {
            return true;
        };
        meta.len() != entry.size || meta.modified().ok() != entry.modified
    }

    /// Re-reads the given files, consuming only appended bytes where possible.
    /// Returns whether anything changed.
    pub fn update(&mut self, paths: impl IntoIterator<Item = PathBuf>) -> bool {
        let mut changed = false;
        let mut titles_dirty = false;
        for path in paths {
            let path = source_path(&path);
            let Some(tool) = classify(&path) else {
                titles_dirty |= is_title_source(&path);
                continue;
            };
            titles_dirty = true;
            if !path.exists() {
                changed |= self.files.remove(&path).is_some();
                continue;
            }
            if !self.is_stale(&path) {
                continue;
            }
            changed = true;
            if let Some(entry) = self.files.get_mut(&path) {
                if let Some(cursor) = entry.cursor.as_mut() {
                    if let Ok(true) = parse::parse_appended(&path, &mut entry.session, cursor) {
                        if let Ok(meta) = fs::metadata(&path) {
                            entry.size = meta.len();
                            entry.modified = meta.modified().ok();
                        }
                        continue;
                    }
                }
            }
            match read_file(tool, &path) {
                Ok(entry) => {
                    self.files.insert(path, entry);
                }
                Err(_) => {
                    // Mid-write SQLite or a vanished file; retried on the next event.
                }
            }
        }
        if titles_dirty {
            changed |= self.reload_titles();
        }
        changed
    }

    /// Returns whether any title changed.
    fn reload_titles(&mut self) -> bool {
        let mut titles = crate::parse::antigravity::titles(&paths::antigravity_homes());
        titles.extend(codex_titles(&paths::codex_session_index()));
        let changed = titles != self.titles;
        self.titles = titles;
        changed
    }
}

fn is_title_source(path: &Path) -> bool {
    path == paths::codex_session_index()
        || path
            .file_name()
            .is_some_and(|name| name == "conversation_summaries.db")
}

fn read_file(tool: Tool, path: &Path) -> io::Result<FileEntry> {
    let meta = fs::metadata(path)?;
    let (session, cursor) = parse::parse_full(tool, path)?;
    Ok(FileEntry {
        tool,
        session,
        cursor,
        size: meta.len(),
        modified: meta.modified().ok(),
    })
}

/// Thread names from Codex's `session_index.jsonl`; later lines win.
fn codex_titles(path: &Path) -> HashMap<String, String> {
    #[derive(serde::Deserialize)]
    struct Line {
        id: String,
        thread_name: Option<String>,
    }
    let Ok(data) = fs::read(path) else {
        return HashMap::new();
    };
    data.split(|&byte| byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Line>(line).ok())
        .filter_map(|line| {
            Some((
                line.id,
                line.thread_name.filter(|name| !name.trim().is_empty())?,
            ))
        })
        .collect()
}

/// Writes `data` to `path` via a temporary file and rename.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)
}
