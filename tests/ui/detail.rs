//! Date-scoped detail selection and navigation targets, without GTK setup.

use super::*;
use toktop::aggregate::{DayDetail, Row, SessionRow};
use toktop::model::Tokens;

#[test]
fn segmented_bar_stays_inside_its_width() {
    let parts = [0.01, 0.01, 20.0, 0.01];
    for width in [300.0, 5.0] {
        let end = chart::segments(&parts, width)
            .map(|(_, x, part)| x + part)
            .fold(0.0, f64::max);
        assert!(
            end <= width + 1e-9,
            "segments of a {width}px bar end at {end}"
        );
    }
    assert_eq!(chart::segments(&[0.0; 4], 300.0).count(), 0);
}

#[test]
fn day_drilldowns_keep_the_date_usage_and_share() {
    let date = NaiveDate::from_ymd_opt(2026, 10, 10).unwrap();
    let usage = |input| Usage {
        tokens: Tokens {
            input,
            ..Tokens::default()
        },
        ..Usage::default()
    };
    let split = Split {
        models: vec![("gpt-6.1-sol".into(), 25)],
        projects: vec![("/work/app".into(), 25)],
        ..Split::default()
    };
    let row = |key: &str, input| Row {
        key: key.into(),
        label: key.into(),
        usage: usage(input),
        split: split.clone(),
        daily_speed: Vec::new(),
    };
    let session = |input| SessionRow {
        tool: Tool::Codex,
        id: "session".into(),
        title: "Session title".into(),
        project: Some("/work/app".into()),
        first_activity: 0,
        last_activity: 1000,
        usage: usage(input),
        models: vec![("gpt-6.1-sol".into(), input)],
        subagents: 0,
    };
    let mut summary = Summary {
        total: usage(200),
        by_model: vec![row("gpt-6.1-sol", 100)],
        by_project: vec![row("/work/app", 100)],
        sessions: vec![session(100)],
        daily: vec![DayRow {
            date,
            usage: usage(50),
            split: split.clone(),
        }],
        day_detail: Some(DayDetail {
            date,
            by_model: vec![row("gpt-6.1-sol", 25)],
            by_project: vec![row("/work/app", 25)],
            sessions: vec![session(25)],
        }),
        ..Summary::default()
    };
    let day = Info::find(&Selection::Day(date), &summary).unwrap();
    assert_eq!(
        day.lists
            .iter()
            .map(|(title, _)| *title)
            .collect::<Vec<_>>(),
        ["MODELS", "PROJECTS", "TOOLS"]
    );
    assert_eq!(
        day.lists[0].1[0].selection,
        Some(Selection::DayModel(date, "gpt-6.1-sol".into()))
    );
    assert_eq!(
        day.lists[1].1[0].selection,
        Some(Selection::DayProject(date, "/work/app".into()))
    );
    assert!(day.lists[2].1.is_empty());
    assert_eq!(
        day.sessions[0].selection,
        Some(Selection::DaySession(date, Tool::Codex, "session".into()))
    );
    assert!(
        Info::find(&Selection::Model("gpt-6.1-sol".into()), &summary)
            .unwrap()
            .sessions
            .is_empty()
    );
    let session_selection = Selection::DaySession(date, Tool::Codex, "session".into());
    let session_info = Info::find(&session_selection, &summary).unwrap();
    assert_eq!(session_info.kind, "SESSION · OCT 10, 2026");
    assert_eq!(session_info.usage.tokens.input, 25);
    assert_eq!(
        session_info.lists[0].1[0].selection,
        Some(Selection::DayModel(date, "gpt-6.1-sol".into()))
    );
    assert_eq!(
        Info::find(&Selection::Session(Tool::Codex, "session".into()), &summary)
            .unwrap()
            .usage
            .tokens
            .input,
        100
    );

    for (selection, regular, kind) in [
        (
            Selection::DayModel(date, "gpt-6.1-sol".into()),
            Selection::Model("gpt-6.1-sol".into()),
            "MODEL",
        ),
        (
            Selection::DayProject(date, "/work/app".into()),
            Selection::Project("/work/app".into()),
            "PROJECT",
        ),
    ] {
        let info = Info::find(&selection, &summary).unwrap();
        assert_eq!(info.kind, format!("{kind} · OCT 10, 2026"));
        assert_eq!(info.usage.tokens.input, 25);
        assert_eq!(
            info.tiles.iter().find(|tile| tile.0 == "SHARE").unwrap().1,
            "50%"
        );
        assert!(info.daily_speed.is_empty());
        assert_eq!(
            Info::find(&regular, &summary).unwrap().usage.tokens.input,
            100
        );
    }

    // An in-flight reply for a different day must never supply this card.
    summary.day_detail.as_mut().unwrap().date = date + chrono::Days::new(1);
    assert!(Info::find(&Selection::DayModel(date, "gpt-6.1-sol".into()), &summary).is_none());
    assert!(Info::find(&Selection::DayProject(date, "/work/app".into()), &summary).is_none());
    assert!(Info::find(&session_selection, &summary).is_none());
}
