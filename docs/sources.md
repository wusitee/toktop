# Sources

Where toktop finds each tool's usage and how titles are resolved.

| Tool | Files | Format |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl`, `$XDG_CONFIG_HOME/claude/projects` (or each dir in `CLAUDE_CONFIG_DIR`) | Append-only JSONL |
| Codex | `$CODEX_HOME/{sessions,archived_sessions}/**/rollout-*.jsonl` (default `~/.codex`) | Append-only JSONL |
| Antigravity | `~/.gemini/antigravity{,-cli}/conversations/*.db` | SQLite with protobuf blobs |

Titles come from Claude `ai-title` / `custom-title` lines, Codex `session_index.jsonl` and Antigravity `conversation_summaries.db`.

A missing root is skipped. Roots created later are picked up by the next scan.
