//! Settings file contracts.

use toktop::aggregate::Period;
use toktop::config::{Anchor, Config, Layer, Sections, Widget};
use toktop::model::Tool;

#[test]
fn missing_and_partial_files_use_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("toktop/config.toml");
    assert_eq!(Config::load(&path).unwrap(), Config::default());

    // The `[window]` table of earlier versions is accepted and ignored.
    let partial = Config::parse(
        "period = \"7d\"\n[window]\nwidth = 600\n[widget]\nanchor = \"bottom-left\"\n",
    )
    .unwrap();
    assert_eq!(partial.period, Period::Last7Days);
    assert_eq!(partial.widget.anchor, Anchor::BottomLeft);
    assert_eq!(partial.widget.layer, Layer::Bottom);
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
        widget: Widget {
            layer: Layer::Overlay,
            output: "DP-1".into(),
            sections: Sections {
                activity: false,
                ..Sections::default()
            },
            ..Widget::default()
        },
        legacy_window: None,
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
fn sessions_are_hidden_by_their_switch_or_a_zero_count() {
    let mut widget = Config::parse("[widget]\nsessions = 0").unwrap().widget;
    assert_eq!(widget.shown_sessions(), 0);
    widget.show_sessions(false);
    assert_eq!(widget.shown_sessions(), 0);
    // Changing the count keeps them hidden.
    widget.set_session_count(3);
    assert_eq!(widget.shown_sessions(), 0);
    widget.show_sessions(true);
    assert_eq!(widget.shown_sessions(), 3);
    widget.sessions = 7;
    widget.show_sessions(false);
    assert_eq!((widget.shown_sessions(), widget.sessions), (0, 7));
}

#[test]
fn invalid_files_are_errors() {
    for text in [
        "period = \"yesterday\"",
        "tools = [\"cursor\"]",
        "tray = \"yes\"",
        "unknown = 1",
        "[widget]\nlayer = \"desktop\"",
        "[widget.sections]\nheatmap = false",
        "period = ",
    ] {
        assert!(Config::parse(text).is_err(), "{text}");
    }
    // Duplicate tools are not an error.
    let config = Config::parse("tools = [\"codex\", \"codex\"]").unwrap();
    assert_eq!(config.tools, [Tool::Codex]);
}
