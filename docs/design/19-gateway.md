# 19 — Gateway: models, accounts, quotas, cache, over HTTP and ACP

Requirements (2026-10-12): the gateway is an axum proxy inside the operator daemon; ACP sessions are durable in the
database; agents run in ready-built environment images (Codex and Claude Code as a layer, health-checked); callers
that speak the OpenAI (Codex) or Anthropic (Claude) API get exact and semantic caching with replay, a pool of models
across accounts with quotas, API mixing, and a budget per model per provider; the same calls work over ACP.

Replaces the "AI gateway" section of 12-operator and "Model calls, accounts and quotas" of 14-sessions.

## Where it runs

Inside `legionctl-operator` (decided 2026-10-12), on `$XDG_RUNTIME_DIR/legionctl/gateway.sock` and a localhost port
for agents. Trade-off accepted: an operator restart interrupts model calls in flight (callers retry); one process,
one database, one config, instead of the separate `legionctl-gateway` in 15-layout.

## Built (2026-10-13, G1)

`crates/legionctl-operator/src/gateway.rs`: axum on `127.0.0.1:<gateway.port>` (off with 0, the default) inside the
operator; `/v1/messages` (Anthropic), `/v1/chat/completions` and `/v1/responses` (OpenAI). Accounts name a provider
and a base URL; `keyring: true` takes the key from `secret-tool lookup legionctl-account <name>`, and such an account
serves only callers that send `x-legionctl-token` (the per-user token in `~/.local/state/legionctl/gateway-token`,
mode 600), because any local user reaches a local port. Without a key the caller's own credentials go through (Claude
Code and Codex logins). Pool (model, then `*`) with fallback on 429; `account/model` without fallback; money budgets
per `account/model` (G2 part 1, below). Exact cache in `operator.db`
(namespace `gateway` of the disposable cache, note 12; key = BLAKE2b of the path and the request) with replay of JSON and SSE bodies, `x-legionctl-cache:
off|refresh`; a usage row per call (`gateway_usage`; `legionctl gateway usage`). The usage row is written before the
caller's stream ends, so a budget check right after sees it. Upstream TLS is native (OpenSSL): rustls would bring
ISC-licensed crates that cargo-deny does not allow yet. Not built: translation of the OpenAI Responses API, durable
ACP sessions, session environments. Tests: a stand-in provider
(`tests/fixtures/fake-provider.py`) in `crates/legionctl/tests/gateway.rs`.

## Built (2026-10-13, G2 part 1: money budgets)

`gateway.prices`: model -> `{ "input", "output" }` in USD per million tokens; no prices are built in, because they
change. Each usage row has `cost_usd` (0 for a replay, a refusal, or a model without a price). `gateway.budgets`:
`account/model` -> `{ "day_usd", "month_usd" }` over rolling windows of 24 hours and 30 days; the account leaves the
pool when one is used, and an explicit call gets a 429 with the limit and the amount spent. A budget for a model
without a price makes the config invalid (the reason names the key), because its cost would always be 0.
Prompt-cache tokens (2026-10-14) count as input: Anthropic reports reads and writes beside `input_tokens`, `OpenAI`
reads inside `prompt_tokens`/`input_tokens`; a price's optional `cache_read` and `cache_write` price them, and
without those they cost `input` (a budget counts too much, never too little; most of an agent's input is cache
reads, so leaving them out let a budget pass far above its limit).

## Built (2026-10-13, G2 part 2: API translation)

`crates/legionctl-operator/src/translate.rs`: an Anthropic caller (`/v1/messages`) reaches an OpenAI account
(`/v1/chat/completions`), and the other way round, by name (`oa/gpt-x`) or through the pool. Translated: system
text, text and image content, tool definitions, tool choice and parallel calls, tool calls and their results (an
Anthropic tool error becomes `[tool error] ` before the OpenAI tool text), stop sequences, `max_tokens`, temperature,
`top_p`, the user id, JSON answers, error bodies, and SSE streams event by event (tool-call arguments as they arrive;
Anthropic's `message_delta` gets both token counts; an OpenAI caller gets the usage chunk only when it asked for
it). Refused with the reason, never changed: keys with no equivalent (`top_k`, `response_format`, `n` above 1, ...),
thinking blocks, documents, server tools, and a Chat Completions request without `max_tokens` (Anthropic has no
default). `cache_control` hints are dropped (cost, not answer). Only a keyring account can serve a translated call:
the caller's own key is for the other provider. Usage and cost count the provider's bytes, so a caller's shape cannot
hide tokens. The cache keeps and replays the caller's shape. The OpenAI Responses API (`/v1/responses`) is not
translated yet: refused with the reason. Tests: both directions, JSON and streams, with tool calls, against the
stand-in provider (`crates/legionctl/tests/gateway.rs`).

## Built (2026-10-13, G2 part 3: semantic cache)

`gateway.semantic`: `{ "account", "model", "threshold" }`, null (off) by default. The account speaks the OpenAI API
(`/v1/embeddings`; its keyring key, or none for a local model). A caller that sends `x-legionctl-cache: semantic`
gets the exact cache first; on a miss the prompt text (system, instructions, each message's role and text) is
embedded, and the kept answer with the nearest prompt replays when it reaches the threshold. Only the prompt may
differ: the scope is the API path, the model and a hash of the rest of the request (stream mode, tools, parameters),
so a JSON answer never replays to a stream or to a request with other tools. The embedding account follows the same
rules as any account: a keyring key needs the gateway token, and its budget counts (`x-legionctl-cache: semantic`). On a miss the embedding is kept with the new answer and expires with it.
Each embedding call is a usage row of the embedding account (with its cost when `gateway.prices` has the model). An
embedding failure is logged and the call goes on without the semantic cache. Embeddings are kept in `operator.db`
(the cache's `vectors` beside the entries) and searched in full: the cache holds one row per kept answer, which stays small.

## Model addressing

| Caller writes `model` as | The gateway uses |
| --- | --- |
| `claude-opus-5-5`, `gpt-5.x`, ... (plain) | the model pool: an ordered, editable list of (account, model) entries for that model family; the first entry with quota left serves the call |
| `<provider-or-account>/<model>` (for example `work-anthropic/claude-opus-5-5`, `openai/gpt-5.x`) | exactly that account and model; no fallback through the pool |

- Accounts are named credentials (keyring) for a provider: Anthropic API, Claude subscription, OpenAI API, Codex
  subscription.
- API mixing: an OpenAI-shaped request may resolve to an Anthropic model and the other way round; the gateway
  translates request and response shapes (messages, tools, streaming events) and validates both against the
  provider's schema. A request that cannot be expressed in the target API is refused with the reason, never
  silently changed.

## Quotas and budgets

| Limit | Scope | When reached |
| --- | --- | --- |
| Provider rate limits | per account, from response headers | the pool moves to the next entry; explicit `<account>/<model>` gets a 429 with the reset time |
| Quota | per account and API (tokens or requests per window) | same as above |
| Budget | per model per provider (money per day and per month) | the entry leaves the pool until the period resets; explicit calls are refused |
| Session budget | per ACP session (14) | the session pauses |

Usage rows (session, account, model, tokens in/out, cache hits, cost, latency) go to `operator.db`.

## Cache and replay

| Cache | Key | Use |
| --- | --- | --- |
| Exact | hash of the normalized request (model, messages, tools, parameters) | replay the stored response, streamed the same way |
| Semantic | embedding of the prompt, nearest neighbour above a per-model threshold | replay when the caller allows it (`x-legionctl-cache: semantic`) |

- Each cache entry can be refreshed (call again, replace) or disabled (never replay this key); rules per model, per
  account and per caller decide which caches apply. TTL per model.
- Responses are stored in `operator.db` (metadata) and a blob store under `~/.cache/legionctl/gateway/`
  (bodies); both are per user and excluded from backups unless chosen.

## Over ACP

ACP sessions (14) are durable rows in `operator.db`: session, agent, image, workspace, state, prompts and updates,
so a session resumes after an operator restart. Code that runs inside a session uses the OpenAI or Anthropic SDK
pointed at the gateway (base URL from the session's config mount), so its own model calls go through the same pool,
quotas, budgets and cache, and are recorded under the session.

## Session environments

ACP sessions run in images built and run as 20-images describes:

- a **base environment image**: toolchains and the repository's build dependencies, with mapped entrypoints (build,
  test, run targets);
- an **acp-sessions variant**: the base plus Codex and Claude Code as one layer, with a health check that calls
  each agent's version and a short ACP `initialize` before the image is marked ready;
- **config mounts** at start: gateway base URL and token, agent settings, MCP proxy endpoints (14), the dossier
  (18). Nothing secret is baked into an image.
