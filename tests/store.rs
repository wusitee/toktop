//! Store change detection.

use toktop::model::Tool;
use toktop::store::Store;

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
    let generation = |id: u8| -> Vec<u8> {
        let usage = [0x10, 0x05, 0x5a, 0x01, b'a' + id];
        let mut chat = vec![0x22, usage.len() as u8];
        chat.extend_from_slice(&usage);
        chat.extend_from_slice(&[0x9a, 0x01, 0x01, b'm']);
        let mut root = vec![0x0a, chat.len() as u8];
        root.extend_from_slice(&chat);
        root
    };
    conn.execute("INSERT INTO gen_metadata VALUES (0, ?1)", [generation(0)])
        .unwrap();

    let mut store = Store::default();
    store.track(path.clone(), Tool::Antigravity).unwrap();
    assert_eq!(store.files[&path].session.events.len(), 1);

    // The commit lands in conv.db-wal; conv.db itself is unchanged.
    conn.execute("INSERT INTO gen_metadata VALUES (1, ?1)", [generation(1)])
        .unwrap();
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    assert!(store.update([wal.into()]));
    assert_eq!(store.files[&path].session.events.len(), 2);
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
