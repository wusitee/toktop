# Architecture

toktop reads the local logs of AI coding tools, turns them into usage events and aggregates them for display. It is a single process with no subprocesses. The only network access is the daily price download.

## Threads

| Thread | Role |
|---|---|
| GTK main | Renders `Summary` values. Never reads files. |
| `toktop-worker` | Owns the `Store`. Applies file changes, recomputes the summary and sends it to the UI when it differs from the last one. |
| `toktop-watch` | Blocks on inotify and forwards changed paths to the worker. |

The worker debounces changes (250 ms quiet, at most 1 s behind continuous writes) and recomputes every 30 s for time-dependent values: the live token rate, relative times and day rollover. Prices refresh in a short-lived thread once a day. Without inotify the worker falls back to a full rescan every 10 s.

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
| `src/watch.rs` | Recursive inotify watcher. |
| `src/worker.rs` | Worker thread loop. |
| `src/ui/` | GTK 4 / libadwaita window and the daily chart. |

## Incremental reads

JSONL files are append-only. Each keeps a `Cursor` (byte offset, inode and parser state), so a change parses only the new bytes. A trailing line without a newline is left for the next read. A file that shrinks or changes inode is re-parsed from the start.

Antigravity databases are re-read whole on change; they are small. A file counts as changed when its inode, size or mtime differs, or those of its SQLite `-wal` sidecar, since commits land there first. A locked or unreadable database keeps its previous snapshot.

The first scan parses all files in parallel on up to 8 threads.

## Watching

The watcher subscribes to content and directory events only (modify, create, delete, move), not opens or reads, so it is idle while no tool writes. Roots that do not exist yet are picked up when created: their nearest existing ancestor is watched for new directories. Title indexes outside the roots are watched through their parent directory. A queue overflow or a removed watched directory triggers a full resync.

## Memory

glibc is limited to one malloc arena (`M_ARENA_MAX=1`) at startup, and the heap is trimmed after full scans. Without this the parallel initial scan leaves about 20 MB stranded in per-thread arenas. The retained data for ~11K events is about 4 MB; most of the remaining footprint is GTK's own baseline.

## Periods

Periods use local time: Today, This week (from Monday), Last 7 days, This month, Last 30 days and All time. The live token rate is tokens per minute over the last five minutes.

## Project grouping

Projects group by full path with trailing `/` characters removed. `Row.key` retains that path; `Row.label` displays its basename, extended by the fewest parent components needed to distinguish other projects in the summary. `project_label` remains a basename-only helper for standalone display. Model rows use the model id for both key and label.

## Tests

Maintained tests live in `tests/`: `parse.rs` (per-tool parser contracts), `aggregate.rs` (pricing and aggregation) and `store.rs` (change detection). Fixtures are built inline; tests never read real logs.
