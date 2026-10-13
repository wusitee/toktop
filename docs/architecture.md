# Architecture

toktop reads the local logs of AI coding tools, turns them into usage events and aggregates them for display. It is a single process: no Electron, no subprocesses, no network access except the daily price download.

## Modules

| Module | Role |
|---|---|
| `src/parse/` | One parser per tool. Each produces a `Session` (events, models, project, title, Codex rate limits) for one source file. |
| `src/store.rs` | Index of every source file and its parsed session. Discovers files, re-reads changed ones and loads tool-side title indexes. |
| `src/aggregate.rs` | Builds a `Summary` for a period: totals, breakdowns by tool, model, project and session, daily history, live token rate. |
| `src/pricing.rs` | LiteLLM price table, reduced and cached; cost per request. |
| `src/paths.rs` | Source and cache locations, with the tools' environment overrides. |

## Sources

| Tool | Files | Format |
|---|---|---|
| Claude Code | `~/.claude/projects/**/*.jsonl`, `$XDG_CONFIG_HOME/claude/projects` (or each dir in `CLAUDE_CONFIG_DIR`) | Append-only JSONL |
| Codex | `$CODEX_HOME/{sessions,archived_sessions}/**/rollout-*.jsonl` (default `~/.codex`) | Append-only JSONL |
| Antigravity | `~/.gemini/antigravity{,-cli}/conversations/*.db` | SQLite with protobuf blobs |

Titles come from Claude `ai-title` / `custom-title` lines, Codex `session_index.jsonl` and Antigravity `conversation_summaries.db`.

## Incremental reads

JSONL files are append-only. Each keeps a `Cursor` (byte offset, inode and parser state), so a change event parses only the new bytes. A trailing line without a newline is left for the next read. A file that shrinks or changes inode is re-parsed from the start. Antigravity databases are re-read whole on change; they are small. A file counts as changed when its inode, size or mtime differs, or those of its SQLite `-wal` sidecar, since commits land there first. A locked or unreadable database keeps its previous snapshot.

The first scan parses all files in parallel on up to 8 threads.

## Token accounting

`Tokens` buckets are disjoint and sum to the total: uncached input, output, cache read, cache write and reasoning. `cache_write_1h` is the subset of `cache_write` written with a one-hour TTL.

- **Claude Code.** Streaming writes one response several times. Copies sharing `message.id` + `requestId` merge by per-field maximum, except input: a copy that reports cache fields carries only the uncached remainder and replaces a bare copy's whole-prompt input. The 1-hour cache-write split is kept raw and clamped to the merged total. `output_tokens` already includes thinking. Advisor calls in `usage.iterations` (`advisor_message`) are separate events under their own model. `<synthetic>` placeholder messages are ignored.
- **Codex.** Each request's `last_token_usage` is counted once, when `total_token_usage` changes. A total that drops by less than about one request is a late duplicate and is skipped; a larger drop, or a restart at the request's own size, is a compaction reset and is counted. Rows without `last_token_usage` fall back to the difference of totals. Cached and cache-write tokens are subsets of input, and reasoning a subset of output, so they are split out. Forked threads (`forked_from_id`, `parent_thread_id` or `source.subagent.thread_spawn`) begin by replaying the parent's turns. A turn whose UUIDv7 timestamp is older than the fork's is replayed: its counts are not emitted but set the baseline, so a re-reported inherited total is not counted. Events are keyed by session id, timestamp and total, so a rollout in both `sessions/` and `archived_sessions/` counts once.
- **Antigravity.** `gen_metadata` rows decode as `chatModel.usage`: `#1 + #2` input, `#5` cache read, `#9` output, `#10` thinking. Rows are deduplicated by response id (`#11`). Timestamps come from the referenced step's metadata.

Events carrying a stable key are also deduplicated across files at aggregation time: a response copied into several files (resumed sessions) is counted once, with the per-field maximum of all copies.

The parsers follow the same rules as [tokscale](https://github.com/junhoyeo/tokscale), which Token Monitor uses, and are validated against its per-session totals.

## Pricing

Prices come from LiteLLM's `model_prices_and_context_window.json`, downloaded at most once a day and cached as a reduced table in `$XDG_CACHE_HOME/toktop/prices.json`. Lookup tries the full id (so `azure/…` keeps Azure's price), then the bare name preferring first-party provider entries, then drops trailing `-suffix` segments (`claude-opus-4-6-thinking` → `claude-opus-4-6`). Long-context tiers (above 200K or 272K prompt tokens) apply per request, including tiered cache-write rates; a rate the tier omits falls back to the base rate. Models without a price count as zero cost and are listed as unpriced.
