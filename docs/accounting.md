# Token accounting

How each parser turns tool logs into usage events. The rules follow [tokscale](https://github.com/junhoyeo/tokscale), the engine Token Monitor uses, and are checked against its per-session totals.

`Tokens` buckets are disjoint and sum to the total: uncached input, output, cache read, cache write and reasoning. `cache_write_1h` is the subset of `cache_write` written with a one-hour TTL.

## Claude Code

Source: `message.usage` on `assistant` lines. `output_tokens` already includes thinking, so `reasoning` stays zero.

- Streaming writes one response several times. Copies sharing `message.id` + `requestId` merge by per-field maximum.
- Input is the exception: a copy that supplies `input_tokens` together with cache fields carries only the uncached remainder and replaces a bare copy's whole-prompt input. Cache fields without input leave that input replaceable.
- The 1-hour cache-write split (`cache_creation.ephemeral_1h_input_tokens`) is kept raw and clamped to the merged cache-write total.
- Advisor calls in `usage.iterations` (`advisor_message`) are separate events under their own model. Malformed optional iterations are ignored without dropping main usage.
- `<synthetic>` placeholder messages are ignored.

## Codex

Source: `token_count` events in rollout files. Cached and cache-write tokens are subsets of input, and reasoning a subset of output, so they are split into disjoint buckets.

- Each request's `last_token_usage` is counted once, when `total_token_usage` changes.
- A total that drops by less than about one request is a late duplicate and is skipped. A larger drop, or a restart at the request's own size, is a compaction reset and is counted.
- Rows without `last_token_usage` fall back to the difference of totals. A last-only row advances an existing baseline by its usage, so a later cumulative snapshot does not count it again; it does not create a baseline on its own.
- Events are keyed by session id, timestamp and total, so a rollout in both `sessions/` and `archived_sessions/` counts once.

### Service tiers

- Thread tiers come from `thread_settings_applied` snapshots and persist across turns. A snapshot without a tier clears the thread tier.
- Request tiers from turn contexts, usage records or token-count metadata override the thread tier until the next turn context.
- Pricing per tier is described in [pricing.md](pricing.md#service-tiers).

### Forked threads

Forked threads (`forked_from_id`, `parent_thread_id` or `source.subagent.thread_spawn`) begin by replaying the parent's turns. Replayed counts set the baseline without emitting usage.

- **Opening the gate.** When both the session and turn ids are UUIDv7, their creation times are compared: an older turn is replayed, a turn at or after the fork is the fork's own. Legacy turn ids instead require a child-local `task_started` announcement followed by the matching `turn_context`. For a UUIDv7 child, a legacy announcement's `started_at` must be at or after the child's creation second.
- **Inherited totals.** Totals seen during replay that are re-reported after the gate opens are skipped without lowering the baseline, except a genuine reset whose total equals the request's own usage. An accepted compaction reset retires the inherited totals.
- **Settings.** During replay, only `thread_settings_applied` snapshots whose `thread_id` matches the fork's own session id affect its tier, including snapshots before its first local turn.

## Antigravity

Source: `gen_metadata` rows, each a protobuf without a published schema. Field numbers follow tokscale's reverse-engineered layout.

| Field | Meaning |
|---|---|
| `chatModel.usage.#1 + #2` | Input (fixed system prompt + new input) |
| `chatModel.usage.#5` | Cache read |
| `chatModel.usage.#9` | Output |
| `chatModel.usage.#10` | Thinking, disjoint from output |
| `chatModel.usage.#11` | Response id, used to deduplicate rows |
| `chatModel.#19` | Model id |
| `chatModel.#21` | Model display label |

**Model recovery.** A first pass maps display labels to model ids, so a continuation row missing its id recovers it from a sibling row with the same label. Routing ids (`gemini-default`) are not concrete models and use the same recovery. Labels seen with different priced models are ambiguous and recover nothing. A row with neither field uses the file's single model id only when every label was identified; otherwise it stays `unknown`. Labels are join keys, never pricing keys.

**Timestamps**, in order of preference:

1. The explicit legacy generation timestamp (`chatModel.#9.#4`).
2. The referenced step's metadata (`root.#2` → `steps.idx`).
3. A planner response step (`step_type = 15`) joined by response id (`metadata.#9.#11`), then by generation index (`metadata.#20.#3`).
4. Session creation time, or the file's mtime.

## Pi

Source: `usage` on assistant messages, tool results (nested model work), `compaction` and `branch_summary` entries (summaries) and standalone `usage` entries (such as cache warming), as in Pi's own session totals. Only assistant messages and `usage` entries name their model; the others may cover any model, or several, so they count under `unknown`. An assistant message's `responseModel` (set when the provider answered with a different model) wins over the requested `model`. Pi normalizes every provider: `input`, `cacheRead` and `cacheWrite` are disjoint, `output` includes `reasoning`, and `cacheWrite1h` is a subset of `cacheWrite`. Reasoning is split out of output.

- Entries form a tree. Abandoned branches consumed tokens too, so every usage-bearing entry counts.
- `/fork` and `/clone` copy entries into a new file. Events are keyed by `responseId`, or by entry id and timestamp, and counted once across files.
- Failed or aborted requests with empty usage are ignored.
- The session id and project come from the `session` header; the title from the latest `session_info` name.

## Kimi Code

Source: `usage.record` lines in each agent's `wire.jsonl`, both `turn` scope (model calls of a turn) and `session` scope (work outside turns), as in Kimi's own totals. `inputOther`, `inputCacheRead` and `inputCacheCreation` are disjoint; `output` includes thinking, which Kimi does not report apart. Models keep Kimi's alias (`kimi-code/k3`), priced through an alias.

- Each agent writes its own wire log. `agents/main` is the session; every other agent is a subagent session grouped under it.
- A fork copies the source's records verbatim. Events are keyed by agent, write time, model and token counts, and counted once across files.
- The project is the `environmentDisclosure.cwd` of `profile.bind` or `config.update`; the title comes from the session's `state.json`.

## dsh

Source: `usage` on `assistant/message` lines (one per settled model call), `assistant/attempt` lines (a failed, retried or cancelled call, v4) and `compaction/summary` lines (the summarization call, under its own `model`). `inputTokens` (uncached), `cacheReadTokens` and `cacheWriteTokens` are disjoint; `outputTokens` includes `reasoningTokens`, which is split out when reported. The model is the message's `source.model`; usage without a model of its own takes that of the latest `request/context` (a fork's inherited one included).

- In v4 logs, the model stream is embedded in each message or attempt. An attempt, or a message without `usage`, counts the last `usage` chunk of its stream; every attempt is billed, retries included.
- In v0 logs, a step's `usage` chunk is a line of its own and, as in dsh's own totals, counts as soon as it is written, even when the request then fails. A later sample of the same attempt, or the usage on its message, replaces it; a message without usage leaves it counted. An `llm/retry-started` line begins a new attempt, billed on its own.
- A fork child begins with history copied from its parent, which is skipped: lines with a `seq` below `seedLength` (v0), or everything up to the last `session/end-seed` marker tagged `inherited` (v4).
- Events are keyed by message id or compaction id, and counted once across files.
- The session id, project (`cwd`) and parent (`parentSession`) come from the `session` header; the title from the latest `session/title` line. Only user-typed `user/message` lines (`source.kind` `user`) give the prompt.

## Reasoning effort

Events carry nullable `effort` metadata. Codex takes it from the current `turn_context.effort`; a context without effort clears the previous value. Claude Code reads `perTurnEffort`, then `effort`, from transcript lines, retains it across tool results, and clears it at a new typed prompt when no value is supplied. Streaming and cross-file copies preserve a known value when another copy omits it. Other tools leave it null.

Model breakdowns label recorded efforts as `model · effort`; pricing still resolves the original model id. Reports can group model and effort independently. See [reports.md](reports.md) for the report schema and filtering rules.

## Response time and speed

Each event may carry a response time (`duration_ms`): from the request, the latest prompt or tool result before the response, to the last output written for it. It includes time to first token, thinking and any retries, so speeds are end-to-end and lower than a model's raw decode rate.

- **Claude Code.** The request starts at the latest `user` or `attachment` line; the response ends at the last copy of its `message.id` + `requestId`.
- **Codex.** The request starts at the latest input `response_item` (a user message or a tool call output) and ends at the last output item (reasoning, an assistant message or any `*_call`) before its `token_count`. Tool outputs written after the model's output start the next request; order in the log decides, not timestamps, which often tie. Only an accepted usage snapshot or a counter reset ends a request that has output, so unchanged or rate-limit-only snapshots mid-response do not. `task_complete` and `turn_aborted` clear pending timing. Each input starts at most one request; a response with no unused input before it has no duration. Both are read from the line prefix (`{"timestamp":…,"type":"response_item","payload":{"type":…`), so other rows are never JSON-parsed.
- **Pi.** The message `timestamp` (set when the response starts) to the entry's write time.
- **Kimi Code.** The first `llm.request` of the agent loop (`kind` `loop`) since the previous turn usage, to the next `turn`-scope `usage.record`. `turn.prompt` and `turn.ended` drop a request left unanswered by a cancelled, failed or interrupted turn. Retries are included; side calls and `session`-scope usage neither start nor end a request.
- **dsh.** The step's `step/start`, or the latest `llm/retry-started`, to its `assistant/message`, `assistant/attempt` or usage sample; a compaction's `compaction/start` to its summary.
- **Antigravity.** No response time.

Times of zero or over 15 minutes (a lost request start) are discarded. Average speed is the ratio of sums, output plus reasoning tokens over response time, across timed requests; untimed requests do not count. Rows, sessions and model days average over their own requests; the widget's SPEED tile averages the latest 20 timed requests, whatever the period, so it follows the model in use.

## Sessions

- **Titles**, in order: the tool's title index (Codex `session_index.jsonl`, Antigravity summaries, Kimi Code `state.json`), the transcript's own title (Claude `custom-title`, then `ai-title`, then `summary`; Pi `session_info`; dsh `session/title`), the start of the first typed prompt, then the session id. Prompts skip injected context: Claude `isMeta` lines and tool results, and text starting with `<`, `# AGENTS.md` or `Caveat:`. Whitespace is collapsed and the prompt cut to 120 characters. Kimi Code titles taken from the first prompt (`titleKind` `replaceable`) and dsh fallback titles get the same filtering; custom and generated titles are kept.
- **Subagents.** A Codex rollout spawned by another (`source.subagent.thread_spawn.parent_thread_id`) is grouped under its top-level session, following nested spawns; so is a Kimi Code subagent's wire log and a dsh subagent session (`origin` `subagent`). The session row sums their usage, counts subagents with usage in the period, and lists every model used, most tokens first.

## Cross-file deduplication

A response copied into several files (resumed or forked sessions) is counted once, at aggregation time, using each event's stable key. Copies merge with the same rules as Claude streaming copies: per-field maxima (the response time included), split-derived input replacing bare input, and the raw 1-hour cache-write maximum clamped only after merging. Events keep the input-split provenance and the raw split across files for this.

A keyed copy that carries only a raw cache split stays pending for later copies. Events whose merged buckets are still empty are excluded from totals, request counts and session lists.

## Known differences from tokscale

When a Codex counter restarts after compaction and the new total equals the request's own usage, toktop counts the request; tokscale treats it as a duplicate.
