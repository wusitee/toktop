//! Locations of tool logs and of toktop's own cache.

use crate::config::Sources;
use crate::model::Tool;
use std::env;
use std::path::{Path, PathBuf};

fn home() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn env_dir(var: &str) -> Option<PathBuf> {
    env::var_os(var)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// Directories scanned (recursively) for a tool's source files.
pub fn source_roots(tool: Tool) -> Vec<PathBuf> {
    source_roots_with(tool, &[])
}

/// Source directories of the default homes of `tool` and the `extra` ones.
pub fn source_roots_with(tool: Tool, extra: &[String]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = homes(tool, extra)
        .iter()
        .flat_map(|home| home_roots(tool, home))
        .collect();
    match tool {
        Tool::Pi => roots.extend(env_dir("PI_CODING_AGENT_SESSION_DIR")),
        Tool::Antigravity => roots.push(t3_antigravity_root()),
        _ => {}
    }
    roots
}

/// Where a tool keeps sessions inside one of its homes.
fn home_roots(tool: Tool, home: &Path) -> Vec<PathBuf> {
    match tool {
        Tool::ClaudeCode => vec![home.join("projects")],
        Tool::Codex => vec![home.join("sessions"), home.join("archived_sessions")],
        Tool::Antigravity => vec![home.join("conversations")],
        Tool::Pi | Tool::Kimi | Tool::Dsh => vec![home.join("sessions")],
    }
}

/// The directories named by each tool's location variable, or their defaults.
fn default_homes(tool: Tool) -> Vec<PathBuf> {
    let home = home();
    match tool {
        // CLAUDE_CONFIG_DIR may list several directories separated by commas.
        Tool::ClaudeCode => match env::var("CLAUDE_CONFIG_DIR") {
            Ok(value) if !value.trim().is_empty() => value
                .split(',')
                .map(|part| PathBuf::from(part.trim()))
                .collect(),
            _ => vec![home.join(".claude"), config_home().join("claude")],
        },
        Tool::Codex => vec![env_dir("CODEX_HOME").unwrap_or_else(|| home.join(".codex"))],
        Tool::Antigravity => vec![
            home.join(".gemini/antigravity"),
            home.join(".gemini/antigravity-cli"),
        ],
        // An explicit session directory replaces the default home's.
        Tool::Pi if env_dir("PI_CODING_AGENT_SESSION_DIR").is_some() => Vec::new(),
        Tool::Pi => vec![env_dir("PI_CODING_AGENT_DIR").unwrap_or_else(|| home.join(".pi/agent"))],
        Tool::Kimi => vec![env_dir("KIMI_CODE_HOME").unwrap_or_else(|| home.join(".kimi-code"))],
        Tool::Dsh => vec![env_dir("DSH_HOME").unwrap_or_else(|| home.join(".dsh"))],
    }
}

/// Default homes of `tool` plus configured ones, falling back to the
/// defaults when a pattern cannot be expanded.
pub fn homes(tool: Tool, extra: &[String]) -> Vec<PathBuf> {
    try_homes(tool, extra).unwrap_or_else(|_| default_homes(tool))
}

/// Default homes of `tool` plus the directories matching `extra` and, for
/// Codex, `TOKTOP_CODEX_HOMES`, without duplicates or symlink aliases.
/// Defaults keep their spelling, except Codex homes, which are all resolved.
pub fn try_homes(tool: Tool, extra: &[String]) -> std::io::Result<Vec<PathBuf>> {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let from_env = match tool {
        Tool::Codex => env::var("TOKTOP_CODEX_HOMES").unwrap_or_default(),
        _ => String::new(),
    };
    let mut homes = default_homes(tool);
    if tool == Tool::Codex {
        for path in &mut homes {
            *path = canonical(path);
        }
    }
    let mut seen: Vec<PathBuf> = homes.iter().map(|path| canonical(path)).collect();
    let mut added = Vec::new();
    for pattern in extra
        .iter()
        .map(String::as_str)
        .chain(from_env.split(':').filter(|p| !p.is_empty()))
    {
        let expanded = pattern.strip_prefix("~/").map_or_else(
            || pattern.to_owned(),
            |rest| home().join(rest).to_string_lossy().into_owned(),
        );
        let paths = glob::glob(&expanded).map_err(std::io::Error::other)?;
        for path in paths {
            let path =
                path.map_err(|error| std::io::Error::new(error.error().kind(), error.to_string()))?;
            let path = canonical(&path);
            if path.is_dir() && !seen.contains(&path) {
                seen.push(path.clone());
                added.push(path);
            }
        }
    }
    added.sort();
    homes.extend(added);
    if tool == Tool::Codex {
        homes.sort();
    }
    Ok(homes)
}

/// Kimi Code's session directory.
pub fn kimi_sessions() -> PathBuf {
    env_dir("KIMI_CODE_HOME")
        .unwrap_or_else(|| home().join(".kimi-code"))
        .join("sessions")
}

fn t3_antigravity_root() -> PathBuf {
    home().join(".t3/userdata/providers/antigravity")
}

/// Keep T3 provider scans and watches in conversation directories, excluding
/// brain artifacts, configuration and credentials. Other roots are unfiltered.
pub fn is_source_path(path: &Path) -> bool {
    let root = t3_antigravity_root();
    let Ok(relative) = path.strip_prefix(&root) else {
        return true;
    };
    let mut parts = relative.iter();
    parts.next(); // Provider instance.
    let Some(app) = parts.next() else { return true };
    app == "antigravity-acp" && parts.next().is_none_or(|part| part == "conversations")
}

/// Antigravity IDE, CLI and T3 provider data directories.
pub fn antigravity_homes() -> Vec<PathBuf> {
    antigravity_homes_with(&[])
}

/// [`antigravity_homes`] plus the `extra` ones.
pub fn antigravity_homes_with(extra: &[String]) -> Vec<PathBuf> {
    let mut homes = homes(Tool::Antigravity, extra);
    if let Ok(providers) = std::fs::read_dir(t3_antigravity_root()) {
        let mut isolated: Vec<_> = providers
            .flatten()
            .map(|provider| provider.path().join("antigravity-acp"))
            .filter(|path| path.is_dir())
            .collect();
        isolated.sort();
        homes.extend(isolated);
    }
    homes
}

/// Codex's thread index, which carries thread titles.
pub fn codex_session_index() -> PathBuf {
    env_dir("CODEX_HOME")
        .unwrap_or_else(|| home().join(".codex"))
        .join("session_index.jsonl")
}

/// Files outside the source roots that hold session titles for `tools`.
pub fn title_sources(tools: &[Tool]) -> Vec<PathBuf> {
    title_sources_with(tools, &Sources::default())
}

pub fn title_sources_with(tools: &[Tool], extra: &Sources) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    if tools.contains(&Tool::Codex) {
        sources.extend(
            homes(Tool::Codex, extra.get(Tool::Codex))
                .into_iter()
                .map(|home| home.join("session_index.jsonl")),
        );
    }
    if tools.contains(&Tool::Antigravity) {
        sources.extend(
            antigravity_homes_with(extra.get(Tool::Antigravity))
                .into_iter()
                .map(|home| home.join("conversation_summaries.db")),
        );
    }
    sources
}

/// Whether `path` holds session titles for `tools`: one of
/// [`title_sources`], or a Kimi Code session's `state.json`.
pub fn is_title_source(tools: &[Tool], path: &std::path::Path) -> bool {
    // Cheap filename gate first: this runs for every watched event, and
    // `title_sources` has to scan the Antigravity provider directories.
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if !matches!(
        name,
        "conversation_summaries.db" | "session_index.jsonl" | "state.json"
    ) {
        return false;
    }
    title_sources(tools).iter().any(|source| source == path)
        || (tools.contains(&Tool::Kimi)
            && name == "state.json"
            && path.starts_with(kimi_sessions()))
}

pub fn config_home() -> PathBuf {
    env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config"))
}

pub fn cache_dir() -> PathBuf {
    env_dir("XDG_CACHE_HOME")
        .unwrap_or_else(|| home().join(".cache"))
        .join("toktop")
}
