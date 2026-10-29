//! Pricing and aggregation contracts.

use chrono::{Local, NaiveDate, TimeZone};
use std::collections::HashMap;
use toktop::aggregate::{self, Period};
use toktop::model::{Event, ServiceTier, Session, Tokens, Tool};
use toktop::pricing::{Price, PriceTable, Tier};
use toktop::store::Store;

const LITELLM: &str = r#"{
    "sample_spec": {"input_cost_per_token": "doc", "mode": "one of chat, embedding"},
    "claude-opus-5-5": {"mode": "chat", "input_cost_per_token": 4e-06, "output_cost_per_token": 2e-05,
        "cache_read_input_token_cost": 2e-07, "cache_creation_input_token_cost": 5e-06,
        "cache_creation_input_token_cost_above_1hr": 8e-06},
    "claude-sonnet-4-6": {"input_cost_per_token": 3e-06, "output_cost_per_token": 1.5e-05},
    "gemini-3.1-pro-preview": {"input_cost_per_token": 2e-06, "output_cost_per_token": 1.2e-05},
    "gpt-5.6-sol": {"input_cost_per_token": 5e-06, "output_cost_per_token": 3e-05},
    "azure/gpt-6.1-sol": {"mode": "chat", "input_cost_per_token": 9e-06, "output_cost_per_token": 9e-05},
    "openai/gpt-6.1-sol": {"mode": "chat", "input_cost_per_token": 2e-06, "output_cost_per_token": 1e-05,
        "input_cost_per_token_above_272k_tokens": 4e-06, "output_cost_per_token_above_272k_tokens": 2e-05},
    "text-embedding-x": {"mode": "embedding", "input_cost_per_token": 1e-07, "output_cost_per_token": 0}
}"#;

#[test]
fn litellm_table_resolves_tool_model_ids() {
    let table = PriceTable::from_litellm(LITELLM.as_bytes()).unwrap();
    assert!(table.lookup("text-embedding-x").is_none());
    // First-party provider entries win over resellers for bare names, but a
    // provider-qualified id keeps its own price.
    assert_eq!(table.lookup("gpt-6.1-sol").unwrap().input, 2e-06);
    assert_eq!(table.lookup("azure/gpt-6.1-sol").unwrap().input, 9e-06);
    // Effort and variant suffixes fall back to the catalog name.
    assert_eq!(
        table.lookup("claude-opus-5-5-thinking").unwrap().input,
        4e-06
    );
    assert!(table.lookup("claude-opus").is_none());
    assert!(table.lookup("codex-auto-review").is_none());
    assert!(table.lookup("gemini-3-flash").is_none());
    for (alias, target) in [
        ("MODEL_PLACEHOLDER_M35", "claude-sonnet-4-6"),
        ("claude-sonnet-4.6", "claude-sonnet-4-6"),
        ("anthropic/claude-sonnet-4.6-thinking", "claude-sonnet-4-6"),
        ("MODEL_PLACEHOLDER_M35-thinking", "claude-sonnet-4-6"),
        ("gemini-pro-default", "gemini-3.1-pro-preview"),
        ("MODEL_PLACEHOLDER_M37", "gemini-3.1-pro-preview"),
        ("openai/gpt-daybreak-blue-latest", "gpt-5.6-sol"),
    ] {
        assert_eq!(table.lookup(alias), table.lookup(target), "{alias}");
    }
}

#[test]
fn cost_applies_cache_and_long_context_rates() {
    let price = Price {
        input: 4e-06,
        output: 2e-05,
        cache_read: Some(2e-07),
        cache_write: Some(5e-06),
        cache_write_1h: Some(8e-06),
        tier: Some(Tier {
            threshold: 1000,
            input: 8e-06,
            output: 4e-05,
            cache_read: None,
            cache_write: Some(1e-05),
            cache_write_1h: None,
        }),
        ..Price::default()
    };
    let tokens = Tokens {
        input: 100,
        output: 10,
        cache_read: 500,
        cache_write: 300,
        cache_write_1h: 100,
        reasoning: 5,
    };
    // Cost by kind; reasoning is billed as output, both cache writes as cache write.
    let cost = price.cost(&tokens, ServiceTier::Standard);
    let parts = [
        (cost.input, 100.0 * 4e-06),
        (cost.cache_read, 500.0 * 2e-07),
        (cost.cache_write, 200.0 * 5e-06 + 100.0 * 8e-06),
        (cost.output, 15.0 * 2e-05),
    ];
    for (part, expected) in parts {
        assert!((part - expected).abs() < 1e-12);
    }
    let expected: f64 = parts.iter().map(|(_, expected)| expected).sum();
    assert!((price.cost(&tokens, ServiceTier::Priority).total() - expected * 2.0).abs() < 1e-12);

    // Above the threshold, tier rates apply; missing ones fall back to base rates.
    let long = Tokens {
        input: 2000,
        cache_write: 300,
        cache_write_1h: 100,
        ..Tokens::default()
    };
    let expected = 2000.0 * 8e-06 + 200.0 * 1e-05 + 100.0 * 8e-06;
    assert!((price.cost(&long, ServiceTier::Standard).total() - expected).abs() < 1e-12);
    assert!((price.cost(&long, ServiceTier::Priority).total() - expected * 2.0).abs() < 1e-12);
}

fn ms(date: NaiveDate, hour: u32) -> i64 {
    Local
        .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
        .earliest()
        .unwrap()
        .timestamp_millis()
}

fn session(id: &str, project: &str, model: &str, events: Vec<(i64, u64, Option<&str>)>) -> Session {
    Session {
        id: id.to_owned(),
        project: Some(project.to_owned()),
        title: None,
        prompt: None,
        models: vec![model.to_owned()],
        events: events
            .into_iter()
            .map(|(ts, input, key)| Event {
                effort: None,
                ts,
                model: 0,
                tier: ServiceTier::Standard,
                tokens: Tokens {
                    input,
                    ..Tokens::default()
                },
                key: key.map(Into::into),
                input_is_split: false,
                cache_write_1h_raw: 0,
                duration_ms: 0,
            })
            .collect(),
        parent: None,
        limits: None,
        codex_home: None,
        limit_history: Vec::new(),
    }
}

#[test]
fn remote_hosts_keep_sessions_projects_and_duplicates_separate() {
    let now = ms(NaiveDate::from_ymd_opt(2026, 10, 14).unwrap(), 12);
    let mut store = Store::with_tools(vec![Tool::Codex]);
    let parent = session("same", "/work/app", "model", vec![(now, 10, Some("key"))]);
    let mut child = session(
        "child",
        "/work/app",
        "model",
        vec![(now, 5, Some("child-key"))],
    );
    child.parent = Some("same".into());
    store.insert_session("/local".into(), Tool::Codex, parent.clone());
    for host in ["one", "two"] {
        store.replace_remote(
            host,
            vec![
                (Tool::Codex, parent.clone()),
                (Tool::Codex, parent.clone()),
                (Tool::Codex, child.clone()),
            ],
        );
    }
    let summary = aggregate::summarize(&store, &PriceTable::default(), Period::AllTime, now);
    assert_eq!(summary.total.tokens.input, 40);
    assert_eq!(summary.sessions.len(), 3);
    assert_eq!(
        summary
            .sessions
            .iter()
            .filter(|row| row.subagents == 1)
            .count(),
        2
    );
    assert_eq!(summary.by_project.len(), 3);
    assert!(
        summary
            .by_project
            .iter()
            .any(|row| row.label.contains("[one]"))
    );
    assert!(
        summary
            .sessions
            .iter()
            .any(|row| row.title.starts_with("[two]"))
    );

    store.replace_remote("one", vec![]);
    let summary = aggregate::summarize(&store, &PriceTable::default(), Period::AllTime, now);
    assert_eq!(summary.total.tokens.input, 25);
    assert_eq!(summary.sessions.len(), 2);
}

#[test]
fn summary_counts_free_auto_review_without_unpriced_warning() {
    let now = ms(NaiveDate::from_ymd_opt(2026, 10, 14).unwrap(), 12);
    let mut store = Store::default();
    for model in ["codex-auto-review", "unknown-model"] {
        store.insert_session(
            format!("/{model}.jsonl").into(),
            Tool::Codex,
            session(model, "/work/app", model, vec![(now, 10, None)]),
        );
    }
    let prices = PriceTable::from_litellm(LITELLM.as_bytes()).unwrap();
    let summary = aggregate::summarize(&store, &prices, Period::Last24Hours, now);
    assert_eq!(summary.unpriced, ["unknown-model"]);
    assert_eq!(summary.total.cost.total(), 0.0);
    assert_eq!(summary.total.tokens.input, 20);
    assert_eq!(summary.total.requests, 2);
}

#[test]
fn summary_filters_by_period_and_dedups_across_files() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
    let now = ms(today, 12);
    let monday = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
    let last_month = NaiveDate::from_ymd_opt(2026, 9, 30).unwrap();

    let mut store = Store::default();
    let mut first = session(
        "a",
        "/work/app",
        "claude-opus-5-5",
        vec![(ms(today, 9), 100, Some("m1")), (ms(monday, 9), 20, None)],
    );
    first.events[0].duration_ms = 1000;
    store.insert_session("/a.jsonl".into(), Tool::ClaudeCode, first);
    // A resumed copy of the same response in another file, with more
    // complete counts: counted once, at the most complete values.
    let mut resumed = session(
        "b",
        "/work/other",
        "claude-opus-5-5",
        vec![(ms(today, 9), 100, Some("m1"))],
    );
    resumed.events[0].tokens.output = 50;
    resumed.events[0].duration_ms = 4000;
    store.insert_session("/b.jsonl".into(), Tool::ClaudeCode, resumed);
    store.insert_session(
        "/c.jsonl".into(),
        Tool::Codex,
        session(
            "c",
            "/work/app/",
            "gpt-6.1-sol",
            vec![(ms(last_month, 9), 7, None), (now - 60_000, 3, None)],
        ),
    );
    store.titles.insert("c".into(), "Indexed title".into());

    let prices = PriceTable::from_prices(HashMap::from([(
        "claude-opus-5-5".to_owned(),
        Price {
            input: 0.01,
            ..Price::default()
        },
    )]));

    let today_summary = aggregate::summarize(&store, &prices, Period::Last24Hours, now);
    assert_eq!(today_summary.total.tokens.input, 103);
    assert_eq!(today_summary.total.tokens.output, 50);
    // The most complete copy's duration goes with its counts.
    assert_eq!(today_summary.total.speed(), Some(12.5));
    assert!((today_summary.total.cost.total() - 1.0).abs() < 1e-9);
    assert!(today_summary.prices_available);
    assert_eq!(today_summary.unpriced, ["gpt-6.1-sol"]);
    let unavailable =
        aggregate::summarize(&store, &PriceTable::default(), Period::Last24Hours, now);
    assert!(!unavailable.prices_available);
    assert!(unavailable.unpriced.is_empty());
    assert_eq!(unavailable.total.tokens, today_summary.total.tokens);
    assert_eq!(today_summary.by_project[0].label, "app");
    assert_eq!(today_summary.by_project.len(), 1);
    assert_eq!(today_summary.sessions[0].title, "Indexed title");
    assert!((today_summary.rate_per_min - 3.0 / 5.0).abs() < 1e-9);
    assert_eq!(today_summary.recent_speed, Some(12.5));

    let week = aggregate::summarize(&store, &prices, Period::Last7Days, now);
    assert_eq!(week.total.tokens.input, 123);
    let all = aggregate::summarize(&store, &prices, Period::AllTime, now);
    assert_eq!(all.total.tokens.input, 130);
    assert_eq!(all.daily.len(), 3);
    assert_eq!(all.daily[0].date, last_month);

    // An open day's breakdown is independent of the main period and uses
    // the same merged counts, prices and durations as the regular rows.
    assert!(today_summary.day_detail.is_none());
    let detailed =
        aggregate::summarize_with_day(&store, &prices, Period::Last24Hours, now, Some(today));
    let detail = detailed.day_detail.unwrap();
    assert_eq!(detail.date, today);
    for (actual, expected) in detail.by_model.iter().zip(&today_summary.by_model) {
        assert_eq!(actual.key, expected.key);
        assert_eq!(actual.usage, expected.usage);
        assert_eq!(actual.split, expected.split);
        assert!(actual.daily_speed.is_empty());
    }
    assert_eq!(detail.by_model.len(), today_summary.by_model.len());
    assert_eq!(detail.by_project, today_summary.by_project);
    assert_eq!(detail.sessions, today_summary.sessions);
    assert_eq!(detailed.total, today_summary.total);

    let historical =
        aggregate::summarize_with_day(&store, &prices, Period::Last24Hours, now, Some(last_month));
    let detail = historical.day_detail.unwrap();
    assert_eq!(historical.total, today_summary.total);
    assert_eq!(detail.by_model.len(), 1);
    assert_eq!(detail.by_model[0].key, "gpt-6.1-sol");
    assert_eq!(detail.by_model[0].usage.tokens.input, 7);
    assert_eq!(detail.by_project[0].key, "/work/app");
    assert_eq!(detail.by_project[0].usage.requests, 1);
    assert_eq!(detail.sessions.len(), 1);
    let row = &detail.sessions[0];
    assert_eq!(row.id, "c");
    assert_eq!(row.title, "Indexed title");
    assert_eq!(row.usage.tokens.input, 7);
    assert_eq!(row.first_activity, ms(last_month, 9));
    assert_eq!(row.last_activity, ms(last_month, 9));

    let empty = aggregate::summarize_with_day(
        &store,
        &prices,
        Period::Last24Hours,
        now,
        Some(today + chrono::Days::new(1)),
    );
    let detail = empty.day_detail.unwrap();
    assert!(detail.by_model.is_empty());
    assert!(detail.by_project.is_empty());
    assert!(detail.sessions.is_empty());

    // Yesterday's request just inside 24 hours is included; the one just
    // outside is not, regardless of local midnight.
    let cutoff = now - 24 * 60 * 60 * 1000;
    store.insert_session(
        "/boundary.jsonl".into(),
        Tool::Codex,
        session(
            "boundary",
            "/work/app",
            "gpt-6.1-sol",
            vec![(cutoff + 1, 5, None), (cutoff - 1, 11, None)],
        ),
    );
    let rolling = aggregate::summarize(&store, &prices, Period::Last24Hours, now);
    assert_eq!(
        rolling.total.tokens.input,
        today_summary.total.tokens.input + 5
    );
    assert_eq!(rolling.total.requests, today_summary.total.requests + 1);
}

#[test]
fn subagents_group_under_their_root_session_with_speed_and_streak() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
    let now = ms(today, 12);
    let mut store = Store::default();
    let timed = |mut session: Session, output: u64, duration_ms: u32| {
        session.events[0].tokens.output = output;
        session.events[0].duration_ms = duration_ms;
        session
    };
    let root = timed(
        session(
            "root",
            "/work/app",
            "gpt-6.1-sol",
            vec![(ms(today, 9), 10, None)],
        ),
        100,
        2000,
    );
    let mut child = timed(
        session(
            "child",
            "/work/app",
            "gpt-6-luna",
            vec![(ms(today, 10), 10, None)],
        ),
        300,
        2000,
    );
    child.parent = Some("root".into());
    // A nested subagent, and an untimed request that does not affect speed.
    let mut grandchild = session(
        "grandchild",
        "/work/app",
        "gpt-6-luna",
        vec![(ms(today, 11), 10, None)],
    );
    grandchild.parent = Some("child".into());
    grandchild.events[0].tokens.output = 5000;
    store.insert_session("/root.jsonl".into(), Tool::Codex, root);
    store.insert_session("/child.jsonl".into(), Tool::Codex, child);
    store.insert_session("/grandchild.jsonl".into(), Tool::Codex, grandchild);
    let yesterday = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
    store.insert_session(
        "/old.jsonl".into(),
        Tool::ClaudeCode,
        session(
            "old",
            "/work/app",
            "claude-opus-5-5",
            vec![
                (ms(yesterday, 9), 1, None),
                (ms(today - chrono::Days::new(3), 9), 1, None),
            ],
        ),
    );

    let summary = aggregate::summarize_with_day(
        &store,
        &PriceTable::default(),
        Period::Last24Hours,
        now,
        Some(today),
    );
    assert_eq!(
        summary.day_detail.as_ref().unwrap().sessions,
        summary.sessions
    );
    assert_eq!(summary.sessions.len(), 1);
    let row = &summary.sessions[0];
    assert_eq!((row.id.as_str(), row.subagents), ("root", 2));
    assert_eq!(
        row.models,
        [
            ("gpt-6-luna".to_owned(), 5320),
            ("gpt-6.1-sol".to_owned(), 110)
        ]
    );
    assert_eq!(row.usage.requests, 3);
    assert_eq!(
        (row.first_activity, row.last_activity),
        (ms(today, 9), ms(today, 11))
    );
    // Splits count subagents as part of their root session.
    let luna = summary
        .by_model
        .iter()
        .find(|row| row.key == "gpt-6-luna")
        .unwrap();
    assert_eq!(luna.split.sessions, 1);
    assert_eq!(luna.split.tools, [(Tool::Codex, 5320)]);
    assert_eq!(luna.split.projects, [("/work/app".to_owned(), 5320)]);
    // Only the timed request counts toward the model's daily speed.
    assert_eq!(luna.daily_speed, [(today, 150.0)]);
    let day = summary.daily.last().unwrap();
    assert_eq!(day.date, today);
    assert_eq!((day.split.sessions, day.split.models.len()), (1, 2));
    assert_eq!(summary.total.speed(), Some(100.0));
    assert_eq!(summary.recent_speed, Some(100.0));
    assert_eq!(summary.streak_days, 2);

    // Recent speed covers only the latest timed requests, whatever the period.
    let mut recent = Store::default();
    // Events newest first, so file order differs from time order.
    let mut events: Vec<_> = (0..=aggregate::RECENT_REQUESTS as i64)
        .rev()
        .map(|i| (ms(yesterday, 9) + i * 60_000, 0, None))
        .collect();
    events.push((now + 60_000, 0, None));
    let mut fast = session("fast", "/work/app", "gpt-6.1-sol", events);
    for (i, event) in fast.events.iter_mut().enumerate() {
        // The oldest and a future request are slow and fall outside; the
        // rest alternate 100 and 300 tok/s.
        let slow = i >= aggregate::RECENT_REQUESTS;
        (event.tokens.output, event.duration_ms) = match (slow, i % 2) {
            (true, _) => (1, 1000),
            (false, 0) => (100, 1000),
            (false, _) => (900, 3000),
        };
    }
    recent.insert_session("/fast.jsonl".into(), Tool::Codex, fast);
    let summary = aggregate::summarize(&recent, &PriceTable::default(), Period::Last24Hours, now);
    // A ratio of sums, not the 200 tok/s mean of request speeds.
    assert_eq!(summary.recent_speed, Some(250.0));

    // A row whose subagent alone has usage in the period keeps the parent's
    // prompt and project.
    let mut store = Store::default();
    let mut parent = session(
        "p",
        "/work/parent",
        "gpt-6.1-sol",
        vec![(ms(yesterday, 9), 1, None)],
    );
    parent.prompt = Some("Parent task".into());
    let mut child = session(
        "c",
        "/work/child",
        "gpt-6.1-sol",
        vec![(ms(today, 9), 1, None)],
    );
    child.parent = Some("p".into());
    child.prompt = Some("Child subtask".into());
    store.insert_session("/p.jsonl".into(), Tool::Codex, parent);
    store.insert_session("/c.jsonl".into(), Tool::Codex, child);
    let summary = aggregate::summarize_with_day(
        &store,
        &PriceTable::default(),
        Period::AllTime,
        now,
        Some(today),
    );
    let row = &summary.day_detail.as_ref().unwrap().sessions[0];
    assert_eq!((row.title.as_str(), row.subagents), ("Parent task", 1));
    assert_eq!(row.project.as_deref(), Some("/work/parent"));

    // Without usage today, a streak still runs through yesterday.
    let day = |date| aggregate::DayRow {
        date,
        usage: aggregate::Usage::default(),
        split: aggregate::Split::default(),
    };
    let daily = [day(today - chrono::Days::new(2)), day(yesterday)];
    assert_eq!(aggregate::streak(&daily, today), 2);
    assert_eq!(aggregate::streak(&daily[..1], today), 0);
}

#[test]
fn claude_cross_file_merge_preserves_input_and_raw_cache_splits() {
    let dir = tempfile::tempdir().unwrap();
    let copies = [
        concat!(
            r#"{"type":"assistant","timestamp":"2026-10-01T10:00:01Z","message":{"id":"input","model":"claude-opus-5-5","usage":{"input_tokens":42494}}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-10-01T10:00:01Z","message":{"id":"cache","model":"claude-opus-5-5","usage":{"cache_creation":{"ephemeral_1h_input_tokens":30}}}}"#,
            "\n",
        ),
        concat!(
            r#"{"type":"assistant","timestamp":"2026-10-01T10:00:01Z","message":{"id":"input","model":"claude-opus-5-5","usage":{"input_tokens":265,"cache_read_input_tokens":42000,"output_tokens":120}}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-10-01T10:00:01Z","message":{"id":"cache","model":"claude-opus-5-5","usage":{"cache_creation_input_tokens":40}}}"#,
            "\n",
        ),
    ];
    // Provenance survives serialization, and merging is independent of file order.
    for order in [[0, 1], [1, 0]] {
        let mut store = Store::default();
        for (index, copy) in order.into_iter().enumerate() {
            let path = dir.path().join(format!("{index}.jsonl"));
            std::fs::write(&path, copies[copy]).unwrap();
            let (session, _) = toktop::parse::parse_full(Tool::ClaudeCode, &path).unwrap();
            let session = serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
            store.insert_session(path, Tool::ClaudeCode, session);
        }
        let summary = aggregate::summarize(&store, &PriceTable::default(), Period::AllTime, 0);
        assert_eq!(summary.total.tokens.input, 265);
        assert_eq!(summary.total.tokens.cache_write_1h, 30);
    }
}

#[test]
fn recent_token_rate_saturates() {
    let now = ms(NaiveDate::from_ymd_opt(2026, 10, 14).unwrap(), 12);
    let mut store = Store::default();
    store.insert_session(
        "/large.jsonl".into(),
        Tool::Codex,
        session(
            "large",
            "/work/app",
            "gpt-6.1-sol",
            vec![(now, u64::MAX, None), (now, 1, None)],
        ),
    );
    let summary = aggregate::summarize(&store, &PriceTable::default(), Period::AllTime, now);
    assert_eq!(summary.rate_per_min, u64::MAX as f64 / 5.0);
}

#[test]
fn projects_keep_full_paths_and_disambiguate_display_labels() {
    let mut store = Store::default();
    for (index, (path, input)) in [
        ("/work/client-a/api", 10),
        ("/work/client-a/api///", 20),
        ("/work/client-b/api", 40),
        ("/other/client-a/api", 80),
        ("/work/web", 160),
        ("/work/api", 320),
    ]
    .into_iter()
    .enumerate()
    {
        store.insert_session(
            format!("/{index}.jsonl").into(),
            Tool::Codex,
            session(
                &index.to_string(),
                path,
                "gpt-6.1-sol",
                vec![(0, input, None)],
            ),
        );
    }
    let summary = aggregate::summarize(&store, &PriceTable::default(), Period::AllTime, 0);
    let projects: HashMap<_, _> = summary
        .by_project
        .iter()
        .map(|row| (row.label.as_str(), row.usage.tokens.input))
        .collect();
    assert_eq!(
        projects,
        HashMap::from([
            ("work/client-a/api", 30),
            ("client-b/api", 40),
            ("other/client-a/api", 80),
            ("web", 160),
            ("work/api", 320),
        ])
    );
    assert_eq!(
        summary
            .by_project
            .iter()
            .find(|row| row.label == "work/client-a/api")
            .unwrap()
            .key,
        "/work/client-a/api"
    );
    assert_eq!(aggregate::project_label("/work/client-a/api/"), "api");
}

#[test]
fn cost_uses_priority_rates_and_doubles_missing_tariffs() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("prices.json");
    // Old reduced caches have neither service-tier fields nor a long-context tier.
    std::fs::write(
        &cache,
        r#"{"prices":{"gpt-legacy":{"input":1,"output":2,
        "cache_read":0.25,"cache_write":null,"cache_write_1h":null}}}"#,
    )
    .unwrap();
    let (legacy, _) = PriceTable::load(&cache).unwrap();
    let legacy = legacy.lookup("gpt-legacy").unwrap();
    let tokens = Tokens {
        input: 10,
        output: 3,
        reasoning: 2,
        cache_read: 20,
        cache_write: 4,
        cache_write_1h: 1,
    };
    assert_eq!(
        legacy.cost(&tokens, ServiceTier::Priority).total(),
        legacy.cost(&tokens, ServiceTier::Standard).total() * 2.0
    );

    let table = PriceTable::from_litellm(
        br#"{
        "gpt-6.1-sol": {"input_cost_per_token": 1, "output_cost_per_token": 2,
            "cache_read_input_token_cost": 0.25,
            "input_cost_per_token_above_272k_tokens": 2,
            "output_cost_per_token_above_272k_tokens": 4,
            "cache_read_input_token_cost_above_272k_tokens": 0.5,
            "input_cost_per_token_priority": 3, "output_cost_per_token_priority": 5,
            "cache_read_input_token_cost_priority": 0.75,
            "input_cost_per_token_above_272k_tokens_priority": 6,
            "output_cost_per_token_above_272k_tokens_priority": 10,
            "cache_read_input_token_cost_above_272k_tokens_priority": 1.5}
    }"#,
    )
    .unwrap();
    // A flat priority tariff remains valid above 272K when no context tier exists.
    let flat = PriceTable::from_litellm(
        br#"{"gpt-4.1": {"input_cost_per_token": 2e-6,
            "output_cost_per_token": 8e-6, "cache_read_input_token_cost": 0.5e-6,
            "input_cost_per_token_priority": 3.5e-6,
            "output_cost_per_token_priority": 14e-6,
            "cache_read_input_token_cost_priority": 0.875e-6}}"#,
    )
    .unwrap();
    let flat = flat.lookup("gpt-4.1").unwrap();
    let long_flat = Tokens {
        input: 300_000,
        output: 1_000,
        cache_read: 50_000,
        ..Tokens::default()
    };
    assert!((flat.cost(&long_flat, ServiceTier::Priority).total() - 1.10775).abs() < 1e-12);

    // Reduced priority tariffs survive disk caching.
    table.save(&cache).unwrap();
    let (table, _) = PriceTable::load(&cache).unwrap();
    let price = *table.lookup("gpt-6.1-sol").unwrap();
    assert_eq!(price.cost(&tokens, ServiceTier::Standard).total(), 29.0);
    assert_eq!(price.cost(&tokens, ServiceTier::Priority).total(), 78.0);
    let boundary = Tokens {
        input: 272_000,
        ..Tokens::default()
    };
    assert_eq!(
        price.cost(&boundary, ServiceTier::Priority).total(),
        272_000.0 * 3.0
    );
    let long = Tokens {
        input: 272_001,
        ..tokens
    };
    assert_eq!(
        price.cost(&long, ServiceTier::Priority).total(),
        272_001.0 * 6.0 + 5.0 * 10.0 + 20.0 * 1.5 + 4.0 * 4.0
    );
    // Missing rates double the corresponding effective standard rate, even
    // above the context threshold; cache writes retain the same premium.
    let partial = Price {
        output_priority: None,
        cache_read_priority: None,
        input_priority_above_272k: None,
        output_priority_above_272k: None,
        cache_read_priority_above_272k: None,
        ..price
    };
    assert_eq!(partial.cost(&tokens, ServiceTier::Priority).total(), 68.0);
    assert_eq!(
        partial.cost(&long, ServiceTier::Priority).total(),
        partial.cost(&long, ServiceTier::Standard).total() * 2.0
    );
}
