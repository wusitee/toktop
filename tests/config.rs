//! Settings file contracts.

use toktop::aggregate::Period;
use toktop::config::{Config, Window};
use toktop::model::Tool;

#[test]
fn missing_and_partial_files_use_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("toktop/config.toml");
    assert_eq!(Config::load(&path).unwrap(), Config::default());

    let partial = Config::parse("period = \"7d\"\n[window]\nwidth = 600\n").unwrap();
    assert_eq!(partial.period, Period::Last7Days);
    assert_eq!(partial.window.width, 600);
    assert_eq!(partial.window.height, Config::default().window.height);
    assert_eq!(partial.tools, Tool::ALL);
    assert!(partial.tray);
}

#[test]
fn saved_settings_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("toktop/config.toml");
    let config = Config {
        period: Period::ThisMonth,
        tray: false,
        tools: vec![Tool::Codex, Tool::ClaudeCode],
        window: Window {
            maximized: true,
            ..Window::default()
        },
    };
    config.save(&path).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("period = \"month\""), "{text}");
    assert!(
        text.contains("tools = [\"codex\", \"claude-code\"]"),
        "{text}"
    );
    assert_eq!(Config::load(&path).unwrap(), config);
}

#[test]
fn invalid_files_are_errors() {
    for text in [
        "period = \"yesterday\"",
        "tools = [\"cursor\"]",
        "tray = \"yes\"",
        "unknown = 1",
        "period = ",
    ] {
        assert!(Config::parse(text).is_err(), "{text}");
    }
    // Duplicate tools are not an error.
    let config = Config::parse("tools = [\"codex\", \"codex\"]").unwrap();
    assert_eq!(config.tools, [Tool::Codex]);
}
