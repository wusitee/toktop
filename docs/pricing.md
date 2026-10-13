# Pricing

## Price source

Prices come from LiteLLM's [`model_prices_and_context_window.json`](https://github.com/BerriAI/litellm), downloaded at most once a day. Only chat-model rates are kept, cached as a reduced table in `$XDG_CACHE_HOME/toktop/prices.json`. Older caches missing newer fields stay readable.

## Model lookup

A tool's model id is resolved in this order, trying aliases for each candidate:

1. Normalize dotted Claude versions (`claude-sonnet-4.6` → `claude-sonnet-4-6`) and resolve tool machine-id aliases from tokscale's static map (`MODEL_PLACEHOLDER_M35` → `claude-sonnet-4-6`), in `src/pricing/aliases.rs`.
2. The full id, so `azure/…` keeps Azure's price.
3. The bare name, preferring first-party provider entries.
4. The id with trailing `-suffix` segments dropped (`claude-opus-4-6-thinking` → `claude-opus-4-6`). Gemini 3 and 3.1 Pro also try the catalog's `-preview` spelling.

Models without a price count as zero cost and are listed as unpriced.

## Cost

Each request is priced on its own. Output and reasoning use the output rate. Cache reads and 5-minute cache writes use their own rates, falling back to the input rate; 1-hour cache writes fall back to the 5-minute rate, then the input rate. Long-context tiers (above 200K or 272K prompt tokens) apply per request, including tiered cache-write rates; a rate the tier omits falls back to the base rate.

## Service tiers

Codex priority (fast) requests use LiteLLM's explicit priority rates. Long-context priority rates apply only when the model's long-context tier applies; models without a long-context tier keep their base priority rates at any prompt size. A missing priority rate falls back to twice the standard rate for that request's context length, matching tokscale's premium; cache-write rates double too. Other tiers (default, flex, unspecified) use standard pricing.
