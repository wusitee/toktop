//! Store change detection.

use toktop::model::Tool;
use toktop::store::Store;

fn generation(id: u8, model: &str) -> Vec<u8> {
    let usage = [0x10, 0x05, 0x5a, 0x01, b'a' + id];
    let mut chat = vec![0x22, usage.len() as u8];
    chat.extend_from_slice(&usage);
    chat.extend_from_slice(&[0x9a, 0x01, model.len() as u8]);
    chat.extend_from_slice(model.as_bytes());
    let mut root = vec![0x0a, chat.len() as u8];
    root.extend_from_slice(&chat);
    root
}

#[test]
fn wal_commits_refresh_antigravity_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conv.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; CREATE TABLE gen_metadata (idx integer, data blob);",
    )
    .unwrap();
    // chatModel { usage { #2: input } , #19: model }, with a distinct responseId per row.
    conn.execute(
        "INSERT INTO gen_metadata VALUES (0, ?1)",
        [generation(0, "m")],
    )
    .unwrap();

    let mut store = Store::default();
    store.track(path.clone(), Tool::Antigravity).unwrap();
    assert_eq!(store.files[&path].session.events.len(), 1);

    // The commit lands in conv.db-wal; conv.db itself is unchanged.
    conn.execute(
        "INSERT INTO gen_metadata VALUES (1, ?1)",
        [generation(1, "m")],
    )
    .unwrap();
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    assert!(store.update([wal.into()]));
    assert_eq!(store.files[&path].session.events.len(), 2);
}

#[test]
fn t3_antigravity_sources_include_conversations_and_skip_artifacts() {
    const CHILD: &str = "TOKTOP_T3_SOURCE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "t3_antigravity_sources_include_conversations_and_skip_artifacts",
            ])
            .env(CHILD, "1")
            .env("HOME", dir.path())
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let root = toktop::paths::source_roots(Tool::Antigravity)
        .pop()
        .unwrap();
    let home = root.join("provider-a/antigravity-acp");
    let conversations = home.join("conversations");
    std::fs::create_dir_all(&conversations).unwrap();
    let path = conversations.join("conversation.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE gen_metadata (idx integer, data blob)")
        .unwrap();
    conn.execute(
        "INSERT INTO gen_metadata VALUES (0, ?1)",
        [generation(0, "gemini-3.8-flash-n")],
    )
    .unwrap();
    let titles = home.join("conversation_summaries.db");
    rusqlite::Connection::open(&titles)
        .unwrap()
        .execute_batch(
            "CREATE TABLE conversation_summaries (conversation_id text, title text);
         INSERT INTO conversation_summaries VALUES ('conversation', 'Gemini task');",
        )
        .unwrap();
    let artifact = home.join("brain/artifacts/unrelated.db");
    std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
    std::fs::write(&artifact, "not a conversation").unwrap();
    assert_eq!(toktop::store::classify(&path), Some(Tool::Antigravity));
    assert_eq!(toktop::store::classify(&artifact), None);
    assert_eq!(toktop::store::classify(&titles), None);

    let mut store = Store::with_tools(vec![Tool::Antigravity]);
    assert!(store.sync_all());
    assert_eq!(store.files.keys().collect::<Vec<_>>(), [&path]);
    assert_eq!(store.files[&path].session.models, ["gemini-3.8-flash-n"]);
    assert_eq!(store.files[&path].session.events[0].tokens.input, 5);
    assert_eq!(store.titles["conversation"], "Gemini task");

    let (tx, rx) = std::sync::mpsc::channel();
    let _watcher = toktop::watch::spawn(vec![root.clone()], vec![titles.clone()], move |change| {
        let _ = tx.send(change);
    })
    .unwrap();
    assert!(matches!(
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
        toktop::watch::Change::Coverage(true)
    ));
    std::fs::write(&artifact, "changed artifact").unwrap();
    assert!(matches!(
        rx.recv_timeout(std::time::Duration::from_millis(100)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));

    // Title files outside conversations still receive change notifications.
    rusqlite::Connection::open(&titles)
        .unwrap()
        .execute(
            "UPDATE conversation_summaries SET title = 'Renamed Gemini task'",
            [],
        )
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let change = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        if let toktop::watch::Change::Paths(paths) = change {
            store.update(paths);
            if store.titles["conversation"] == "Renamed Gemini task" {
                break;
            }
        }
    }

    // A new provider is discovered without restarting or watching its artifacts.
    let next = root.join("provider-b/antigravity-acp/conversations");
    std::fs::create_dir_all(&next).unwrap();
    std::fs::copy(&path, next.join("second.db")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let change = rx
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        if matches!(change, toktop::watch::Change::Rescan) {
            break;
        }
    }
    assert!(store.sync_all());
    assert_eq!(store.files.len(), 2);
    assert_eq!(toktop::paths::antigravity_homes().len(), 4);
}

#[test]
fn sync_all_reloads_titles_without_transcript_changes() {
    // Isolate path discovery in a child process instead of mutating the
    // environment shared by parallel tests or reading the developer's logs.
    const CHILD: &str = "TOKTOP_TITLE_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "sync_all_reloads_titles_without_transcript_changes",
            ])
            .env(CHILD, "1")
            .env("HOME", dir.path())
            .env("CODEX_HOME", dir.path().join("codex"))
            .env("KIMI_CODE_HOME", dir.path().join("kimi"))
            .env("CLAUDE_CONFIG_DIR", dir.path().join("claude"))
            .env("XDG_CONFIG_HOME", dir.path().join("config"))
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let index = toktop::paths::codex_session_index();
    std::fs::create_dir_all(index.parent().unwrap()).unwrap();
    let mut store = Store::default();
    assert!(!store.sync_all());
    std::fs::write(
        &index,
        "{\"id\":\"session\",\"thread_name\":\"First title\"}\n",
    )
    .unwrap();
    assert!(store.sync_all());
    assert_eq!(store.titles["session"], "First title");
    assert!(!store.sync_all());
    std::fs::write(&index, "{\"id\":\"session\",\"thread_name\":\"Renamed\"}\n").unwrap();
    assert!(store.sync_all());
    assert_eq!(store.titles["session"], "Renamed");
    std::fs::remove_file(&index).unwrap();
    assert!(store.sync_all());
    assert!(store.titles.is_empty());
    assert!(store.files.is_empty());
}

#[test]
fn dsh_reads_only_the_latest_generation() {
    const CHILD: &str = "TOKTOP_DSH_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "dsh_reads_only_the_latest_generation"])
            .env(CHILD, "1")
            .env("HOME", dir.path())
            .env("DSH_HOME", dir.path().join("dsh"))
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let dir = toktop::paths::source_roots(Tool::Dsh)[0].join("--work--/s1");
    std::fs::create_dir_all(&dir).unwrap();
    let log = |id: &str| {
        format!(
            "{}\n{}\n",
            r#"{"type":"session","version":0,"id":"s1","createdAt":1790848800000,"delegationDepth":0}"#,
            format_args!(
                r#"{{"type":"assistant/message","seq":0,"time":1790848801000,"data":{{"turn":1,"step":1,"message":{{"id":"{id}"}},"usage":{{"inputTokens":5,"outputTokens":1}}}}}}"#
            ),
        )
    };
    let old = dir.join("session.jsonl");
    std::fs::write(&old, log("m1")).unwrap();
    let mut store = Store::with_tools(vec![Tool::Dsh]);
    assert!(store.sync_all());
    assert_eq!(store.files.keys().collect::<Vec<_>>(), [&old]);

    // Opening the session migrates it into a new generation beside the old.
    let new = dir.join("session.v4.jsonl");
    std::fs::write(&new, log("m2")).unwrap();
    assert!(store.update([new.clone()]));
    assert_eq!(store.files.keys().collect::<Vec<_>>(), [&new]);
    std::fs::write(&old, log("m3")).unwrap();
    store.update([old.clone()]);
    assert_eq!(store.files.keys().collect::<Vec<_>>(), [&new]);

    let mut fresh = Store::with_tools(vec![Tool::Dsh]);
    fresh.sync_all();
    assert_eq!(fresh.files.keys().collect::<Vec<_>>(), [&new]);
}
