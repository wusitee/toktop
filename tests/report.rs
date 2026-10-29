//! Accounting CLI contracts, exercised without a desktop or network access.

use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;
use toktop::{config::Config, model::Tool, pricing::PriceTable, report::Options, store::Store};

fn write_lines(path: &Path, lines: &[Value]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text = lines.iter().map(|line| {
        // Codex timing rows have a fixed prefix in real rollout files.
        if line["type"] == "response_item" {
            format!(r#"{{"timestamp":{},"type":"response_item","payload":{{"type":"message","role":{},"content":[]}}}}"#, line["timestamp"], line["payload"]["role"]) + "\n"
        } else { serde_json::to_string(line).unwrap() + "\n" }
    }).collect::<String>();
    std::fs::write(path, text).unwrap();
}

fn codex(id: &str, project: &str) -> Vec<Value> {
    let limits = |time, used, reset| json!({"timestamp": time, "type":"event_msg", "payload": {"type":"token_count", "rate_limits": {"plan_type":"plus", "primary": {"used_percent": used, "window_minutes":300, "resets_at":reset}}}});
    vec![
        json!({"type":"session_meta", "payload":{"id":id, "cwd":project}}),
        json!({"type":"turn_context", "payload":{"model":"bench-model-1", "effort":"xhigh"}}),
        limits("2026-10-29T10:00:00Z", 23, 1793282400i64),
        json!({"timestamp":"2026-10-29T10:00:00Z", "type":"response_item", "payload":{"type":"message", "role":"user", "content":[]}}),
        json!({"timestamp":"2026-10-29T10:00:02Z", "type":"response_item", "payload":{"type":"message", "role":"assistant", "content":[]}}),
        json!({"timestamp":"2026-10-29T10:00:02Z", "type":"event_msg", "payload":{"type":"token_count", "info":{"total_token_usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":6,"reasoning_output_tokens":2,"total_tokens":16}, "last_token_usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":6,"reasoning_output_tokens":2,"total_tokens":16}}}}),
        // Concurrent observations can lower usage while reset estimates jitter.
        limits("2026-10-29T10:01:00Z", 26, 1793282401i64),
        limits("2026-10-29T10:02:00Z", 25, 1793282400i64),
        limits("2026-10-29T10:03:00Z", 70, 1793282401i64),
        limits("2026-10-29T10:04:00Z", 2, 1793300400i64),
        limits("2026-10-29T11:00:00Z", 99, 1793300400i64),
    ]
}

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        write_lines(
            &dir.path().join(".codex/sessions/rollout-one.jsonl"),
            &codex("one", "/tmp/nwb-codex-one"),
        );
        write_lines(
            &dir.path()
                .join(".codex/archived_sessions/rollout-copy.jsonl"),
            &codex("one", "/tmp/nwb-codex-one"),
        );
        write_lines(
            &dir.path()
                .join("runs/two/codex-home/sessions/rollout-two.jsonl"),
            &codex("two", "/other/project"),
        );
        write_lines(
            &dir.path().join("claude/projects/test/session.jsonl"),
            &[
                json!({"type":"user","timestamp":"2026-10-29T10:00:00Z","sessionId":"claude-one","cwd":"/tmp/nwb-claude-one", "perTurnEffort":"high", "message":{"content":"Hello"}}),
                json!({"type":"assistant","timestamp":"2026-10-29T10:00:01Z","sessionId":"claude-one","message":{"id":"a","model":"unpriced-model-1","usage":{"input_tokens":3,"output_tokens":2}}}),
            ],
        );
        let config = Config::parse("tools = [\"codex\", \"claude-code\"]\ncodex_homes = [\"~/runs/*/codex-home\", \"~/.codex\"]\n[[project_group]]\nname = \"nai-wai-bench\"\nmatch = [\"/tmp/nwb-*\"]\n").unwrap();
        config
            .save(&dir.path().join("config/toktop/config.toml"))
            .unwrap();
        std::fs::write(
            dir.path().join("prices.toml"),
            "[models.\"bench-model-1\"]\ninput = 4.0\ncache_read = 0.4\noutput = 20.0\n",
        )
        .unwrap();
        // Empty caches and --offline must still allow custom prices.
        Self { dir }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_toktop-collector"));
        command
            .env("HOME", self.dir.path())
            .env("XDG_CONFIG_HOME", self.dir.path().join("config"))
            .env("XDG_CACHE_HOME", self.dir.path().join("cache"))
            .env("CODEX_HOME", self.dir.path().join(".codex"))
            .env(
                "TOKTOP_CODEX_HOMES",
                self.dir.path().join("runs/*/codex-home"),
            )
            .env("CLAUDE_CONFIG_DIR", self.dir.path().join("claude"));
        command
    }
    fn report(&self, args: &[&str]) -> Value {
        let output = self
            .command()
            .args(["report", "--offline", "--format=json", "--price-file"])
            .arg(self.dir.path().join("prices.toml"))
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

#[test]
fn collector_reports_filtered_prices_nested_effort_and_quota_per_home() {
    let fixture = Fixture::new();
    let report = fixture.report(&[
        "--since",
        "2026-10-29T10:00:00Z",
        "--until",
        "2026-10-29T10:05:00Z",
        "--project",
        "nai-wai-bench",
        "--tools=codex",
        "--by",
        "model",
        "--by",
        "effort",
    ]);
    assert_eq!(report["schema"], 1);
    assert_eq!(report["total"]["requests"], 1);
    assert_eq!(report["total"]["sessions"], 1);
    assert_eq!(report["total"]["response_ms"], 2000);
    assert_eq!(
        report["total"]["tokens"],
        json!({"input":6,"cache_read":4,"cache_write":0,"cache_write_1h":0,"output":4,"reasoning":2})
    );
    assert!((report["total"]["cost_usd"]["total"].as_f64().unwrap() - 0.0001456).abs() < 1e-12);
    assert_eq!(report["groups"][0]["key"]["model"], "bench-model-1");
    assert_eq!(report["groups"][0]["groups"][0]["key"]["effort"], "xhigh");
    assert_eq!(report["prices"]["source"], "price-file");
    assert_eq!(report["prices"]["overridden"], json!(["bench-model-1"]));
    assert_eq!(report["unpriced"], json!([]));
    let homes = report["codex_limits"].as_array().unwrap();
    assert_eq!(homes.len(), 2);
    assert_ne!(homes[0]["home"], homes[1]["home"]);
    for home in homes {
        assert_eq!(home["windows"].as_array().unwrap().len(), 2);
        assert_eq!(home["windows"][0]["first"]["used_percent"], 23.0);
        assert_eq!(home["windows"][0]["last"]["used_percent"], 70.0);
        assert_eq!(home["windows"][0]["resets_at"], "2026-10-29T14:00:01.000Z");
        assert_eq!(home["windows"][1]["first"]["used_percent"], 2.0);
        assert_eq!(home["windows"][1]["last"]["used_percent"], 2.0);
    }
    let report = fixture.report(&[
        "--project=/other/*",
        "--project=/tmp/nwb-*",
        "--session=claude-one",
        "--by=effort",
    ]);
    assert_eq!(report["total"]["requests"], 1);
    assert_eq!(report["groups"][0]["key"]["effort"], "high");
    assert_eq!(report["unpriced"], json!(["unpriced-model-1"]));
    let empty = fixture.report(&["--since=2026-10-30T00:00:00Z"]);
    assert_eq!(empty["total"]["requests"], 0);
    assert_eq!(empty["codex_limits"], json!([]));
    // Table format is useful on the same headless executable.
    let table = fixture
        .command()
        .args(["report", "--offline", "--by=model", "--by=effort"])
        .output()
        .unwrap();
    assert!(table.status.success());
    let table = String::from_utf8_lossy(&table.stdout);
    assert!(table.contains("Unpriced models:"));
    assert!(
        table
            .lines()
            .any(|line| line.starts_with("  model=bench-model-1"))
    );
    assert!(
        table
            .lines()
            .any(|line| line.starts_with("    effort=xhigh"))
    );
    assert!(!table.contains(" / "));

    // Older caches may carry nanoseconds and an explicit zero UTC offset.
    let mut cached = PriceTable::default();
    cached.fetched_at = Some("2026-10-29T10:00:00.123456789+00:00".into());
    cached
        .save(&fixture.dir.path().join("cache/toktop/prices.json"))
        .unwrap();
    assert_eq!(
        fixture.report(&[])["prices"]["fetched_at"],
        "2026-10-29T10:00:00.123Z"
    );
}

#[test]
fn report_rejects_bad_options_and_prices_without_emitting_json() {
    let now = toktop::now_ms();
    assert_eq!(
        toktop::report::parse_time("90m", now).unwrap(),
        now - 90 * 60_000
    );
    assert_eq!(
        toktop::report::parse_time("6h", now).unwrap(),
        now - 6 * 3_600_000
    );
    assert_eq!(
        toktop::report::parse_time("2d", now).unwrap(),
        now - 2 * 86_400_000
    );
    assert_eq!(
        toktop::report::parse_time("2026-10-29T18:00:00+08:00", now).unwrap(),
        toktop::report::parse_time("2026-10-29T10:00:00Z", now).unwrap()
    );
    for time in ["bad", "💥", "-1h", "999999999999999999d"] {
        assert!(toktop::report::parse_time(time, now).is_err());
    }
    for args in [
        vec!["--since=1h", "--until=2h"],
        vec!["--by=invalid"],
        vec!["--by=model", "--by=model"],
        vec!["--format=csv"],
        vec!["--project=["],
        vec!["--until"],
        vec!["--oops"],
    ] {
        assert!(
            Options::parse(
                &args.into_iter().map(str::to_owned).collect::<Vec<_>>(),
                now
            )
            .is_err()
        );
    }
    let fixture = Fixture::new();
    for command in ["--snapshot", "report"] {
        let output = fixture
            .command()
            .args([command, "--tools=claude"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("`claude`"));
        assert!(Tool::ALL.iter().all(|tool| error.contains(tool.key())));
    }
    for invalid in ["input=-1.0", "input=nan", "input=inf"] {
        std::fs::write(
            fixture.dir.path().join("bad.toml"),
            format!("[models.test]\n{invalid}\noutput=0.0\n"),
        )
        .unwrap();
        let output = fixture
            .command()
            .args(["report", "--offline", "--price-file"])
            .arg(fixture.dir.path().join("bad.toml"))
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn new_codex_homes_are_found_on_rescan_and_widget_groups_match_report() {
    // Use the existing subprocess pattern to isolate environment discovery.
    const CHILD: &str = "TOKTOP_REPORT_RESCAN_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let fixture = Fixture::new();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "new_codex_homes_are_found_on_rescan_and_widget_groups_match_report",
            ])
            .env(CHILD, "1")
            .env("HOME", fixture.dir.path())
            .env("CODEX_HOME", fixture.dir.path().join(".codex"))
            .env("XDG_CONFIG_HOME", fixture.dir.path().join("config"))
            .env("CLAUDE_CONFIG_DIR", fixture.dir.path().join("claude"))
            .env_remove("TOKTOP_CODEX_HOMES")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let config = Config::load(&toktop::config::path()).unwrap();
    let mut store = Store::with_config(&config);
    store.sync_all();
    let before = store.files.len();
    let home = Path::new(&std::env::var("HOME").unwrap()).join("runs/three/codex-home");
    write_lines(
        &home.join("sessions/rollout-three.jsonl"),
        &codex("three", "/tmp/nwb-codex-three"),
    );
    write_lines(
        &home.join("session_index.jsonl"),
        &[json!({"id":"three","thread_name":"New benchmark"})],
    );
    assert!(store.sync_all());
    assert_eq!(store.files.len(), before + 1);
    assert_eq!(store.titles["three"], "New benchmark");
    let summary = toktop::aggregate::summarize(
        &store,
        &PriceTable::default(),
        toktop::aggregate::Period::AllTime,
        toktop::now_ms(),
    );
    let project = summary
        .by_project
        .iter()
        .find(|row| row.key == "nai-wai-bench")
        .unwrap();
    assert_eq!(project.usage.requests, 3);
    assert!(
        summary
            .by_model
            .iter()
            .any(|row| row.label == "bench-model-1 · xhigh")
    );
    assert_eq!(summary.codex_limits_by_home.len(), 3);
}

#[test]
fn quota_uses_reset_boundaries_and_handles_missing_reset_metadata() {
    use toktop::model::{LimitWindow, RateLimits, Session};
    let cases = [
        // A missing estimate keeps the known reset; a later, different reset
        // starts a new window even when usage rises rather than falling.
        (
            vec![
                (1_000, 23.0, Some(600)),
                (2_000, 24.0, None),
                (3_000, 23.0, Some(601)),
                (4_000, 25.0, Some(1200)),
                (5_000, 26.0, None),
                (1_261_000, 2.0, None),
            ],
            vec![(23.0, 23.0), (25.0, 26.0), (2.0, 2.0)],
        ),
        // With no reset estimates, falling usage is the reset signal.
        (
            vec![
                (1_000, 23.0, None),
                (2_000, 25.0, None),
                (3_000, 2.0, None),
                (4_000, 4.0, None),
            ],
            vec![(23.0, 25.0), (2.0, 4.0)],
        ),
    ];
    for (observations, expected) in cases {
        let session = Session {
            codex_home: Some("/extra/codex-home".into()),
            limit_history: observations
                .into_iter()
                .map(|(observed, used_percent, resets_at)| RateLimits {
                    observed,
                    plan: None,
                    primary: Some(LimitWindow {
                        used_percent,
                        window_minutes: 300,
                        resets_at,
                    }),
                    secondary: None,
                })
                .collect(),
            ..Session::default()
        };
        let report = toktop::report::build(
            &[(Tool::Codex, session)],
            &Config::default(),
            &PriceTable::default(),
            &Options::default(),
        );
        let json = serde_json::to_value(report).unwrap();
        let windows = json["codex_limits"][0]["windows"].as_array().unwrap();
        let observed: Vec<_> = windows
            .iter()
            .map(|window| {
                (
                    window["first"]["used_percent"].as_f64().unwrap(),
                    window["last"]["used_percent"].as_f64().unwrap(),
                )
            })
            .collect();
        assert_eq!(observed, expected);
    }
}
