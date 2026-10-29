# Accounting reports

`toktop-collector report` reads local logs and prints priced usage without GTK or a display server. `toktop report` exposes the same command in desktop builds. Both read source and project-group settings from the local [configuration file](configuration.md). Reports do not fetch configured SSH hosts; run the collector on the host being measured.

```sh
toktop-collector report --since 90m --project nai-wai-bench \
  --tools codex,claude-code --by model --by effort --format json --offline
```

```text
report [--since TIME] [--until TIME]
       [--tools tool,...] [--project GLOB]... [--session ID]...
       [--by model|effort|project|session|tool|day]...
       [--format table|json] [--offline] [--price-file PATH]
```

Options accept `--name value` and `--name=value`. `--help` lists usage and tool names: `claude-code`, `codex`, `antigravity`, `pi`, `kimi-code`, `dsh`. Without `--tools`, reports use configured tools. Table output is the default.

## Filters and grouping

- Times accept RFC 3339, including offsets, or a nonnegative integer duration ago (`90m`, `6h`, `2d`). Relative bounds use the same current instant. Bounds are inclusive and apply to each event’s timestamp, rather than session creation time. Omitted bounds are unbounded. `--since` must not exceed `--until`.
- `--project` globs match the original full project path or its configured group name. Repeated project filters are OR. Repeated `--session` values are also OR and match exact session ids, including subagent ids individually. Different filter types combine with AND.
- Each repeated `--by` adds a nested grouping level, in argument order. `day` uses UTC dates. Missing effort or project is a null group key. No `--by` produces totals with an empty groups array.
- Cross-file copies merge before filters using the [accounting rules](accounting.md#cross-file-deduplication). The first copy in sorted source-path order supplies timestamp, model and session/project attribution. A matching fork therefore does not recover a request already attributed to another session.
- `sessions` counts distinct `(tool, session id)` pairs with nonempty usage in that total or group. Subagents count as separate sessions. `response_ms` sums known response durations; untimed events contribute zero.

A traversal or source-read error fails the command without emitting a partial report. Missing source directories are skipped.

## Prices

Reports use the widget’s LiteLLM price cache and normal per-request rules, including long-context and Codex priority rates. Unless `--offline` is set, a missing or stale table triggers a refresh; a failed refresh warns on stderr and retains cached prices. Offline reports never fetch prices. Unknown prices contribute zero cost and appear in `unpriced`, even when no price table exists. Missing prices for the free `codex-auto-review` model do not produce a warning.

Override or add tariffs with `--price-file PATH`:

```toml
[models."gpt-5.6-sol"]
input = 4.0
cache_read = 0.40
output = 20.0
```

Rates are USD per **million** tokens, finite and nonnegative. Each entry requires `input` and `output`; optional `cache_read`, `cache_write` and `cache_write_1h` follow the standard fallback rules. An entry replaces that model’s full tariff, including long-context tiers. Codex priority uses the usual 2× fallback. Overrides are applied only to this report and can supply all prices offline with no cache. Invalid files fail the report.

## JSON schema 1

The top-level object has stable fields:

| Field | Meaning |
|---|---|
| `schema` | Integer schema version, currently `1`. |
| `window` | `since` and `until`, normalized UTC RFC 3339 strings or null for an unbounded side. |
| `prices` | `source` (`litellm`, `price-file`, or `unavailable`), nullable `fetched_at`, and sorted `overridden` model ids. A cache without fetch metadata uses its file modification time. |
| `total` | Usage metrics for all matching events. |
| `groups` | Nested groups in `--by` order, sorted by key, with null before named keys. |
| `unpriced` | Sorted model ids with included usage and no known tariff. Costs can be partial when this is nonempty. |
| `codex_limits` | Account-wide quota observations, kept separate by Codex home. |

Every total and group has `requests`, `sessions`, `response_ms`, `tokens`, and `cost_usd`. `tokens` always contains `input`, `cache_read`, `cache_write`, `cache_write_1h`, `output`, and `reasoning`, including zero values. Buckets are disjoint except `cache_write_1h`, which is a subset of `cache_write`; do not add it again. Codex `output` excludes reasoning. `cost_usd` has `input`, `cache_read`, `cache_write`, `output`, and `total`; its output cost includes reasoning.

Groups also contain `key`, an object containing the selected keys through their nesting level. A group with children includes `groups`; leaves omit that field. Missing values are null, not the string `unknown`.

```json
{
  "key": {"model": "gpt-5.6-sol"},
  "requests": 169,
  "sessions": 169,
  "response_ms": 1200000,
  "tokens": {"input": 0, "cache_read": 0, "cache_write": 0, "cache_write_1h": 0, "output": 0, "reasoning": 0},
  "cost_usd": {"input": 0, "cache_read": 0, "cache_write": 0, "output": 0, "total": 0},
  "groups": [{"key": {"model": "gpt-5.6-sol", "effort": "xhigh"}, "requests": 169, "sessions": 169, "response_ms": 1200000, "tokens": {"input": 0, "cache_read": 0, "cache_write": 0, "cache_write_1h": 0, "output": 0, "reasoning": 0}, "cost_usd": {"input": 0, "cache_read": 0, "cache_write": 0, "output": 0, "total": 0}}]
}
```

## Codex quota

`codex_limits` includes the first and last observations **inside** the requested time window, including rate-limit-only log rows. Observations outside the window never become baselines. Tool filters apply; project and session filters do not, because quota reflects all concurrent sessions on that home’s account.

Each entry contains an absolute `home`, the latest observed `plan`, and `windows`. Each window contains `minutes`, `first` and `last` (`at`, `used_percent`), plus nullable `resets_at`. Reset timestamps within 60 seconds of the preceding estimate belong to the same window; `resets_at` reports the latest known estimate. A larger change starts a separate window of the same duration. Usage can decrease slightly when concurrent sessions report stale observations, so a decrease alone does not split a window with valid reset timestamps. Without reset metadata, a usage decrease starts a new window; if metadata disappears, that fallback applies after the last known reset has passed by 60 seconds. A single observation appears as both first and last. With no observations inside the time range, that home is omitted.

Subtract first from last to estimate percentage points consumed within one reset window. These are observed account-wide percentages, not token quotas or a precise attribution to the filtered requests. Different homes can share an account and repeat its observations; do not add their deltas without establishing that they represent separate accounts.
