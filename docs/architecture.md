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

- **Claude Code.** Streaming writes one response several times. Copies sharing `message.id` + `requestId` merge by per-field maximum, except input: a copy that supplies `input_tokens` together with cache fields carries only the uncached remainder and replaces a bare copy's whole-prompt input. Cache fields without input leave that input replaceable. The 1-hour cache-write split is kept raw and clamped to the merged total. `output_tokens` already includes thinking. Advisor calls in `usage.iterations` (`advisor_message`) are separate events under their own model. Malformed optional iterations or entries are ignored without dropping main usage. `<synthetic>` placeholder messages are ignored.
- **Codex.** Each request's `last_token_usage` is counted once, when `total_token_usage` changes. A total that drops by less than about one request is a late duplicate and is skipped; a larger drop, or a restart at the request's own size, is a compaction reset and is counted. Rows without `last_token_usage` fall back to the difference of totals. A last-only row advances an existing baseline component-wise by its usage, so a later cumulative snapshot does not count it again. Thread service tiers come from `thread_settings_applied` snapshots and persist across turns; a snapshot without a tier clears the thread tier. Request tiers from turn contexts, usage records or token-count metadata override the thread tier until the next turn context. During parent replay, only settings snapshots whose `thread_id` matches the fork's first `session_meta` id affect its tier, including snapshots before its first local turn context. Cached and cache-write tokens are subsets of input, and reasoning a subset of output, so they are split out.

  Forked threads (`forked_from_id`, `parent_thread_id` or `source.subagent.thread_spawn`) begin by replaying the parent's turns. Creation times are compared only when both the session and turn ids are UUIDv7; an older turn is replayed, while a turn at or after the fork opens the gate. Legacy turn ids require a child-local `task_started` announcement followed by the matching `turn_context`. For a UUIDv7 child, a legacy announcement's `started_at` must be at or after the child's creation second, so older ancestor announcements cannot open the gate. Replayed counts set the baseline without emitting usage. Known inherited totals re-reported after the gate opens are skipped without lowering the baseline, except a genuine reset whose total equals the request's own usage. An accepted compaction reset retires the inherited totals, so the child's new counter can reuse those values. Events are keyed by session id, timestamp and total, so a rollout in both `sessions/` and `archived_sessions/` counts once.
- **Antigravity.** `gen_metadata` rows decode as `chatModel.usage`: `#1 + #2` input, `#5` cache read, `#9` output, `#10` thinking. Rows are deduplicated by response id (`#11`). A first pass over all rows maps display labels (`chatModel.#21`) to machine ids (`#19`), so continuation rows missing an id can recover it from matching labels, including later rows. Routing ids (`gemini-default`) provide no concrete model identity and use the same sibling recovery. Labels mapping to different canonical alias targets are ambiguous and cannot recover an id. Rows without either field use the file's single machine id only when every label was identified; otherwise they remain `unknown`. Labels are join keys, never pricing keys. Timestamps prefer the explicit legacy generation timestamp (`chatModel.#9.#4`), then the referenced step's metadata (`root.#2` → `steps.idx`). Without that join, planner response steps (`step_type = 15`) join by response id (`metadata.#9.#11` → `usage.#11`), then by generation index (`metadata.#20.#3` → `gen_metadata.idx`). Session creation time or file mtime is the fallback.

Events carrying a stable key are also deduplicated across files at aggregation time: a response copied into several files (resumed sessions) is counted once. Copies merge with the same rules as Claude streaming copies: per-field maxima, split-derived input replacing bare input, and the raw 1-hour cache-write maximum clamped only after merging. Events retain the input-split provenance and raw cache-write split across file boundaries; consumers receive clamped token buckets. Keyed raw-only cache splits remain pending for later copies; aggregation excludes events whose merged token buckets are still empty from totals, request counts and session lists.

The parsers follow the same rules as [tokscale](https://github.com/junhoyeo/tokscale), which Token Monitor uses, and are validated against its per-session totals.

## Pricing

Prices come from LiteLLM's `model_prices_and_context_window.json`, downloaded at most once a day and cached as a reduced table in `$XDG_CACHE_HOME/toktop/prices.json`. Lookup normalizes dotted Claude versions (`claude-sonnet-4.6` → `claude-sonnet-4-6`) and resolves supported tool machine-id aliases from tokscale's static map (`MODEL_PLACEHOLDER_M35` → `claude-sonnet-4-6`) before consulting the catalog. It tries the full id (so `azure/…` keeps Azure's price), then the bare name preferring first-party provider entries, then drops trailing `-suffix` segments (`claude-opus-4-6-thinking` → `claude-opus-4-6`), resolving aliases for each candidate. Gemini 3 and 3.1 Pro also try their catalog's `-preview` spelling. Long-context tiers (above 200K or 272K prompt tokens) apply per request, including tiered cache-write rates; a rate the tier omits falls back to the base rate. Models without a price count as zero cost and are listed as unpriced.

Codex priority/fast requests use LiteLLM's explicit priority input, output and cache-read rates, selecting long-context priority rates only when the model's long-context tier applies. Models without a long-context tier keep their base priority rates regardless of prompt size. Each missing priority rate falls back to twice the corresponding standard rate for that request's context length, matching tokscale's premium; cache-write rates are also doubled. Default, Flex and unspecified service tiers use standard pricing. Reduced price caches without priority fields remain readable and use the same fallback.

## Project grouping

Projects group by full path with trailing `/` characters removed. `Row.key` retains that path; `Row.label` displays its basename, extended by the fewest parent components needed to distinguish other projects in the summary. `project_label` remains a basename-only helper for standalone display. Model rows use the model id for both key and label.
