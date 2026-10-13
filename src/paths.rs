//! Locations of tool logs and of toktop's own cache.

use crate::model::Tool;
use std::env;
use std::path::PathBuf;

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
        Tool::Antigravity => antigravity_homes()
            .into_iter()
            .map(|base| base.join("conversations"))
            .collect(),
    }
}

/// Antigravity IDE and CLI data directories.
pub fn antigravity_homes() -> Vec<PathBuf> {
    let gemini = home().join(".gemini");
    vec![gemini.join("antigravity"), gemini.join("antigravity-cli")]
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

pub fn config_home() -> PathBuf {
    env_dir("XDG_CONFIG_HOME").unwrap_or_else(|| home().join(".config"))
}

pub fn cache_dir() -> PathBuf {
    env_dir("XDG_CACHE_HOME")
        .unwrap_or_else(|| home().join(".cache"))
        .join("toktop")
}
