//! Collector protocol and source discovery.

use std::process::Command;
use toktop::{model::Tool, remote::Snapshot};

#[test]
fn ssh_transport_accepts_only_successful_compatible_responses() {
    const CHILD: &str = "TOKTOP_REMOTE_TEST_CASE";
    if let Ok(case) = std::env::var(CHILD) {
        let result = toktop::remote::fetch("dev-server", &[Tool::Codex]);
        match case.as_str() {
            "ok" => assert_eq!(result.unwrap().version, 1),
            "version" => assert!(
                result
                    .unwrap_err()
                    .contains("unsupported collector protocol")
            ),
            "failure" => assert!(result.unwrap_err().contains("collector failed")),
            _ => unreachable!(),
        }
        assert!(toktop::remote::fetch("-oBad", &[Tool::Codex]).is_err());
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let ssh = dir.path().join("ssh");
    std::fs::write(&ssh, "#!/bin/sh\ncase \"$TOKTOP_REMOTE_TEST_CASE\" in\n  ok) printf '%s' '{\"version\":1,\"sessions\":[]}' ;;\n  version) printf '%s' '{\"version\":2,\"sessions\":[]}' ;;\n  failure) echo 'collector failed' >&2; exit 1 ;;\nesac\n").unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    for case in ["ok", "version", "failure"] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ssh_transport_accepts_only_successful_compatible_responses",
            ])
            .env(CHILD, case)
            .env("PATH", dir.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn collector_exports_parsed_sessions_and_titles() {
    let dir = tempfile::tempdir().unwrap();
    let codex = dir.path().join(".codex");
    let sessions = codex.join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("rollout-test.jsonl"), concat!(
        "{\"timestamp\":\"2026-10-01T10:00:00Z\",\"type\":\"session_meta\",\"payload\":{\"id\":\"thread-id\",\"cwd\":\"/work/app\"}}\n",
        "{\"timestamp\":\"2026-10-01T10:01:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":100,\"output_tokens\":30,\"total_tokens\":130},\"last_token_usage\":{\"input_tokens\":100,\"output_tokens\":30,\"total_tokens\":130}}}}\n"
    )).unwrap();
    std::fs::write(
        codex.join("session_index.jsonl"),
        "{\"id\":\"thread-id\",\"thread_name\":\"A title\"}\n",
    )
    .unwrap();
    let antigravity = dir.path().join(".gemini/antigravity");
    std::fs::create_dir_all(&antigravity).unwrap();
    std::fs::write(antigravity.join("conversations"), "not a directory").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_toktop-collector"))
        .arg("--snapshot")
        .arg("--tools=codex")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("CODEX_HOME", &codex)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("PI_CODING_AGENT_SESSION_DIR")
        .env_remove("PI_CODING_AGENT_DIR")
        .env_remove("KIMI_CODE_HOME")
        .env_remove("DSH_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot: Snapshot = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(snapshot.version, 1);
    assert_eq!(snapshot.sessions.len(), 1);
    let (tool, session) = &snapshot.sessions[0];
    assert_eq!(*tool, Tool::Codex);
    assert_eq!(session.title.as_deref(), Some("A title"));
    assert_eq!(session.events[0].tokens.total(), 130);
}

#[test]
fn collector_fails_instead_of_exporting_an_incomplete_scan() {
    let dir = tempfile::tempdir().unwrap();
    let codex = dir.path().join("codex");
    std::fs::create_dir_all(&codex).unwrap();
    std::fs::write(codex.join("sessions"), "not a directory").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_toktop-collector"))
        .arg("--snapshot")
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("CODEX_HOME", &codex)
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("PI_CODING_AGENT_SESSION_DIR")
        .env_remove("PI_CODING_AGENT_DIR")
        .env_remove("KIMI_CODE_HOME")
        .env_remove("DSH_HOME")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("sessions"));
}
