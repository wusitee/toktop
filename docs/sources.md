# Sources

Where toktop finds each tool's usage and how titles are resolved.

| Tool | Files | Format |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl`, `$XDG_CONFIG_HOME/claude/projects` (or each dir in `CLAUDE_CONFIG_DIR`) | Append-only JSONL |
| Codex | `$CODEX_HOME/{sessions,archived_sessions}/**/rollout-*.jsonl` (default `~/.codex`) | Append-only JSONL |
| Antigravity | `~/.gemini/antigravity{,-cli}/conversations/*.db` | SQLite with protobuf blobs |
| Pi | `$PI_CODING_AGENT_SESSION_DIR/**/*.jsonl`, else `$PI_CODING_AGENT_DIR/sessions` (default `~/.pi/agent/sessions`) | Append-only JSONL |
| Kimi Code | `$KIMI_CODE_HOME/sessions/<workspace>/session_<id>/agents/<agent>/wire.jsonl` (default `~/.kimi-code`) | Append-only JSONL |
| dsh | `$DSH_HOME/sessions/<project>/<session>/session.jsonl.zstd` (default `~/.dsh`), or `session.jsonl` with compression off | Append-only JSONL in concatenated Zstandard frames |

Titles come from Claude `ai-title` / `custom-title` lines, Codex `session_index.jsonl`, Antigravity `conversation_summaries.db`, Pi `session_info` names, Kimi Code `state.json` and dsh `session/title` lines, falling back to the first prompt; see [accounting.md](accounting.md#sessions).

A `sessionDir` set in Pi's `settings.json` is not read; set `PI_CODING_AGENT_SESSION_DIR` instead. Sessions of the earlier Python `kimi-cli` (`~/.kimi`) are not read. A missing root is skipped. Roots created later are picked up by the next scan.
