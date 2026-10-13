# Architecture

toktop reads the local logs of AI coding tools, turns them into usage events and aggregates them for display. It is a single process with no subprocesses. The only network access is the daily price download.

## Data flow

1. **Discover.** `Store` walks each tool's source roots ([sources.md](sources.md)).
2. **Parse.** Each source file becomes one `Session`: events (timestamp, model, tokens, optional dedup key), project, title and Codex rate limits.
3. **Aggregate.** `summarize` filters events by period, merges copies of one response across files, prices each event and builds the breakdowns ([accounting.md](accounting.md), [pricing.md](pricing.md)).

## Modules

| Module | Role |
|---|---|
| `src/parse/` | One parser per tool, plus a minimal protobuf reader for Antigravity. |
| `src/store.rs` | Index of every source file and its parsed session. Discovers files, re-reads changed ones and loads tool-side title indexes. |
| `src/aggregate.rs` | Builds a `Summary` for a period: totals, breakdowns by tool, model, project and session, daily history, live token rate. |
| `src/pricing.rs`, `src/pricing/aliases.rs` | LiteLLM price table, reduced and cached; tool model aliases; cost per request. |
| `src/model.rs` | Shared types: `Tool`, `Tokens`, `Event`, `Session`, rate limits. |
| `src/paths.rs` | Source and cache locations, with the tools' environment overrides. |

## Incremental reads

JSONL files are append-only. Each keeps a `Cursor` (byte offset, inode and parser state), so a change parses only the new bytes. A trailing line without a newline is left for the next read. A file that shrinks or changes inode is re-parsed from the start.

Antigravity databases are re-read whole on change; they are small. A file counts as changed when its inode, size or mtime differs, or those of its SQLite `-wal` sidecar, since commits land there first. A locked or unreadable database keeps its previous snapshot.

The first scan parses all files in parallel on up to 8 threads.

## Periods

Periods use local time: Today, This week (from Monday), Last 7 days, This month, Last 30 days and All time. The live token rate is tokens per minute over the last five minutes.

## Project grouping

Projects group by full path with trailing `/` characters removed. `Row.key` retains that path; `Row.label` displays its basename, extended by the fewest parent components needed to distinguish other projects in the summary. `project_label` remains a basename-only helper for standalone display. Model rows use the model id for both key and label.

## Tests

Maintained tests live in `tests/`: `parse.rs` (per-tool parser contracts), `aggregate.rs` (pricing and aggregation) and `store.rs` (change detection). Fixtures are built inline; tests never read real logs.
