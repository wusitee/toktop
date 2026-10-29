# Sources

Where toktop finds each tool's usage and how titles are resolved.

| Tool | Files | Format |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl`, `$XDG_CONFIG_HOME/claude/projects` (or each dir in `CLAUDE_CONFIG_DIR`) | Append-only JSONL |
| Codex | `$CODEX_HOME/{sessions,archived_sessions}/**/rollout-*.jsonl` (default `~/.codex`), plus configured extra homes | Append-only JSONL |
| Antigravity | `~/.gemini/antigravity{,-cli}/conversations/*.db`, `~/.t3/userdata/providers/antigravity/*/antigravity-acp/conversations/*.db` | SQLite with protobuf blobs |
| Pi | `$PI_CODING_AGENT_SESSION_DIR/**/*.jsonl`, else `$PI_CODING_AGENT_DIR/sessions` (default `~/.pi/agent/sessions`) | Append-only JSONL |
| Kimi Code | `$KIMI_CODE_HOME/sessions/<workspace>/session_<id>/agents/<agent>/wire.jsonl` (default `~/.kimi-code`) | Append-only JSONL |
| dsh | `$DSH_HOME/sessions/<project>/<session>/session[.vN].jsonl.zstd` (default `~/.dsh`), or `session[.vN].jsonl` with compression off | Append-only JSONL in concatenated Zstandard frames |

Titles come from Claude `ai-title` / `custom-title` lines, Codex `session_index.jsonl`, Antigravity `conversation_summaries.db`, Pi `session_info` names, Kimi Code `state.json` and dsh `session/title` lines, falling back to the first prompt; see [accounting.md](accounting.md#sessions).

T3 Antigravity provider instances are discovered automatically and watched for new conversations and database changes. Their `brain/` artifacts and configuration directories are excluded from scanning and watching.

A `sessionDir` set in Pi's `settings.json` is not read; set `PI_CODING_AGENT_SESSION_DIR` instead. Sessions of the earlier Python `kimi-cli` (`~/.kimi`) are not read. dsh keeps one file per format generation (`session.jsonl.zstd` is v0, `session.v4.jsonl.zstd` v4) and leaves older generations in place when it migrates a session; only the highest generation in a session directory is read. A missing root is skipped. Roots created later are picked up by the next scan.

Additional Codex homes can be configured with `codex_homes` or `TOKTOP_CODEX_HOMES`; see [configuration.md](configuration.md#extra-codex-homes-and-project-groups). Each home supplies its own session directories, title index and quota observations.
