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
    pub window: Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Window {
    pub width: i32,
    pub height: i32,
    pub maximized: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            period: Period::Today,
            tray: true,
            tools: Tool::ALL.to_vec(),
            window: Window::default(),
        }
    }
}

impl Default for Window {
    fn default() -> Self {
        Window {
            width: 420,
            height: 680,
            maximized: false,
        }
    }
}

#[derive(Debug)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub fn path() -> PathBuf {
    paths::config_home().join("toktop").join("config.toml")
}

impl Config {
    /// Parses a config file's contents.
    pub fn parse(text: &str) -> Result<Config, Error> {
        toml::from_str(text).map_err(|error| Error(error.to_string()))
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
                "unknown period `{key}`, expected one of today, week, 7d, month, 30d, all"
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
                    "unknown tool `{key}`, expected claude-code, codex or antigravity"
                ))
            })?;
            if !tools.contains(&tool) {
                tools.push(tool);
            }
        }
        Ok(tools)
    }
}
