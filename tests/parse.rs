//! Parser contracts: dedup, incremental reads and token bucket mapping.

use std::io::Write;
use std::path::Path;
use toktop::model::{ServiceTier, Tokens, Tool};
use toktop::parse;

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
}

fn append(path: &Path, text: &str) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

const CLAUDE: &str = concat!(
    r#"{"type":"user","isMeta":true,"sessionId":"s1","cwd":"/work/app","timestamp":"2026-09-30T10:00:00Z","message":{"role":"user","content":"Caveat: injected"}}"#,
    "\n",
    r#"{"type":"user","sessionId":"s1","cwd":"/work/app","timestamp":"2026-10-01T10:00:00Z","message":{"role":"user","content":[{"type":"text","text":"Fix  the\nbug"}]}}"#,
    "\n",
    // The raw 1-hour split arrives before any countable tokens.
    r#"{"type":"assistant","sessionId":"s1","requestId":"r1","timestamp":"2026-10-01T10:00:01Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"cache_creation":{"ephemeral_1h_input_tokens":30}}}}"#,
    "\n",
    // A bare prompt snapshot reports the whole prompt without cache fields.
    r#"{"type":"assistant","sessionId":"s1","requestId":"r1","timestamp":"2026-10-01T10:00:01Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":150,"output_tokens":1}}}"#,
    "\n",
    // Cache fields without input must leave the bare input replaceable.
    r#"{"type":"assistant","sessionId":"s1","requestId":"r1","timestamp":"2026-10-01T10:00:01Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"cache_read_input_tokens":100}}}"#,
    "\n",
    r#"{"type":"assistant","sessionId":"s1","requestId":"r1","timestamp":"2026-10-01T10:00:01Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":20}}}"#,
    "\n",
    // Final copy: full output, an advisor call listed in `iterations`.
    r#"{"type":"assistant","sessionId":"s1","requestId":"r1","timestamp":"2026-10-01T10:00:02Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":50,"cache_read_input_tokens":100,"cache_creation_input_tokens":40,"iterations":[{"type":"message","input_tokens":10,"output_tokens":50},{"type":"advisor_message","model":"claude-fable-5-1","input_tokens":700,"output_tokens":7}]}}}"#,
    "\n",
    r#"{"type":"assistant","sessionId":"s1","timestamp":"2026-10-01T10:00:03Z","message":{"id":"m2","model":"<synthetic>","usage":{"input_tokens":0,"output_tokens":0}}}"#,
    "\n",
    r#"{"type":"ai-title","sessionId":"s1","aiTitle":"Generated"}"#,
    "\n",
    r#"{"type":"custom-title","sessionId":"s1","customTitle":"Renamed"}"#,
    "\n",
    r#"{"type":"ai-title","sessionId":"s1","aiTitle":"Generated later"}"#,
    "\n",
);

#[test]
fn claude_merges_streaming_duplicates_and_prefers_custom_titles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s1.jsonl");
    write(&path, CLAUDE);
    let (session, _) = parse::parse_full(Tool::ClaudeCode, &path).unwrap();

    assert_eq!(session.id, "s1");
    assert_eq!(session.project.as_deref(), Some("/work/app"));
    assert_eq!(session.title.as_deref(), Some("Renamed"));
    assert_eq!(session.prompt.as_deref(), Some("Fix the bug"));
    assert_eq!(session.events.len(), 2);
    // From the prompt to the response's last copy.
    assert_eq!(session.events[0].duration_ms, 2000);
    assert_eq!(
        session.events[0].tokens,
        Tokens {
            input: 10,
            output: 50,
            cache_read: 100,
            cache_write: 40,
            cache_write_1h: 30,
            reasoning: 0,
        }
    );
    let advisor = &session.events[1];
    assert_eq!(session.models[advisor.model as usize], "claude-fable-5-1");
    assert_eq!((advisor.tokens.input, advisor.tokens.output), (700, 7));
}

#[test]
fn appended_lines_extend_the_session_and_partial_lines_wait() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s1.jsonl");
    let (first, rest) = CLAUDE.split_at(CLAUDE.find("\n").unwrap() + 1);
    // The first response arrives without its trailing newline: still being written.
    let response = &rest[..rest.find("\n").unwrap()];
    write(&path, &format!("{first}{response}"));
    let (mut session, cursor) = parse::parse_full(Tool::ClaudeCode, &path).unwrap();
    let mut cursor = cursor.unwrap();
    assert!(session.events.is_empty());

    append(&path, &rest[response.len()..]);
    assert!(parse::parse_appended(&path, &mut session, &mut cursor).unwrap());
    let (full, _) = parse::parse_full(Tool::ClaudeCode, &path).unwrap();
    assert_eq!(session, full);

    // A rewritten (shorter) file cannot be resumed.
    write(&path, first);
    assert!(!parse::parse_appended(&path, &mut session, &mut cursor).unwrap());
}

#[test]
fn claude_pending_cache_split_is_hidden_until_usage_arrives() {
    use toktop::aggregate::{self, Period};
    use toktop::pricing::PriceTable;
    use toktop::store::Store;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pending.jsonl");
    let split = concat!(
        r#"{"type":"assistant","timestamp":"2026-10-01T10:00:01Z","message":{"id":"cache","model":"claude-sonnet-4-6","usage":{"cache_creation":{"ephemeral_1h_input_tokens":100000}}}}"#,
        "\n",
    );
    write(&path, split);
    let (mut session, cursor) = parse::parse_full(Tool::ClaudeCode, &path).unwrap();
    let mut cursor = cursor.unwrap();
    let mut store = Store::default();
    store.insert_session(path.clone(), Tool::ClaudeCode, session.clone());
    let summary = aggregate::summarize(&store, &PriceTable::default(), Period::AllTime, 0);
    assert_eq!(summary.total.requests, 0);
    assert!(summary.total.tokens.is_empty());
    assert!(summary.sessions.is_empty());
    assert!(summary.by_model.is_empty());
    assert!(summary.daily.is_empty());

    append(
        &path,
        concat!(
            r#"{"type":"assistant","timestamp":"2026-10-01T10:00:02Z","message":{"id":"cache","model":"claude-sonnet-4-6","usage":{"cache_creation_input_tokens":100000}}}"#,
            "\n",
        ),
    );
    assert!(parse::parse_appended(&path, &mut session, &mut cursor).unwrap());
    assert_eq!(session.events[0].tokens.cache_write_1h, 100000);
}

fn codex_count(
    ts: &str,
    total: u64,
    input: u64,
    cached: u64,
    output: u64,
    reasoning: u64,
) -> String {
    let last = input + output;
    format!(
        r#"{{"timestamp":"{ts}","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"total_tokens":{total}}},"last_token_usage":{{"input_tokens":{input},"cached_input_tokens":{cached},"output_tokens":{output},"reasoning_output_tokens":{reasoning},"total_tokens":{last}}}}},"rate_limits":{{"limit_id":"codex","plan_type":"plus","primary":{{"used_percent":12.0,"window_minutes":300,"resets_at":1800000000}}}}}}}}"#
    ) + "\n"
}

#[test]
fn codex_counts_each_request_once_and_splits_overlapping_buckets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-a.jsonl");
    let mut text = String::new();
    text += r#"{"timestamp":"2026-10-01T10:00:00Z","type":"session_meta","payload":{"id":"01a00000-0000-7000-8000-000000000001","cwd":"/work/app"}}"#;
    text += "\n";
    text += r#"{"timestamp":"2026-10-01T10:00:00Z","type":"turn_context","payload":{"turn_id":"01a00000-0000-7000-8000-000000000002","model":"gpt-6.1-sol"}}"#;
    text += "\n";
    text += &codex_count("2026-10-01T10:00:01Z", 1000, 900, 600, 100, 40);
    // Repeated snapshot with an unchanged total.
    text += &codex_count("2026-10-01T10:00:02Z", 1000, 900, 600, 100, 40);
    text += &codex_count("2026-10-01T10:00:03Z", 2000, 950, 0, 50, 0);
    // Out-of-order snapshot that regresses by about one request.
    text += &codex_count("2026-10-01T10:00:04Z", 1990, 950, 0, 50, 0);
    // Counter restarted after compaction: a real request.
    text += &codex_count("2026-10-01T10:00:05Z", 300, 280, 0, 20, 0);
    write(&path, &text);

    let (session, _) = parse::parse_full(Tool::Codex, &path).unwrap();
    assert_eq!(session.project.as_deref(), Some("/work/app"));
    assert_eq!(session.models, ["gpt-6.1-sol"]);
    assert_eq!(session.events.len(), 3);
    assert_eq!(
        session.events[0].tokens,
        Tokens {
            input: 300,
            output: 60,
            cache_read: 600,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 40,
        }
    );
    let total: u64 = session
        .events
        .iter()
        .map(|event| event.tokens.total())
        .sum();
    assert_eq!(total, 1000 + 1000 + 300);
    let limits = session.limits.unwrap();
    assert_eq!(limits.plan.as_deref(), Some("plus"));
    assert_eq!(limits.primary.unwrap().window_minutes, 300);
}

#[test]
fn codex_request_duration_spans_input_to_last_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-timing.jsonl");
    let item = |ts: &str, payload: &str| {
        format!(r#"{{"timestamp":"{ts}","ordinal":1,"type":"response_item","payload":{payload}}}"#)
            + "\n"
    };
    let mut text = String::new();
    text += r#"{"timestamp":"2026-10-01T10:00:00Z","type":"turn_context","payload":{"model":"gpt-6.1-sol"}}"#;
    text += "\n";
    text += &item(
        "2026-10-01T10:00:00Z",
        r#"{"type":"message","id":"m1","role":"user","content":[{"type":"input_text","text":"<environment_context>"},{"type":"input_text","text":"Add a widget"}]}"#,
    );
    text += &item("2026-10-01T10:00:02Z", r#"{"type":"reasoning","id":"r1"}"#);
    text += &item(
        "2026-10-01T10:00:04Z",
        r#"{"type":"function_call","name":"shell"}"#,
    );
    // The tool runs for 26 s; its output starts the next request.
    text += &item(
        "2026-10-01T10:00:30Z",
        r#"{"type":"function_call_output","call_id":"c1"}"#,
    );
    text += &codex_count("2026-10-01T10:00:31Z", 1000, 900, 0, 100, 0);
    text += &codex_count("2026-10-01T10:00:31Z", 1000, 900, 0, 100, 0);
    text += &item(
        "2026-10-01T10:00:33Z",
        r#"{"type":"message","id":"m2","role":"assistant","content":[]}"#,
    );
    text += &codex_count("2026-10-01T10:00:34Z", 2000, 950, 0, 50, 0);
    // A request whose start was not logged has no duration.
    text += &codex_count("2026-10-01T10:00:40Z", 3000, 950, 0, 50, 0);
    // Nor one whose only earlier input was consumed by a previous request.
    text += &item("2026-10-01T10:00:45Z", r#"{"type":"reasoning","id":"r2"}"#);
    text += &codex_count("2026-10-01T10:00:46Z", 4000, 950, 0, 50, 0);
    // An unchanged snapshot mid-response does not end the request, and a
    // tool result logged in the same millisecond as its call starts the next.
    text += &item(
        "2026-10-01T10:01:00Z",
        r#"{"type":"message","id":"m3","role":"user","content":[]}"#,
    );
    text += &item("2026-10-01T10:01:02Z", r#"{"type":"reasoning","id":"r3"}"#);
    text += &codex_count("2026-10-01T10:01:03Z", 4000, 950, 0, 50, 0);
    text += &item(
        "2026-10-01T10:01:04Z",
        r#"{"type":"function_call","name":"shell"}"#,
    );
    text += &item(
        "2026-10-01T10:01:04Z",
        r#"{"type":"function_call_output","call_id":"c2"}"#,
    );
    text += &codex_count("2026-10-01T10:01:05Z", 5000, 950, 0, 50, 0);
    text += &item(
        "2026-10-01T10:01:06Z",
        r#"{"type":"message","id":"m4","role":"assistant","content":[]}"#,
    );
    text += &codex_count("2026-10-01T10:01:07Z", 6000, 950, 0, 50, 0);
    // An aborted turn's unfinished response does not lend its start to the
    // next turn.
    text += &item(
        "2026-10-01T10:02:00Z",
        r#"{"type":"message","id":"m5","role":"user","content":[]}"#,
    );
    text += &item("2026-10-01T10:02:02Z", r#"{"type":"reasoning","id":"r5"}"#);
    text += &codex_count("2026-10-01T10:02:03Z", 6000, 950, 0, 50, 0);
    text += "{\"timestamp\":\"2026-10-01T10:02:04Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"turn_aborted\"}}\n";
    text += &item(
        "2026-10-01T10:02:20Z",
        r#"{"type":"message","id":"m6","role":"user","content":[]}"#,
    );
    text += &item(
        "2026-10-01T10:02:24Z",
        r#"{"type":"message","id":"m7","role":"assistant","content":[]}"#,
    );
    text += &codex_count("2026-10-01T10:02:25Z", 7000, 950, 0, 50, 0);
    write(&path, &text);

    let (session, _) = parse::parse_full(Tool::Codex, &path).unwrap();
    let durations: Vec<u32> = session
        .events
        .iter()
        .map(|event| event.duration_ms)
        .collect();
    assert_eq!(durations, [4000, 3000, 0, 0, 4000, 2000, 4000]);
    assert_eq!(session.prompt.as_deref(), Some("Add a widget"));
}

#[test]
fn codex_fork_skips_replayed_parent_turns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-fork.jsonl");
    let mut text = String::new();
    // A subagent identified only by nested spawn metadata.
    text += r#"{"timestamp":"2026-10-01T11:00:00Z","type":"session_meta","payload":{"id":"01a00000-0500-7fff-8000-000000000000","cwd":"/work/app","source":{"subagent":{"thread_spawn":{"parent_thread_id":"01a00000-0000-7000-8000-000000000001"}}}}}"#;
    text += "\n";
    text += "{\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_settings\":{\"service_tier\":\"priority\"}}}\n";
    text += "{\"type\":\"token_usage_record\",\"payload\":{\"usage\":{\"service_tier\":\"priority\"}}}\n";
    // Parent history, replayed with its original (older) ids.
    text += r#"{"timestamp":"2026-10-01T11:00:00Z","type":"session_meta","payload":{"id":"01a00000-0000-7000-8000-000000000001"}}"#;
    text += "\n";
    text += r#"{"timestamp":"2026-10-01T11:00:00Z","type":"turn_context","payload":{"turn_id":"01a00000-0000-7000-8000-000000000002","model":"gpt-6.1-sol"}}"#;
    text += "\n";
    text += &codex_count("2026-10-01T11:00:00Z", 5000, 4000, 0, 1000, 0);
    // The fork's own turn, created in the same millisecond as the fork but
    // with lower random bits.
    text += r#"{"timestamp":"2026-10-01T11:00:01Z","type":"turn_context","payload":{"turn_id":"01a00000-0500-7000-8000-000000000000","model":"gpt-6-luna"}}"#;
    text += "\n";
    // The inherited total reported again before the first request.
    text += &codex_count("2026-10-01T11:00:01Z", 5000, 4000, 0, 1000, 0);
    text += &codex_count("2026-10-01T11:00:02Z", 5500, 450, 0, 50, 0);
    write(&path, &text);

    let (session, _) = parse::parse_full(Tool::Codex, &path).unwrap();
    assert_eq!(session.id, "01a00000-0500-7fff-8000-000000000000");
    assert_eq!(
        session.parent.as_deref(),
        Some("01a00000-0000-7000-8000-000000000001")
    );
    assert_eq!(session.events.len(), 1);
    assert_eq!(session.events[0].tokens.total(), 500);
    assert_eq!(
        session.models[session.events[0].model as usize],
        "gpt-6-luna"
    );
    assert!(session.events[0].key.is_some());
    assert_eq!(session.events[0].tier, ServiceTier::Standard);
}

#[test]
fn codex_fork_accepts_own_settings_before_local_turn() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-fork.jsonl");
    let mut text = concat!(
        r#"{"type":"session_meta","payload":{"id":"01a109b5-e987-7602-86e6-baf20e2d564e","forked_from_id":"01a109b5-0000-7000-8000-000000000001"}}"#, "\n",
        r#"{"type":"session_meta","payload":{"id":"01a109b5-0000-7000-8000-000000000001"}}"#, "\n",
        r#"{"type":"turn_context","payload":{"turn_id":"01a109b5-0000-7000-8000-000000000002","model":"gpt-6.1-sol"}}"#, "\n",
        r#"{"type":"event_msg","payload":{"type":"thread_settings_applied","thread_id":"01a109b5-0000-7000-8000-000000000001","thread_settings":{"service_tier":"priority"}}}"#, "\n",
    ).to_owned();
    text += &codex_count("2026-10-05T10:00:00Z", 5000, 4000, 0, 1000, 0);
    text += concat!(
        r#"{"type":"event_msg","payload":{"type":"thread_settings_applied","thread_id":"01a109b5-e987-7602-86e6-baf20e2d564e","thread_settings":{"service_tier":"priority"}}}"#,
        "\n",
    );
    // Neither parent, unrelated nor unscoped snapshots may clear the child's tier.
    for thread_id in [
        Some("01a109b5-0000-7000-8000-000000000001"),
        Some("other"),
        None,
    ] {
        text += &serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "thread_settings_applied", "thread_id": thread_id, "thread_settings": {}}
        }).to_string();
        text += "\n";
    }
    text += concat!(
        r#"{"type":"turn_context","payload":{"turn_id":"01a109b5-e988-7000-8000-000000000001","model":"gpt-6.1-sol"}}"#,
        "\n",
    );
    text += &codex_count("2026-10-05T10:00:01Z", 24_351, 19_000, 0, 351, 0);
    write(&path, &text);

    let (session, _) = parse::parse_full(Tool::Codex, &path).unwrap();
    assert_eq!(session.id, "01a109b5-e987-7602-86e6-baf20e2d564e");
    assert_eq!(session.events.len(), 1);
    assert_eq!(session.events[0].tokens.total(), 19_351);
    assert_eq!(session.events[0].tier, ServiceTier::Priority);
}

fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn field_varint(out: &mut Vec<u8>, number: u64, value: u64) {
    varint(out, number << 3);
    varint(out, value);
}

fn field_bytes(out: &mut Vec<u8>, number: u64, value: &[u8]) {
    varint(out, number << 3 | 2);
    varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

fn timestamp(seconds: u64) -> Vec<u8> {
    let mut out = Vec::new();
    field_varint(&mut out, 1, seconds);
    out
}

fn generation(
    step: Option<u64>,
    usage: &[(u64, u64)],
    response_id: &str,
    model: &str,
    label: Option<&str>,
    legacy_seconds: Option<u64>,
) -> Vec<u8> {
    let mut usage_msg = Vec::new();
    for &(number, value) in usage {
        field_varint(&mut usage_msg, number, value);
    }
    field_bytes(&mut usage_msg, 11, response_id.as_bytes());
    let mut chat = Vec::new();
    field_bytes(&mut chat, 4, &usage_msg);
    if !model.is_empty() {
        field_bytes(&mut chat, 19, model.as_bytes());
    }
    if let Some(label) = label {
        field_bytes(&mut chat, 21, label.as_bytes());
    }
    if let Some(seconds) = legacy_seconds {
        let mut generation = Vec::new();
        field_bytes(&mut generation, 4, &timestamp(seconds));
        field_bytes(&mut chat, 9, &generation);
    }
    let mut root = Vec::new();
    field_bytes(&mut root, 1, &chat);
    if let Some(step) = step {
        field_varint(&mut root, 2, step);
    }
    root
}

#[test]
fn antigravity_reads_generation_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conv-1.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE trajectory_metadata_blob (id text, data blob);
         CREATE TABLE steps (idx integer, metadata blob);
         CREATE TABLE gen_metadata (idx integer, data blob);",
    )
    .unwrap();
    let mut folder = Vec::new();
    field_bytes(&mut folder, 1, b"file:///work/app");
    let mut meta = Vec::new();
    field_bytes(&mut meta, 1, &folder);
    field_bytes(&mut meta, 2, &timestamp(1_790_000_000));
    conn.execute(
        "INSERT INTO trajectory_metadata_blob VALUES ('main', ?1)",
        [&meta],
    )
    .unwrap();
    let mut step = Vec::new();
    field_bytes(&mut step, 1, &timestamp(1_790_000_100));
    conn.execute("INSERT INTO steps VALUES (2, ?1)", [&step])
        .unwrap();

    let first = generation(
        Some(2),
        &[(1, 1000), (2, 500), (5, 200), (9, 30), (10, 70)],
        "resp-1",
        "gemini-3.8-flash-n",
        None,
        None,
    );
    // The same response recorded twice must count once.
    for (idx, blob) in [first.clone(), first].iter().enumerate() {
        conn.execute(
            "INSERT INTO gen_metadata VALUES (?1, ?2)",
            rusqlite::params![idx as i64, blob],
        )
        .unwrap();
    }
    drop(conn);

    // A database locked by its writer is an error, not an empty session.
    let writer = rusqlite::Connection::open(&path).unwrap();
    writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
    assert!(parse::parse_full(Tool::Antigravity, &path).is_err());
    drop(writer);

    let (session, cursor) = parse::parse_full(Tool::Antigravity, &path).unwrap();
    assert!(cursor.is_none());
    assert_eq!(session.id, "conv-1");
    assert_eq!(session.project.as_deref(), Some("/work/app"));
    assert_eq!(session.events.len(), 1);
    let event = &session.events[0];
    assert_eq!(event.ts, 1_790_000_100_000);
    assert_eq!(
        event.tokens,
        Tokens {
            input: 1500,
            output: 30,
            cache_read: 200,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 70,
        }
    );
}

#[test]
fn antigravity_decodes_workspace_uri_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE trajectory_metadata_blob (data blob);
         CREATE TABLE gen_metadata (idx integer, data blob);",
    )
    .unwrap();
    for (uri, expected) in [
        ("file:///work/my%20project", "/work/my project"),
        ("file://localhost/work/my%20project", "/work/my project"),
        ("file:///work/%E6%88%91%e7%9a%84", "/work/我的"),
        ("file:///work/%FF%20project", "/work/%FF%20project"),
        ("file:///work/bad%2%GG+name", "/work/bad%2%GG+name"),
        ("/work/my%20project", "/work/my%20project"),
    ] {
        let mut folder = Vec::new();
        field_bytes(&mut folder, 1, uri.as_bytes());
        let mut meta = Vec::new();
        field_bytes(&mut meta, 1, &folder);
        conn.execute("DELETE FROM trajectory_metadata_blob", [])
            .unwrap();
        conn.execute("INSERT INTO trajectory_metadata_blob VALUES (?1)", [&meta])
            .unwrap();
        let (session, _) = parse::parse_full(Tool::Antigravity, &path).unwrap();
        assert_eq!(session.project.as_deref(), Some(expected), "{uri}");
    }
}

#[test]
fn codex_nested_child_skips_replayed_legacy_uuid_v4_turn() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-child.jsonl");
    // Ported from tokscale: the ancestor task_started predates the child.
    let text = concat!(
        r#"{"timestamp":"2026-05-05T21:52:10.197Z","type":"session_meta","payload":{"id":"019e5c03-1f5d-7000-8000-000000000001","forked_from_id":"019e5c03-0000-7000-8000-000000000001","source":{"subagent":{"thread_spawn":{"parent_thread_id":"019e5c03-0000-7000-8000-000000000001","depth":2}}},"thread_source":"subagent","model_provider":"openai","cwd":"/repo"}}"#,
        "\n",
        r#"{"timestamp":"2026-05-05T21:52:10.197Z","type":"session_meta","payload":{"id":"019e5c03-0000-7000-8000-000000000001","forked_from_id":"019e5b00-0000-7000-8000-000000000001","source":{"subagent":{"thread_spawn":{"parent_thread_id":"019e5b00-0000-7000-8000-000000000001","depth":1}}},"thread_source":"subagent","model_provider":"openai","cwd":"/repo"}}"#,
        "\n",
        r#"{"timestamp":"2026-05-05T21:52:10.197Z","type":"session_meta","payload":{"id":"019e5b00-0000-7000-8000-000000000001","source":"cli","thread_source":"user","model_provider":"openai","cwd":"/repo"}}"#,
        "\n",
        r#"{"timestamp":"2026-05-05T21:52:10.197Z","type":"event_msg","payload":{"type":"task_started","turn_id":"81d2f55b-894b-4d67-b75b-436ead477f65","started_at":1778017800}}"#,
        "\n",
        r#"{"timestamp":"2026-05-05T21:52:10.197Z","type":"turn_context","payload":{"turn_id":"81d2f55b-894b-4d67-b75b-436ead477f65","model":"gpt-5.5","cwd":"/repo"}}"#,
        "\n",
        r#"{"timestamp":"2026-05-05T21:52:10.198Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330},"last_token_usage":{"input_tokens":300,"output_tokens":30,"total_tokens":330}}}}"#,
        "\n",
        r#"{"timestamp":"2026-05-05T21:52:10.610Z","type":"event_msg","payload":{"type":"task_started","turn_id":"019e5c03-2100-7000-8000-000000000001","started_at":1779660169}}"#,
        "\n",
        r#"{"timestamp":"2026-05-05T21:52:10.611Z","type":"turn_context","payload":{"turn_id":"019e5c03-2100-7000-8000-000000000001","model":"gpt-5.5","cwd":"/repo"}}"#,
        "\n",
        r#"{"timestamp":"2026-05-05T21:52:10.612Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":320,"output_tokens":32,"total_tokens":352},"last_token_usage":{"input_tokens":20,"output_tokens":2,"total_tokens":22}}}}"#,
        "\n"
    );
    // Exercise both v7 and task_started-announced legacy child turns.
    for own_turn in [
        "019e5c03-2100-7000-8000-000000000001",
        "71d2f55b-894b-4d67-b75b-436ead477f65",
    ] {
        write(
            &path,
            &text.replace("019e5c03-2100-7000-8000-000000000001", own_turn),
        );
        let (session, _) = parse::parse_full(Tool::Codex, &path).unwrap();
        assert_eq!(
            session
                .events
                .iter()
                .map(|event| event.tokens.total())
                .sum::<u64>(),
            22
        );
    }
}

#[test]
fn codex_token_count_advances_baseline_after_missing_total_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-missing-total.jsonl");
    let line1 = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"turn_context","payload":{"model":"gpt-5.2"}}"#;
    let line2 = r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5},"last_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5}}}}"#;
    let line3 = r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
    let line4 = r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":110,"cached_input_tokens":22,"output_tokens":33,"reasoning_output_tokens":6},"last_token_usage":{"input_tokens":10,"cached_input_tokens":2,"output_tokens":3,"reasoning_output_tokens":1}}}}"#;
    let content = format!("{}\n{}\n{}\n{}", line1, line2, line3, line4);
    write(&path, &(content + "\n"));
    let (session, _) = parse::parse_full(Tool::Codex, &path).unwrap();
    assert_eq!(
        session
            .events
            .iter()
            .map(|event| event.tokens.total())
            .sum::<u64>(),
        143
    );

    // A total-only row must also diff from the advanced baseline.
    write(
        &path,
        concat!(
            r#"{"timestamp":"2026-01-01T00:00:01Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"total_tokens":100}}}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":50,"total_tokens":50}}}}"#,
            "\n",
            r#"{"timestamp":"2026-01-01T00:00:03Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":200,"total_tokens":200}}}}"#,
            "\n",
        ),
    );
    let (session, _) = parse::parse_full(Tool::Codex, &path).unwrap();
    assert_eq!(
        session
            .events
            .iter()
            .map(|event| event.tokens.total())
            .sum::<u64>(),
        200
    );
}

#[test]
fn codex_fork_skips_known_inherited_snapshots_after_own_context() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-fork.jsonl");
    let mut text = concat!(
        r#"{"type":"session_meta","payload":{"id":"01a00000-0500-7000-8000-000000000000","forked_from_id":"01a00000-0000-7000-8000-000000000001"}}"#, "\n",
        r#"{"type":"turn_context","payload":{"turn_id":"01a00000-0000-7000-8000-000000000002","model":"gpt-6.1-sol"}}"#, "\n",
    ).to_owned();
    text += &codex_count("2026-10-01T11:00:00Z", 9500, 450, 0, 50, 0);
    text += &codex_count("2026-10-01T11:00:01Z", 10000, 450, 0, 50, 0);
    text += concat!(
        r#"{"type":"turn_context","payload":{"turn_id":"01a00000-0600-7000-8000-000000000000","model":"gpt-6.1-sol"}}"#,
        "\n"
    );
    text += &codex_count("2026-10-01T11:00:02Z", 9500, 90, 0, 10, 0);
    text += &codex_count("2026-10-01T11:00:03Z", 10000, 90, 0, 10, 0);
    text += &codex_count("2026-10-01T11:00:04Z", 10100, 90, 0, 10, 0);
    write(&path, &text);
    let (session, _) = parse::parse_full(Tool::Codex, &path).unwrap();
    assert_eq!(
        session
            .events
            .iter()
            .map(|event| event.tokens.total())
            .sum::<u64>(),
        100
    );

    let before_reset = text.clone();
    // A genuine reset to the request's own size can equal an inherited total.
    text += &codex_count("2026-10-01T11:00:05Z", 9500, 9400, 0, 100, 0);
    write(&path, &text);
    let (session, _) = parse::parse_full(Tool::Codex, &path).unwrap();
    assert_eq!(
        session
            .events
            .iter()
            .map(|event| event.tokens.total())
            .sum::<u64>(),
        9600
    );
    // After compaction, the child's new totals can reuse inherited numbers.
    text += &codex_count("2026-10-01T11:00:06Z", 10000, 450, 0, 50, 0);
    write(&path, &text);
    let (session, _) = parse::parse_full(Tool::Codex, &path).unwrap();
    assert_eq!(
        session
            .events
            .iter()
            .map(|event| event.tokens.total())
            .sum::<u64>(),
        10100
    );

    // A total-only component regression also retires inherited counters.
    // Replace the request-based reset so this branch has to clear the set.
    let mut text = before_reset.replace(
        r#""total_token_usage":{"total_tokens":10100}"#,
        r#""total_token_usage":{"input_tokens":10000,"total_tokens":10100}"#,
    );
    text += concat!(
        r#"{"timestamp":"2026-10-01T11:00:05Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":50,"total_tokens":50}}}}"#,
        "\n",
    );
    text += &codex_count("2026-10-01T11:00:06Z", 9500, 90, 0, 10, 0);
    write(&path, &text);
    let (session, _) = parse::parse_full(Tool::Codex, &path).unwrap();
    assert_eq!(
        session
            .events
            .iter()
            .map(|event| event.tokens.total())
            .sum::<u64>(),
        200
    );
}

#[test]
fn antigravity_recovers_models_without_guessing_from_labels() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("models.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE gen_metadata (idx integer, data blob)")
        .unwrap();
    let cases = [
        // Recovery must see later rows, and return the observed machine id.
        (
            &[
                ("", Some("Claude Sonnet 4.6 (Thinking)")),
                (
                    "MODEL_PLACEHOLDER_M35",
                    Some("Claude Sonnet 4.6 (Thinking)"),
                ),
            ][..],
            "MODEL_PLACEHOLDER_M35",
        ),
        // tokscale's routing fixture: a router id must not poison sibling recovery.
        (
            &[
                ("gemini-default", Some("Gemini 3.6 Flash (High)")),
                ("", Some("Gemini 3.6 Flash (High)")),
                ("gemini-3.6-flash", Some("Gemini 3.6 Flash (High)")),
            ][..],
            "gemini-3.6-flash",
        ),
        // Different spellings of the same priced model are not ambiguous.
        (
            &[
                ("", Some("Pro")),
                ("gemini-pro-default", Some("Pro")),
                ("gemini-pro-agent", Some("Pro")),
            ][..],
            "gemini-pro-default",
        ),
        (
            &[
                ("", Some("Shared")),
                ("MODEL_PLACEHOLDER_M35", Some("Shared")),
                ("MODEL_PLACEHOLDER_M26", Some("Shared")),
            ][..],
            "unknown",
        ),
        // Unlabelled rows inherit the single id only if all labels resolve.
        (
            &[
                ("", None),
                ("MODEL_PLACEHOLDER_M35", Some("Sonnet")),
                ("", Some("Sonnet")),
            ][..],
            "MODEL_PLACEHOLDER_M35",
        ),
        (
            &[
                ("", None),
                ("MODEL_PLACEHOLDER_M35", Some("Sonnet")),
                ("", Some("Other")),
            ][..],
            "unknown",
        ),
        (
            &[
                ("", Some("Other")),
                ("MODEL_PLACEHOLDER_M35", Some("Sonnet")),
            ][..],
            "unknown",
        ),
    ];
    for (rows, expected) in cases {
        conn.execute("DELETE FROM gen_metadata", []).unwrap();
        for (index, (model, label)) in rows.iter().enumerate() {
            let blob = generation(
                None,
                &[(1, 100)],
                &format!("response-{index}"),
                model,
                *label,
                None,
            );
            conn.execute(
                "INSERT INTO gen_metadata VALUES (?1, ?2)",
                rusqlite::params![index as i64, blob],
            )
            .unwrap();
        }
        let (session, _) = parse::parse_full(Tool::Antigravity, &path).unwrap();
        assert_eq!(
            session.models[session.events[0].model as usize], expected,
            "{rows:?}"
        );
        if rows[0].0 == "gemini-default" {
            assert!(
                session
                    .events
                    .iter()
                    .all(|event| session.models[event.model as usize] == expected)
            );
        }
    }
}

#[test]
fn antigravity_prefers_legacy_timestamp_then_joins_steps_by_response_or_generation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("timestamps.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE trajectory_metadata_blob (data blob);
         CREATE TABLE steps (idx integer, step_type integer, metadata blob);
         CREATE TABLE gen_metadata (idx integer, data blob);",
    )
    .unwrap();
    let mut meta = Vec::new();
    field_bytes(&mut meta, 2, &timestamp(1_790_000_000));
    conn.execute("INSERT INTO trajectory_metadata_blob VALUES (?1)", [&meta])
        .unwrap();

    for (index, seconds, response, generation) in [
        (10, 1_790_000_100, "legacy", 0),
        (11, 1_790_000_200, "response", 2),
        (12, 1_790_000_300, "unmatched", 1),
    ] {
        let mut step = Vec::new();
        field_bytes(&mut step, 1, &timestamp(seconds));
        let mut response_meta = Vec::new();
        field_bytes(&mut response_meta, 11, response.as_bytes());
        field_bytes(&mut step, 9, &response_meta);
        let mut generation_meta = Vec::new();
        field_varint(&mut generation_meta, 3, generation);
        field_bytes(&mut step, 20, &generation_meta);
        conn.execute(
            "INSERT INTO steps VALUES (?1, 15, ?2)",
            rusqlite::params![index, step],
        )
        .unwrap();
    }
    for (index, step, response, legacy) in [
        (0, Some(10), "legacy", Some(1_790_000_400)),
        (1, None, "response", None),
        (2, None, "generation", None),
    ] {
        let blob = generation(
            step,
            &[(1, 1)],
            response,
            "gemini-3.8-flash-n",
            None,
            legacy,
        );
        conn.execute(
            "INSERT INTO gen_metadata VALUES (?1, ?2)",
            rusqlite::params![index, blob],
        )
        .unwrap();
    }
    drop(conn);
    let (session, _) = parse::parse_full(Tool::Antigravity, &path).unwrap();
    assert_eq!(
        session
            .events
            .iter()
            .map(|event| event.ts)
            .collect::<Vec<_>>(),
        [
            1_790_000_400_000, // Explicit legacy timestamp outranks every step join.
            1_790_000_200_000, // Response id outranks the conflicting generation index.
            1_790_000_200_000, // Generation index joins when the response id is absent.
        ]
    );
}

#[test]
fn claude_keeps_main_usage_and_valid_advisors_with_malformed_iterations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("iterations.jsonl");
    for iterations in [
        serde_json::Value::Null,
        serde_json::json!({"unexpected": true}),
        serde_json::json!("invalid"),
        serde_json::json!([
            null, 42,
            {"type": "advisor_message", "model": "advisor", "input_tokens": "invalid"},
            {"type": "advisor_message", "model": "advisor", "input_tokens": 7}
        ]),
    ] {
        let line = serde_json::json!({
            "type": "assistant", "timestamp": "2026-10-01T10:00:00Z",
            "message": {"id": "main", "model": "claude-opus-5-5", "usage": {
                "input_tokens": 10, "output_tokens": 2, "iterations": iterations
            }}
        });
        write(&path, &format!("{line}\n"));
        let (session, _) = parse::parse_full(Tool::ClaudeCode, &path).unwrap();
        assert_eq!(session.events[0].tokens.input, 10);
        assert_eq!(session.events[0].tokens.output, 2);
        if iterations.is_array() {
            assert_eq!(session.events.len(), 2);
            assert_eq!(session.events[1].tokens.input, 7);
            assert_eq!(
                session.events[1].key.as_deref(),
                Some("message:main:advisor:3")
            );
        } else {
            assert_eq!(session.events.len(), 1);
        }
    }
}

#[test]
fn codex_service_tiers_follow_thread_snapshots_and_request_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tiers.jsonl");
    let settings = [
        r#"{"type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"service_tier":"priority"}}}"#,
        r#"{"type":"turn_context","payload":{"model":"gpt-6.1-sol"}}"#,
        r#"{"type":"turn_context","payload":{"service_tier":"default"}}"#,
        r#"{"type":"turn_context","payload":{}}"#,
        r#"{"type":"token_usage_record","payload":{"usage":{"service_tier":"default"}}}"#,
        r#"{"type":"turn_context","payload":{}}"#,
        r#"{"type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{}}}"#,
        r#"{"type":"turn_context","payload":{"service_tier":"priority"}}"#,
        r#"{"type":"turn_context","payload":{}}"#,
        r#"{"type":"event_msg","payload":{"type":"thread_settings_applied","service_tier":"priority"}}"#,
    ];
    write(
        &path,
        concat!(
            r#"{"type":"session_meta","payload":{"id":"tiers"}}"#,
            "\n",
            r#"{"type":"turn_context","payload":{"model":"gpt-6.1-sol"}}"#,
            "\n",
        ),
    );
    let (mut session, cursor) = parse::parse_full(Tool::Codex, &path).unwrap();
    let mut cursor = cursor.unwrap();
    for (index, settings) in settings.iter().enumerate() {
        append(
            &path,
            &format!(
                "{settings}\n{}",
                codex_count(
                    "2026-10-01T10:00:01Z",
                    (index as u64 + 1) * 100,
                    80,
                    40,
                    20,
                    5
                )
            ),
        );
        assert!(parse::parse_appended(&path, &mut session, &mut cursor).unwrap());
    }
    let serialized = serde_json::to_value(&session).unwrap();
    let tiers: Vec<_> = serialized["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event.get("tier").and_then(serde_json::Value::as_str))
        .collect();
    assert_eq!(
        tiers,
        [
            Some("Priority"),
            Some("Priority"),
            None,
            Some("Priority"),
            None,
            Some("Priority"),
            None,
            Some("Priority"),
            None,
            Some("Priority")
        ]
    );
    for (parent, tier) in [
        ("/payload", " FAST "),
        ("/payload/info", "priority"),
        ("/payload/info/last_token_usage", "priority"),
        ("/payload/info/total_token_usage", "priority"),
        ("/payload/usage", "priority"),
    ] {
        append(
            &path,
            "{\"type\":\"turn_context\",\"payload\":{\"service_tier\":\"default\"}}\n",
        );
        let mut count: serde_json::Value = serde_json::from_str(&codex_count(
            "2026-10-01T10:00:02Z",
            (session.events.len() as u64 + 1) * 100,
            80,
            40,
            20,
            5,
        ))
        .unwrap();
        count["payload"]["usage"] = serde_json::json!({});
        count
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("service_tier".into(), tier.into());
        append(&path, &format!("{count}\n"));
        assert!(parse::parse_appended(&path, &mut session, &mut cursor).unwrap());
        assert_eq!(session.events.last().unwrap().tier, ServiceTier::Priority);
    }
    let (full, _) = parse::parse_full(Tool::Codex, &path).unwrap();
    assert_eq!(session, full);
    let mut legacy = serde_json::to_value(&session.events[0]).unwrap();
    legacy.as_object_mut().unwrap().remove("tier");
    let legacy: toktop::model::Event = serde_json::from_value(legacy).unwrap();
    assert_eq!(legacy.tier, ServiceTier::Standard);

    // Exercise the parser -> aggregation pricing path, including cached and reasoning tokens.
    let prices = toktop::pricing::PriceTable::from_prices(std::collections::HashMap::from([(
        "gpt-6.1-sol".into(),
        toktop::pricing::Price {
            input: 1.0,
            output: 2.0,
            cache_read: Some(0.25),
            ..Default::default()
        },
    )]));
    let expected: f64 = session
        .events
        .iter()
        .map(|event| {
            90.0 * if event.tier == ServiceTier::Priority {
                2.0
            } else {
                1.0
            }
        })
        .sum();
    let mut store = toktop::store::Store::default();
    store.insert_session(path, Tool::Codex, session);
    let summary =
        toktop::aggregate::summarize(&store, &prices, toktop::aggregate::Period::AllTime, 0);
    assert_eq!(summary.total.cost, expected);
}

#[test]
fn pi_reads_normalized_usage_titles_and_response_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("2026-10-01T10-00-00-000Z_p1.jsonl");
    let text = concat!(
        r#"{"type":"session","version":3,"id":"p1","timestamp":"2026-10-01T10:00:00.000Z","cwd":"/work/app"}"#,
        "\n",
        r#"{"type":"message","id":"a1","parentId":null,"timestamp":"2026-10-01T10:00:01.000Z","message":{"role":"user","content":[{"type":"text","text":"Add  a\nwidget"}],"timestamp":1790848801000}}"#,
        "\n",
        // Output includes reasoning; the response started at the message
        // timestamp; the responding model wins over the requested one.
        r#"{"type":"message","id":"a2","parentId":"a1","timestamp":"2026-10-01T10:00:04.000Z","message":{"role":"assistant","model":"mimo-v2.6-pro","responseModel":"mimo-v2.6","responseId":"r1","usage":{"input":100,"output":50,"cacheRead":900,"cacheWrite":40,"cacheWrite1h":10,"reasoning":20,"totalTokens":1090},"timestamp":1790848801000}}"#,
        "\n",
        // A failed request without usage is not an event.
        r#"{"type":"message","id":"a3","parentId":"a2","timestamp":"2026-10-01T10:00:05.000Z","message":{"role":"assistant","model":"mimo-v2.6-pro","stopReason":"error","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0},"timestamp":1790848804000}}"#,
        "\n",
        r#"{"type":"session_info","id":"a4","parentId":"a3","timestamp":"2026-10-01T10:00:06.000Z","name":"Widget work"}"#,
        "\n",
        // Usage outside assistant messages: tool work and a summary have no
        // model of their own; cache warming names its model.
        r#"{"type":"model_change","id":"a5","parentId":"a4","timestamp":"2026-10-01T10:00:07.000Z","provider":"x","modelId":"glm-5"}"#,
        "\n",
        r#"{"type":"message","id":"a6","parentId":"a5","timestamp":"2026-10-01T10:00:08.000Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"task","content":[],"usage":{"input":7,"output":3,"cacheRead":0,"cacheWrite":0,"totalTokens":10},"isError":false,"timestamp":1790848808000}}"#,
        "\n",
        r#"{"type":"compaction","id":"a7","parentId":"a6","timestamp":"2026-10-01T10:00:09.000Z","summary":"s","firstKeptEntryId":"a6","tokensBefore":1000,"usage":{"input":100,"output":10,"cacheRead":0,"cacheWrite":0,"totalTokens":110}}"#,
        "\n",
        r#"{"type":"usage","id":"a8","parentId":"a7","timestamp":"2026-10-01T10:00:10.000Z","kind":"cache_warm","provider":"anthropic","model":"claude-sonnet-4-6","usage":{"input":0,"output":0,"cacheRead":20,"cacheWrite":0,"totalTokens":20}}"#,
        "\n",
    );
    write(&path, text);

    let (session, _) = parse::parse_full(Tool::Pi, &path).unwrap();
    assert_eq!(session.id, "p1");
    assert_eq!(session.project.as_deref(), Some("/work/app"));
    assert_eq!(session.title.as_deref(), Some("Widget work"));
    assert_eq!(session.prompt.as_deref(), Some("Add a widget"));
    assert_eq!(session.events.len(), 4);
    let by_model: Vec<(&str, u64)> = session
        .events
        .iter()
        .map(|event| {
            (
                session.models[event.model as usize].as_str(),
                event.tokens.total(),
            )
        })
        .collect();
    assert_eq!(
        by_model,
        [
            ("mimo-v2.6", 1090),
            ("unknown", 10),
            ("unknown", 110),
            ("claude-sonnet-4-6", 20)
        ]
    );
    assert_eq!(
        session.events[2].key.as_deref(),
        Some("entry:a7:1790848809000")
    );
    let event = &session.events[0];
    assert_eq!(
        event.tokens,
        Tokens {
            input: 100,
            output: 30,
            cache_read: 900,
            cache_write: 40,
            cache_write_1h: 10,
            reasoning: 20,
        }
    );
    assert_eq!(event.key.as_deref(), Some("response:r1"));
    assert_eq!(event.duration_ms, 3000);
}

#[test]
fn kimi_reads_agent_wires_titles_and_response_time() {
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("wd_app_0123/session_k1");
    let main = session.join("agents/main/wire.jsonl");
    let sub = session.join("agents/agent_2/wire.jsonl");
    std::fs::create_dir_all(main.parent().unwrap()).unwrap();
    std::fs::create_dir_all(sub.parent().unwrap()).unwrap();
    write(
        &main,
        concat!(
            r#"{"type":"metadata","protocol_version":"1.5","created_at":1790848800000}"#,
            "\n",
            r#"{"type":"profile.bind","agentId":"main","modelAlias":"kimi-code/k3","environmentDisclosure":{"cwd":"/work/app"}}"#,
            "\n",
            r#"{"type":"llm.request","agentId":"main","kind":"loop","model":"k3","time":1790848801000}"#,
            "\n",
            // A side call does not move the loop request's start.
            r#"{"type":"llm.request","agentId":"main","kind":"title","model":"k3","time":1790848802000}"#,
            "\n",
            // Usage outside a turn counts but answers no loop request.
            r#"{"type":"usage.record","agentId":"main","model":"kimi-code/k3","usage":{"inputOther":5,"output":2,"inputCacheRead":0,"inputCacheCreation":0},"usageScope":"session","time":1790848802500}"#,
            "\n",
            r#"{"type":"usage.record","agentId":"main","model":"kimi-code/k3","usage":{"inputOther":100,"output":50,"inputCacheRead":900,"inputCacheCreation":40},"usageScope":"turn","time":1790848804000}"#,
            "\n",
            // A cancelled turn's request does not time the next turn.
            r#"{"type":"llm.request","agentId":"main","kind":"loop","model":"k3","time":1790848810000}"#,
            "\n",
            r#"{"type":"turn.ended","agentId":"main","turnId":1,"reason":"cancelled","time":1790848811000}"#,
            "\n",
            // Nor does one cut off without `turn.ended` (Kimi exited).
            r#"{"type":"llm.request","agentId":"main","kind":"loop","model":"k3","time":1790848820000}"#,
            "\n",
            r#"{"type":"turn.prompt","agentId":"main","input":[{"type":"text","text":"go on"}]}"#,
            "\n",
            r#"{"type":"llm.request","agentId":"main","kind":"loop","model":"k3","time":1790848880000}"#,
            "\n",
            r#"{"type":"usage.record","agentId":"main","model":"kimi-code/k3","usage":{"inputOther":1,"output":1,"inputCacheRead":0,"inputCacheCreation":0},"usageScope":"turn","time":1790848881000}"#,
            "\n",
        ),
    );
    write(
        &sub,
        concat!(
            r#"{"type":"usage.record","agentId":"agent_2","model":"kimi-code/k3-256k","usage":{"inputOther":7,"output":3,"inputCacheRead":0,"inputCacheCreation":0},"usageScope":"turn","time":1790848805000}"#,
            "\n",
        ),
    );
    write(
        &session.join("state.json"),
        r#"{"id":"session_k1","title":"Kimi work","titleKind":"generated"}"#,
    );
    // A title copied from an injected prompt is not shown.
    let other = dir.path().join("wd_app_0123/session_k2");
    std::fs::create_dir_all(&other).unwrap();
    write(
        &other.join("state.json"),
        r#"{"id":"session_k2","title":"<instructions> do","titleKind":"replaceable"}"#,
    );

    let (session, _) = parse::parse_full(Tool::Kimi, &main).unwrap();
    assert_eq!(session.id, "k1");
    assert_eq!(session.parent, None);
    assert_eq!(session.project.as_deref(), Some("/work/app"));
    assert_eq!(session.events.len(), 3);
    assert_eq!(session.events[0].duration_ms, 0);
    assert_eq!(session.events[2].duration_ms, 1000);
    let event = &session.events[1];
    assert_eq!(
        event.tokens,
        Tokens {
            input: 100,
            output: 50,
            cache_read: 900,
            cache_write: 40,
            cache_write_1h: 0,
            reasoning: 0,
        }
    );
    assert_eq!(event.duration_ms, 3000);
    assert_eq!(
        event.key.as_deref(),
        Some("main:1790848804000:kimi-code/k3:100:50:900:40")
    );

    let (subagent, _) = parse::parse_full(Tool::Kimi, &sub).unwrap();
    assert_eq!(subagent.id, "k1/agent_2");
    assert_eq!(subagent.parent.as_deref(), Some("k1"));
    assert_eq!(subagent.models, ["kimi-code/k3-256k"]);

    let titles = parse::kimi::titles(dir.path());
    assert_eq!(titles.len(), 1);
    assert_eq!(titles["k1"], "Kimi work");
}

/// One Zstandard frame storing `text` in a single raw block.
fn zstd_frame(text: &str) -> Vec<u8> {
    let mut frame = 0xFD2F_B528_u32.to_le_bytes().to_vec();
    // Single segment, 4-byte content size, no checksum.
    frame.push(0xA0);
    frame.extend((text.len() as u32).to_le_bytes());
    // Last raw block.
    frame.extend(&((text.len() as u32) << 3 | 1).to_le_bytes()[..3]);
    frame.extend(text.as_bytes());
    frame
}

#[test]
fn dsh_reads_zstd_frames_skipping_fork_seed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl.zstd");
    let header = zstd_frame(concat!(
        r#"{"type":"session","version":0,"id":"session-c","createdAt":1790848800000,"cwd":"/work/app","parentSession":"session-p","seedLength":3,"origin":"subagent","delegationDepth":1}"#,
        "\n",
    ));
    let batch = zstd_frame(concat!(
        // Copied from the parent: not this session's prompt or usage.
        r#"{"type":"user/message","seq":0,"time":1790848700000,"data":{"content":[{"type":"text","text":"parent task"}],"source":{"kind":"user"},"role":"user"}}"#,
        "\n",
        r#"{"type":"step/start","seq":1,"time":1790848700100,"data":{"turn":1,"step":1}}"#,
        "\n",
        r#"{"type":"assistant/message","seq":2,"time":1790848701000,"data":{"turn":1,"step":1,"message":{"id":"m0","source":{"kind":"model","model":"deepseek-v4-pro"}},"usage":{"inputTokens":9,"outputTokens":9,"cacheReadTokens":9,"reasoningTokens":0}}}"#,
        "\n",
        // Plugin notices are not prompts.
        r#"{"type":"user/message","seq":3,"time":1790848800001,"data":{"content":[{"type":"text","text":"policy changed"}],"source":{"kind":"plugin"},"role":"user"}}"#,
        "\n",
        r#"{"type":"user/message","seq":4,"time":1790848800002,"data":{"content":[{"type":"text","text":"Review  the\ndiff"}],"source":{"kind":"user"},"role":"user"}}"#,
        "\n",
        r#"{"type":"step/start","seq":5,"time":1790848801000,"data":{"turn":1,"step":1}}"#,
        "\n",
        r#"{"type":"assistant/message","seq":6,"time":1790848803000,"data":{"turn":1,"step":1,"message":{"id":"m1","source":{"kind":"model","model":"deepseek-v4-pro"}},"usage":{"inputTokens":100,"outputTokens":50,"cacheReadTokens":900,"reasoningTokens":20}}}"#,
        "\n",
    ));
    let compaction = zstd_frame(concat!(
        r#"{"type":"compaction/start","seq":7,"time":1790848810000,"data":{"compactionId":"c1"}}"#,
        "\n",
        r#"{"type":"compaction/summary","seq":8,"time":1790848814000,"data":{"compactionId":"c1","model":"deepseek-v4-flash","usage":{"inputTokens":500,"outputTokens":40,"cacheReadTokens":0,"reasoningTokens":0}}}"#,
        "\n",
        r#"{"type":"session/title","seq":9,"time":1790848815000,"data":{"title":" Diff review "}}"#,
        "\n",
    ));
    let mut bytes = [header, batch].concat();
    // The last frame is still being written.
    bytes.extend(&compaction[..10]);
    std::fs::write(&path, &bytes).unwrap();

    let (mut session, cursor) = parse::parse_full(Tool::Dsh, &path).unwrap();
    let mut cursor = cursor.unwrap();
    assert_eq!(session.id, "session-c");
    assert_eq!(session.parent.as_deref(), Some("session-p"));
    assert_eq!(session.project.as_deref(), Some("/work/app"));
    assert_eq!(session.prompt.as_deref(), Some("Review the diff"));
    assert_eq!(session.events.len(), 1);
    let event = &session.events[0];
    assert_eq!(
        event.tokens,
        Tokens {
            input: 100,
            output: 30,
            cache_read: 900,
            cache_write: 0,
            cache_write_1h: 0,
            reasoning: 20,
        }
    );
    assert_eq!(event.duration_ms, 2000);
    assert_eq!(event.key.as_deref(), Some("message:m1"));

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&compaction[10..])
        .unwrap();
    assert!(parse::parse_appended(&path, &mut session, &mut cursor).unwrap());
    assert_eq!(session.events.len(), 2);
    let summary = &session.events[1];
    assert_eq!(session.models[summary.model as usize], "deepseek-v4-flash");
    assert_eq!(summary.tokens.total(), 540);
    assert_eq!(summary.duration_ms, 4000);
    assert_eq!(summary.key.as_deref(), Some("compaction:c1"));
    assert_eq!(session.title.as_deref(), Some("Diff review"));
}
