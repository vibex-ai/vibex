# Model Routing and Gateway Observability

## Scope

This document describes the third routing wave: model-level route groups,
ordered rules and classifiers, patience and first-token handling, the route
builder, and the Gateway dashboard/live routing card. Durable contracts live in
`.trellis/spec/backend/provider-config.md`; this is the operator-facing overview.

The gateway remains one loopback HTTP endpoint. It owns no Provider list: the
Providers it can call are the members of enabled route groups. Request routing
state (cooldowns, pins, rotation, inflight) never enters an Agent process
fingerprint.

## Route Kinds

- `ProviderGroup` is the original shape: members are whole Providers, and the
  group offers the intersection of their enabled catalogues.
- `ModelGroup` is additive: members are `(Provider, upstream model)` pairs and
  the group exposes one Agent-facing id, `agent_model_id`. The id may not
  contain `/`; a request is addressed as `group/<route>/<agent_model_id>`.
- `Direct` names one Provider and bypasses the gateway.

A model group needs at least one enabled member. A duplicate
`(Provider, model)` pair, a zero weight, a zero member timeout, or a Manual
strategy without a picked member is rejected at save time and again by
`ModelRoute::validate`.

## Aliases

`ModelProviderMetadata::model_aliases` maps a local alias to an upstream model
id. A model-group member and a classifier `provider/model` pair may name either
the catalogue id or the alias; the gateway resolves the alias only when it
builds the outbound body. This lets the Agent use a stable handle while the
vendor receives its own model name. A same-Provider duplicate alias is refused.

`ModelGroup.agent_model_id` is the route-level alias. Requests naming a member's
upstream id directly are refused: they would bypass the group's rules, order,
and fallback.

## Strategies and Weights

`RouteStrategy` has seven values, all computable from local observations:

| Strategy | Order |
| --- | --- |
| `Ordered` | configured member order |
| `RoundRobin` | advances one member per request |
| `LeastUsed` | fewest locally observed tokens |
| `Smart` | cooling, then p50, then inflight, then usage |
| `LowestLatency` | p50, then p95 |
| `Weighted` | interleaved cycle proportional to weight, rotated by request |
| `Manual` | only `pick_member_id`; no fallback |

Weights are 1-100 and are kept when the strategy changes. The weighted cycle
interleaves units round by round (3:1 reads A, B, A, A) instead of emitting
three A's in a row.

## Rules, Classifier, and Effort Auto

A rule's first match wins. Conditions cover Agent id, estimated context tokens,
images, requested effort, intent, compaction, time window, folder fragments, and
except-folder vetoes. A rule names the member to try first and optional `more`
members; unnamed members keep the strategy order, so a rule changes preference
without removing a fallback. A pause rule is limited to time/Agent/folder
conditions and refuses the turn before any Provider call.

A non-local IANA time zone is stored but not evaluated yet; the rule does not
match rather than silently using the wrong clock. Context tokens are an estimate
(about four characters per token), not a vendor tokenizer count.

When a route names a classifier and has an intent rule or `effort = "auto"`, the
gateway makes one small non-streaming Chat request at the start of the prepared
request:

- Timeout: 8 seconds.
- It never enters the request ledger or the inflight table.
- Its intent feeds rule matching; its effort replaces the Agent's requested
  effort unless the member pins one.
- Any failure (missing model/Key, timeout, bad status, unusable reply) leaves
  the default order in place and records `classifierError` on the decision
  trace.

## Patience and the First Token

Patience retries only recoverable failures: 408/429/5xx, all-keys-cooling, and
first-token timeouts. The waits are 2/4/8/16/30 seconds, bounded by
`patienceMs` (off by default, maximum four minutes). Each retry re-prepares the
call so Key cooldowns and member rotation are refreshed.

For a streaming request with another member left, the gateway reads the first
body byte before committing. A timeout or empty body is recorded as a skipped
member and the next member is tried. The last member waits indefinitely because
there is nowhere to fail over to.

## Builder

The Providers page's model-group editor exposes:

- an Agent-facing model id and a `(Provider, model/alias)` palette;
- an ordered pipeline with enable, weight, effort, and member context/first-token
  overrides;
- policy: strategy, pin, failover, patience, token ceiling, Unlists/Sink;
- templates (stable first, lightweight, low latency, same model) and
  same-model suggestions, all as drafts until Save. "Same model" compares the
  catalogue's canonical id, so Providers that resell one model under different
  upstream ids still group; a model the catalogue does not know falls back to
  its own id.

A dry-run preview of the routing decision is not part of the editor: the
Gateway's live card and ledger show what the router actually did.

`Clone` copies a group as a disabled, unbound record, remapping member ids and
the rules that name them. It copies no Secret because a route owns none.

## Gateway Dashboard and Live Routing

The Gateway page shows uptime from the listener's start time, QPS, success rate,
p50/p95, input/output/cache tokens, failovers, inflight, and top
Providers/models/errors for 1h/24h/7d windows. Tokens are locally observed.

The live routing card derives from `GatewayInflight` plus
`Began`/`FirstToken`/`Ended` events only. It draws the newest 24 attempts and
aggregates the rest, colours waiting amber and first-token green, hides direct
Providers (they produce no gateway events), and exposes a static list for reduced
motion. There is no random particle traffic and no per-token animation.

## Provider Reliability P2

- **Cooldown overrides** are keyed by failure kind (`rate`, `upstream`, `proxy`,
  `credit`, `quota`, `other`), 1 ms to 1 day. `auth` is rejected because a
  rejected credential does not heal with time. The override is read when a
  failure is recorded, so it applies from the next rest onward.
- **A model-list fetch is a picker, not a write.** Asking a shared Provider for
  its catalogue fills a dialog with one row per advertised Model; the reader
  chooses which of them the Provider should offer and can open any row's
  settings before confirming. Nothing is written until the confirm, and the
  confirm adds the chosen Models rather than replacing the list, so a row the
  user authored keeps its capabilities, display name, and enabled state.
- **AutoInclude** is the shared Provider's `autoRefreshModels` flag. It only
  decides how the picker opens: with the flag, the vendor's new Models arrive
  already checked; without it, every row starts unchecked. Either way the
  confirm is what adds them, so discovery can never add a Model the reader did
  not see.

There is deliberately no Provider budget, quota, balance field, or compatibility
switch in this wave.

## Migration and Rollback

Migration 83 adds the model-group columns and member overrides additively; every
column is nullable or defaulted, and the migration is column-by-column so a
rewound version row over a new schema re-runs safely. Existing ProviderGroups
read back unchanged.

Rollback is additive too: disable or delete model groups and continue using
ProviderGroups/Direct routes. The UI flag is not required for data safety
because the old rows are untouched; a downgrade that predates migration 83 must
restore the database snapshot, as with every schema change.

## Limits

- No cost, price, quota, balance, or budget anywhere in this surface.
- No CSV/JSON export, Share/Import text, or backup of route groups.
- No mobile builder; desktop remains the authoring surface.
- The live card is live-only; historical replay from Requests traces is a later
  addition and must not be presented as live traffic.

## Verification

```bash
cargo fmt --all -- --check
cargo clippy -p vibex-core -p vibex-db -p vibex-model-gateway \
  -p vibex-config-switch -p vibex-desktop --all-targets -- -D warnings
cargo test -p vibex-core
cargo test -p vibex-db --lib
cargo test -p vibex-config-switch
NO_PROXY=127.0.0.1,localhost cargo test -p vibex-model-gateway
cargo test -p vibex-desktop --lib management
```

Manual checks: a legacy ProviderGroup must not regress; a model group must fail
over across two different upstream models; a 10k-row ledger and 20-30 concurrent
inflight requests must stay responsive; reduced motion must show the static
list.
