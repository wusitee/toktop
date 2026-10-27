# Architecture

toktop reads AI coding tool logs, turns them into usage events and aggregates them for display. Optional remote sources use SSH subprocesses to run a headless collector on each host. The price download is the only other network access. On Hyprland, the widget also asks the compositor's IPC socket for the space reserved by panels.

## Threads

| Thread | Role |
|---|---|
| GTK main | Renders `Summary` values. Never reads files. |
| `toktop-worker` | Registers the watcher before the initial scan, owns the `Store`, applies file changes and sends summaries when data changes or the clock ticks. |
| `toktop-watch` | Blocks on inotify and forwards changed paths to the worker. |
| ksni service, `toktop-tray` | Serve the tray icon over D-Bus and push tooltip updates; tray clicks and menu items are forwarded to the GTK main thread. Present only when the tray is registered. |

The worker debounces changes (250 ms quiet, at most 1 s behind continuous writes) and recomputes every 30 s for time-dependent values: the live token rate, rolling periods aging out, relative times and day rollover. Prices refresh in a short-lived thread once a day. Without inotify, or while any required watch is missing, the worker falls back to a full rescan every 10 s. Incomplete watch registration is retried on each poll or rescan. Every tick publishes the summary so relative times and Codex limit countdowns stay current. The widget keeps its row slots and only changes their text and visibility on update, adding slots when a breakdown or the sessions list first needs more, up to 100 per list. The detail card is the exception: while open, it is rebuilt from each summary, and closed, it holds no widgets. This keeps per-update CPU low.

## Application lifecycle

`Shell` (in `src/ui/mod.rs`) lives for the whole process. It owns the worker, the latest summary, the settings ([configuration.md](configuration.md)) and the tray icon. The widget window is built on demand from the latest summary and dropped when hidden.

- toktop is a single-instance `GApplication`. Running `toktop` while it is already running shows the existing widget; `toktop --background` then does nothing; `toktop --toggle` and `toktop --settings` activate the primary instance's `app.toggle` and `app.settings` actions over D-Bus.
- When the compositor supports wlr-layer-shell and `widget.layer` is not `window`, the window becomes a layer surface (`src/ui/layer.rs`, a few direct bindings to libgtk4-layer-shell). Otherwise it is an ordinary window.
- The application holds itself open while a layer widget or tray icon exists, so hiding the widget keeps toktop running. A plain window without a tray icon quits when closed.
- Toggling closes an open layer widget. An open ordinary window is raised first if it is not focused.
- The period is saved to the config file when it changes.
- The settings window (`src/ui/settings.rs`) edits the settings through `Shell`, which saves them and applies them at once: a changed tool list is sent to the worker, which replaces its store and watcher, and a changed widget setting rebuilds an open widget.

## Data flow

1. **Discover.** `Store` walks each tool's source roots ([sources.md](sources.md)).
2. **Parse.** Each source file becomes one `Session`: events (timestamp, model, tokens, optional dedup key), project, title and Codex rate limits.
3. **Aggregate.** `summarize` filters events by period, merges copies of one response across files, prices each event and builds the breakdowns ([accounting.md](accounting.md), [pricing.md](pricing.md)).

Opening a day detail card requests full model, project and grouped session breakdowns for that local date, independently of the main period. The worker builds them from cached events in the existing aggregation pass and includes them in `Summary.day_detail`. The card groups models, projects and tools behind the same tabs as the main card, and scrolls that breakdown and the sessions list independently through up to 100 entries. Day drill-downs share those data and open in one reusable secondary popover, leaving the day overview visible. The secondary panel closes independently; closing or switching the day also closes it. Only one day's breakdowns are retained, and closing the card, switching to a period detail or hiding the widget releases them. Navigation adds no threads, timers, file reads or network requests.

## Modules

| Module | Role |
|---|---|
| `src/parse/` | One parser per tool, plus a minimal protobuf reader for Antigravity and libzstd bindings for dsh. |
| `src/store.rs` | Index of every source file and its parsed session. Discovers files, re-reads changed ones and loads tool-side title indexes. |
| `src/aggregate.rs` | Builds a `Summary` for a period: totals, breakdowns by tool, model, project and session (with subagents grouped), daily history, streak, live token rate, average speed and recent speed (latest 20 timed requests). Each breakdown row and day carries a `Split`: its tokens by model, tool and project, and its session count. Model rows also carry their average speed per day over the last 30 days. |
| `src/pricing.rs`, `src/pricing/aliases.rs` | LiteLLM price table, reduced and cached; tool model aliases; cost per request. |
| `src/model.rs` | Shared types: `Tool`, `Tokens`, `Event`, `Session`, rate limits. |
| `src/paths.rs` | Source and cache locations, with the tools' environment overrides. |
| `src/watch.rs` | Recursive inotify watcher. |
| `src/worker.rs` | Worker thread loop. |
| `src/ui/` | Application shell (`mod.rs`), the card (`widget.rs`, `style.css`), the detail card (`detail.rs`), heatmap and trend charts, settings window, layer-shell bindings, Hyprland reserved-space query and tray icon. |
| `src/config.rs` | Settings file. |

## Incremental reads

JSONL files are append-only. Each keeps a `Cursor` (byte offset, inode and parser state), so a change parses only the new bytes. A trailing line without a newline is left for the next read. dsh logs are concatenations of Zstandard frames, one per appended batch; their cursor advances by whole frames, decompressed through the system libzstd, and a frame still being written is left for the next read. A file that shrinks or changes inode is re-parsed from the start.

Antigravity databases are re-read whole on change; they are small. A file counts as changed when its inode, size or mtime differs, or those of its SQLite `-wal` sidecar, since commits land there first. A locked or unreadable database keeps its previous snapshot.

The first scan parses all files in parallel on up to 8 threads.

Remote collectors perform a fresh scan and export a versioned JSON snapshot of parsed sessions and resolved titles. The desktop bounds the response and time spent waiting, validates it and replaces only the corresponding host's last complete snapshot. Scan errors fail the export, leaving the previous snapshot in place. Remote sessions, parent links and event keys are scoped to their SSH alias; projects and session titles identify their host. Remote entries stay separate from locally discovered files. Daily dates use the desktop's local timezone. SSH refresh failures appear in the widget and retain the in-memory snapshot; no remote cache is persisted. See [configuration.md](configuration.md#remote-usage-over-ssh).

## Watching

The watcher subscribes to content and directory events only (modify, create, delete, move), not opens or reads, so it is idle while no tool writes. Roots that do not exist yet are picked up when created: their nearest existing ancestor is watched for new directories. Title indexes outside the roots are watched through their parent directory, or its nearest existing ancestor until the parent appears. Title deletions and moves also update the store, and every full scan reloads titles even if transcripts are unchanged. Directory moves and self-events rebuild watches so descriptors keep current paths, including when a root is moved aside and recreated. Queue overflow re-walks all roots to register directories whose creation events were lost before requesting a full resync. Registration and traversal errors report degraded coverage to the worker and log one warning; an ENOSPC warning points to `fs.inotify.max_user_watches`. The watcher blocks on inotify and a worker control socket while idle.

## Pricing availability

`Summary.prices_available` is false when the price table is empty. The UI then displays “—” for costs and a banner explaining that costs are hidden until the price list downloads; `--print` reports “cost unavailable”. Token counts remain visible and individual unpriced-model warnings are suppressed. A failed refresh keeps any previously cached prices.

## Debugging

Run `TOKTOP_DEBUG=1 toktop` to print timestamped stderr lines for window `close-request`, application `shutdown`, `app.quit` activation and GApplication `name-lost`. These diagnostics help distinguish window closure, an explicit quit and loss of the application’s D-Bus name.

## Memory

glibc is limited to one malloc arena (`M_ARENA_MAX=1`) at startup, and the heap is trimmed after full scans and after the widget is hidden. Without this the parallel initial scan leaves about 20 MB stranded in per-thread arenas. The retained data for ~11K events is about 4 MB; most of the remaining footprint is GTK's own.

With the widget shown, toktop uses about 35 MB PSS under GTK's cairo renderer (`GSK_RENDERER=cairo`); the GL and Vulkan renderers add driver memory on top. GTK keeps its style, icon and renderer caches after the widget is hidden, so hiding it frees little, and repeated show/hide cycles do not grow it. On a desktop session with an accessibility bus, GTK's accessibility objects add roughly 15 MB while the widget is shown.

## Periods

Periods are rolling windows reaching back a fixed time from now, not to local midnight: Last 24 hours, Last 7 days (7 × 24 hours), Last 30 days (30 × 24 hours) and All time. All four are available on the widget's switcher and in the tray menu. The config file and CLI accept the legacy keys `today`, `week` and `month` as aliases for `24h`, `7d` and `30d`.

The heatmap, trend, streak, day detail cards and per-model daily speed use local calendar days independently of the selected period. The live token rate is tokens per minute over the last five minutes. The streak counts consecutive days with usage ending today, or yesterday when today has none yet. Average speed is described in [accounting.md](accounting.md#response-time-and-speed).

## Project grouping

Projects group by full path with trailing `/` characters removed. `Row.key` retains that path; `Row.label` displays its basename, extended by the fewest parent components needed to distinguish other projects in the summary. Detail cards apply the same rule (`project_labels`) to the projects they list. `project_label` remains a basename-only helper for standalone display. Model rows use the model id for both key and label.

## Tests

Maintained tests live in `tests/`: `parse.rs` (per-tool parser contracts), `aggregate.rs` (pricing and aggregation), `store.rs` (change detection and title reloads), `watch.rs` (directory lifetime and watch coverage) and `ui/detail.rs` (date-scoped detail selection, included by the UI module). Fixtures are built inline; tests never read real logs.
