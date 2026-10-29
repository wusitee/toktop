//! User settings in `$XDG_CONFIG_HOME/toktop/config.toml`.
//!
//! The file is optional and only written when a setting changes. A file that
//! fails to parse is never overwritten: its settings fall back to defaults
//! for the session and changes are kept in memory only.

use crate::aggregate::Period;
use crate::model::Tool;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Period selected at startup.
    #[serde(with = "period_key")]
    pub period: Period,
    /// Register a tray icon when a StatusNotifierItem host is available.
    pub tray: bool,
    /// Tools whose logs are read.
    #[serde(with = "tool_keys")]
    pub tools: Vec<Tool>,
    /// SSH host aliases to include in usage totals.
    pub remote: Vec<Remote>,
    /// Additional Codex homes, with tilde and glob expansion on every scan.
    pub codex_homes: Vec<String>,
    pub project_group: Vec<ProjectGroup>,
    pub widget: Widget,
    /// The `[window]` table of earlier versions, accepted and dropped.
    #[serde(rename = "window", skip_serializing)]
    pub legacy_window: Option<serde::de::IgnoredAny>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remote {
    /// An SSH host alias from the user's SSH configuration.
    pub host: String,
}

/// First matching group wins; patterns match full project paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectGroup {
    pub name: String,
    #[serde(rename = "match")]
    pub patterns: Vec<String>,
}

pub fn project_name<'a>(groups: &'a [ProjectGroup], path: &'a str) -> &'a str {
    let raw = path
        .split_once(':')
        .filter(|(host, path)| !host.contains('/') && path.starts_with('/'))
        .map_or(path, |(_, path)| path);
    groups
        .iter()
        .find(|group| {
            group.patterns.iter().any(|pattern| {
                glob::Pattern::new(pattern).is_ok_and(|pattern| pattern.matches(raw))
            })
        })
        .map_or(path, |group| group.name.as_str())
}

/// Placement and contents of the widget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Widget {
    pub layer: Layer,
    pub anchor: Anchor,
    /// Distance from the anchored screen edges, in pixels.
    pub margin: i32,
    /// Monitor connector name, such as `DP-1`. Empty lets the compositor choose.
    pub output: String,
    pub width: i32,
    /// Visible rows before the models, projects and tools list scrolls.
    pub rows: usize,
    /// Recent sessions shown when collapsed.
    pub sessions: usize,
    pub sections: Sections,
}

/// Parts of the card that are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sections {
    /// Total cost split into input, cache read, cache write and output.
    pub cost: bool,
    /// Recent speed, live rate, cache and streak tiles.
    pub stats: bool,
    /// Codex rate limits.
    pub limits: bool,
    /// Models, projects and tools list.
    pub breakdown: bool,
    pub sessions: bool,
    /// Activity heatmap.
    pub activity: bool,
    pub trend: bool,
}

impl Widget {
    /// Recent sessions shown: none when the section is hidden.
    pub fn shown_sessions(&self) -> usize {
        if self.sections.sessions {
            self.sessions
        } else {
            0
        }
    }

    /// Sets how many recent sessions are shown, keeping hidden ones hidden.
    pub fn set_session_count(&mut self, count: usize) {
        if self.shown_sessions() == 0 {
            self.sections.sessions = false;
        }
        self.sessions = count;
    }

    /// Shows or hides recent sessions. A count of zero, which also hides
    /// them, becomes one when they are shown.
    pub fn show_sessions(&mut self, on: bool) {
        self.sections.sessions = on;
        if on {
            self.sessions = self.sessions.max(1);
        }
    }
}

impl Default for Sections {
    fn default() -> Self {
        Sections {
            cost: true,
            stats: true,
            limits: true,
            breakdown: true,
            sessions: true,
            activity: true,
            trend: true,
        }
    }
}

/// Where the widget lives. The first four are layer-shell layers, from the
/// wallpaper up; `Window` is an ordinary window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layer {
    Background,
    Bottom,
    Top,
    Overlay,
    Window,
}

impl Layer {
    /// Whether the widget is part of the desktop and shown from startup,
    /// rather than a surface the user summons.
    pub fn is_desktop(self) -> bool {
        matches!(self, Layer::Background | Layer::Bottom)
    }
}

/// Screen position of a layer-shell widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// Anchored edges as (top, bottom, left, right).
    pub fn edges(self) -> (bool, bool, bool, bool) {
        match self {
            Anchor::TopLeft => (true, false, true, false),
            Anchor::Top => (true, false, false, false),
            Anchor::TopRight => (true, false, false, true),
            Anchor::Left => (false, false, true, false),
            Anchor::Center => (false, false, false, false),
            Anchor::Right => (false, false, false, true),
            Anchor::BottomLeft => (false, true, true, false),
            Anchor::Bottom => (false, true, false, false),
            Anchor::BottomRight => (false, true, false, true),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            period: Period::Last24Hours,
            tray: true,
            tools: Tool::ALL.to_vec(),
            remote: Vec::new(),
            codex_homes: Vec::new(),
            project_group: Vec::new(),
            widget: Widget::default(),
            legacy_window: None,
        }
    }
}

impl Default for Widget {
    fn default() -> Self {
        Widget {
            layer: Layer::Bottom,
            anchor: Anchor::TopRight,
            margin: 16,
            output: String::new(),
            width: 380,
            rows: 5,
            sessions: 5,
            sections: Sections::default(),
        }
    }
}

#[derive(Debug)]
pub struct Error(String);

impl std::error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn path() -> PathBuf {
    paths::config_home().join("toktop").join("config.toml")
}

/// Optional user stylesheet, loaded over the built-in style.
pub fn style_path() -> PathBuf {
    paths::config_home().join("toktop").join("style.css")
}

impl Config {
    /// Parses a config file's contents.
    pub fn parse(text: &str) -> Result<Config, Error> {
        let config: Config = toml::from_str(text).map_err(|error| Error(error.to_string()))?;
        for pattern in config.codex_homes.iter().chain(
            config
                .project_group
                .iter()
                .flat_map(|group| &group.patterns),
        ) {
            glob::Pattern::new(pattern)
                .map_err(|error| Error(format!("invalid glob `{pattern}`: {error}")))?;
        }
        for group in &config.project_group {
            if group.name.trim().is_empty() || group.patterns.is_empty() {
                return Err(Error(
                    "project groups need a name and at least one match pattern".into(),
                ));
            }
        }
        Ok(config)
    }

    /// Reads `path`. A missing file yields defaults; an unreadable or
    /// invalid one yields an error, and the caller must not overwrite it.
    pub fn load(path: &Path) -> Result<Config, Error> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(error) => Err(Error(error.to_string())),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        crate::store::write_atomic(path, text.as_bytes())
    }
}

mod period_key {
    use super::Period;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(period: &Period, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(period.key())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Period, D::Error> {
        let key = String::deserialize(deserializer)?;
        Period::from_key(&key).ok_or_else(|| {
            D::Error::custom(format!(
                "unknown period `{key}`, expected one of 24h, 7d, 30d, all"
            ))
        })
    }
}

mod tool_keys {
    use super::Tool;
    use serde::{Deserialize, Deserializer, Serializer, de::Error, ser::SerializeSeq};

    pub fn serialize<S: Serializer>(tools: &[Tool], serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(tools.len()))?;
        for tool in tools {
            seq.serialize_element(tool.key())?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Tool>, D::Error> {
        let mut tools = Vec::new();
        for key in Vec::<String>::deserialize(deserializer)? {
            let tool = Tool::from_key(&key).ok_or_else(|| {
                D::Error::custom(format!(
                    "unknown tool `{key}`, expected claude-code, codex, antigravity, pi, kimi-code or dsh"
                ))
            })?;
            if !tools.contains(&tool) {
                tools.push(tool);
            }
        }
        Ok(tools)
    }
}
