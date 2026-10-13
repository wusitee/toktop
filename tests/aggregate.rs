//! Pricing and aggregation contracts.

use chrono::{Local, NaiveDate, TimeZone};
use std::collections::HashMap;
use toktop::aggregate::{self, Period};
use toktop::model::{Event, Session, Tokens, Tool};
use toktop::pricing::{Price, PriceTable, Tier};
use toktop::store::Store;

const LITELLM: &str = r#"{
    "sample_spec": {"input_cost_per_token": "doc", "mode": "one of chat, embedding"},
    "claude-opus-5-5": {"mode": "chat", "input_cost_per_token": 4e-06, "output_cost_per_token": 2e-05,
        "cache_read_input_token_cost": 2e-07, "cache_creation_input_token_cost": 5e-06,
        "cache_creation_input_token_cost_above_1hr": 8e-06},
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
    };
    let tokens = Tokens {
        input: 100,
        output: 10,
        cache_read: 500,
        cache_write: 300,
        cache_write_1h: 100,
        reasoning: 5,
    };
    let expected = 100.0 * 4e-06 + 15.0 * 2e-05 + 500.0 * 2e-07 + 200.0 * 5e-06 + 100.0 * 8e-06;
    assert!((price.cost(&tokens) - expected).abs() < 1e-12);

    // Above the threshold, tier rates apply; missing ones fall back to base rates.
    let long = Tokens {
        input: 2000,
        cache_write: 300,
        cache_write_1h: 100,
        ..Tokens::default()
    };
    let expected = 2000.0 * 8e-06 + 200.0 * 1e-05 + 100.0 * 8e-06;
    assert!((price.cost(&long) - expected).abs() < 1e-12);
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
        models: vec![model.to_owned()],
        events: events
            .into_iter()
            .map(|(ts, input, key)| Event {
                ts,
                model: 0,
                tokens: Tokens {
                    input,
                    ..Tokens::default()
                },
                key: key.map(Into::into),
                input_is_split: false,
                cache_write_1h_raw: 0,
            })
            .collect(),
        limits: None,
    }
}

#[test]
fn summary_filters_by_period_and_dedups_across_files() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap(); // a Wednesday
    let now = ms(today, 12);
    let monday = NaiveDate::from_ymd_opt(2026, 10, 12).unwrap();
    let last_month = NaiveDate::from_ymd_opt(2026, 9, 30).unwrap();

    let mut store = Store::default();
    store.insert_session(
        "/a.jsonl".into(),
        Tool::ClaudeCode,
        session(
            "a",
            "/work/app",
            "claude-opus-5-5",
            vec![(ms(today, 9), 100, Some("m1")), (ms(monday, 9), 20, None)],
        ),
    );
    // A resumed copy of the same response in another file, with more
    // complete counts: counted once, at the most complete values.
    let mut resumed = session(
        "b",
        "/work/other",
        "claude-opus-5-5",
        vec![(ms(today, 9), 100, Some("m1"))],
    );
    resumed.events[0].tokens.output = 50;
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

    let today_summary = aggregate::summarize(&store, &prices, Period::Today, now);
    assert_eq!(today_summary.total.tokens.input, 103);
    assert_eq!(today_summary.total.tokens.output, 50);
    assert!((today_summary.total.cost - 1.0).abs() < 1e-9);
    assert_eq!(today_summary.unpriced, ["gpt-6.1-sol"]);
    assert_eq!(today_summary.by_project[0].label, "app");
    assert_eq!(today_summary.by_project.len(), 1);
    assert_eq!(today_summary.sessions[0].title, "Indexed title");
    assert!((today_summary.rate_per_min - 3.0 / 5.0).abs() < 1e-9);

    let week = aggregate::summarize(&store, &prices, Period::ThisWeek, now);
    assert_eq!(week.total.tokens.input, 123);
    let all = aggregate::summarize(&store, &prices, Period::AllTime, now);
    assert_eq!(all.total.tokens.input, 130);
    assert_eq!(all.daily.len(), 3);
    assert_eq!(all.daily[0].date, last_month);
}

#[test]
fn claude_cross_file_merge_preserves_input_and_raw_cache_splits() {
    let dir = tempfile::tempdir().unwrap();
    let copies = [
        concat!(
            r#"{"type":"assistant","timestamp":"2026-10-01T10:00:01Z","message":{"id":"input","model":"claude-opus-5-5","usage":{"input_tokens":42494}}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-10-01T10:00:01Z","message":{"id":"cache","model":"claude-opus-5-5","usage":{"cache_creation_input_tokens":20,"cache_creation":{"ephemeral_1h_input_tokens":30}}}}"#,
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
