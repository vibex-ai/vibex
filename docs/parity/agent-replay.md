# Agent Import And Replay Fixtures

Online Agent sessions run through managed ACP adapters. The
`vibex-agent-claude` and `vibex-agent-codex` crates additionally provide
provider-specific, read-only import and replay adapters for external session
transcripts.

Fixtures live under each crate's `tests/fixtures/parity/<capability>/`
directory:

- `input.jsonl` contains sanitized provider transcript input.
- `expected_timeline.json` contains the canonical `TimelinePayload` sequence.
- `meta.json` describes fixtures that exercise metadata or capability contracts
  without replaying a transcript.

Replay is deterministic and provider-free. It does not start an Agent, load a
provider SDK, or provide an alternate online runtime. Fixture scans reject real
home paths and credential-shaped values.

Covered inputs include streamed messages and reasoning, tool and command
activity, file changes, plans, permissions, collaboration and MCP tools,
attachments, model metadata, session resume metadata, and external transcript
imports. ACP may emit richer canonical events than an imported transcript; the
compatibility requirement is that supported information is not lost or
misclassified.

Run the replay suites with:

```bash
cargo test -p vibex-agent-claude --test parity_replay --locked
cargo test -p vibex-agent-codex --test parity_replay --locked
```

Regenerate a reviewed fixture only through the env-gated test path:

```bash
UPDATE_PARITY_FIXTURES=1 cargo test -p vibex-agent-claude --test parity_replay --locked
VIBEX_PARITY_RECORD=1 cargo test -p vibex-agent-codex --test parity_replay --locked -- --ignored
```
