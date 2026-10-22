//! Locations of tool logs and of toktop's own cache.

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
    let home = home();
    match tool {
        Tool::ClaudeCode => {
            // CLAUDE_CONFIG_DIR may list several directories separated by commas.
            let bases: Vec<PathBuf> = match env::var("CLAUDE_CONFIG_DIR") {
                Ok(value) if !value.trim().is_empty() => value
                    .split(',')
                    .map(|part| PathBuf::from(part.trim()))
                    .collect(),
                _ => vec![home.join(".claude"), config_home().join("claude")],
            };
            bases
                .into_iter()
                .map(|base| base.join("projects"))
                .collect()
        }
        Tool::Codex => {
            let base = env_dir("CODEX_HOME").unwrap_or_else(|| home.join(".codex"));
            vec![base.join("sessions"), base.join("archived_sessions")]
        }
        Tool::Antigravity => vec![
            home.join(".gemini/antigravity/conversations"),
            home.join(".gemini/antigravity-cli/conversations"),
            t3_antigravity_root(),
        ],
        Tool::Pi => vec![env_dir("PI_CODING_AGENT_SESSION_DIR").unwrap_or_else(|| {
            env_dir("PI_CODING_AGENT_DIR")
                .unwrap_or_else(|| home.join(".pi").join("agent"))
                .join("sessions")
        })],
        Tool::Kimi => vec![kimi_sessions()],
        Tool::Dsh => vec![
            env_dir("DSH_HOME")
                .unwrap_or_else(|| home.join(".dsh"))
                .join("sessions"),
        ],
    }
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
    let gemini = home().join(".gemini");
    let mut homes = vec![gemini.join("antigravity"), gemini.join("antigravity-cli")];
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
    let mut sources = Vec::new();
    if tools.contains(&Tool::Codex) {
        sources.push(codex_session_index());
    }
    if tools.contains(&Tool::Antigravity) {
        sources.extend(
            antigravity_homes()
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
