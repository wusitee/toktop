//! Parser contracts: dedup, incremental reads and token bucket mapping.

use std::io::Write;
use std::path::Path;
use toktop::model::{Tokens, Tool};
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
    r#"{"type":"user","sessionId":"s1","cwd":"/work/app","timestamp":"2026-10-01T10:00:00Z"}"#,
    "\n",
    // Streaming writes one response several times. The first copy is a bare
    // prompt snapshot (whole prompt as input, no cache fields); the second
    // reports the cache split and a 1-hour split larger than its partial total.
    r#"{"type":"assistant","sessionId":"s1","requestId":"r1","timestamp":"2026-10-01T10:00:01Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":150,"output_tokens":1}}}"#,
    "\n",
    r#"{"type":"assistant","sessionId":"s1","requestId":"r1","timestamp":"2026-10-01T10:00:01Z","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":20,"cache_creation":{"ephemeral_1h_input_tokens":30}}}}"#,
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
    assert_eq!(session.events.len(), 2);
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
fn codex_fork_skips_replayed_parent_turns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rollout-fork.jsonl");
    let mut text = String::new();
    // A subagent identified only by nested spawn metadata.
    text += r#"{"timestamp":"2026-10-01T11:00:00Z","type":"session_meta","payload":{"id":"01a00000-0500-7fff-8000-000000000000","cwd":"/work/app","source":{"subagent":{"thread_spawn":{"parent_thread_id":"01a00000-0000-7000-8000-000000000001"}}}}}"#;
    text += "\n";
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
    assert_eq!(session.events.len(), 1);
    assert_eq!(session.events[0].tokens.total(), 500);
    assert_eq!(
        session.models[session.events[0].model as usize],
        "gpt-6-luna"
    );
    assert!(session.events[0].key.is_some());
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

fn generation(step: u64, usage: &[(u64, u64)], response_id: &str, model: &str) -> Vec<u8> {
    let mut usage_msg = Vec::new();
    for &(number, value) in usage {
        field_varint(&mut usage_msg, number, value);
    }
    field_bytes(&mut usage_msg, 11, response_id.as_bytes());
    let mut chat = Vec::new();
    field_bytes(&mut chat, 4, &usage_msg);
    field_bytes(&mut chat, 19, model.as_bytes());
    let mut root = Vec::new();
    field_bytes(&mut root, 1, &chat);
    field_varint(&mut root, 2, step);
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
        2,
        &[(1, 1000), (2, 500), (5, 200), (9, 30), (10, 70)],
        "resp-1",
        "gemini-3.8-flash-n",
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
