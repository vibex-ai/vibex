#!/usr/bin/env bash
# Fixture hook for scripts/e2e-workflows.mjs. Delegates to the local
# e2e_gateway_harness control API; prints the fixture JSON on setup.
set -euo pipefail
action="${1:?setup|cleanup}"
target="${2:-unknown}"
transport="${3:-unknown}"
port="${VIBEX_E2E_CONTROL_PORT:?VIBEX_E2E_CONTROL_PORT is required}"
curl -fsS -X POST "http://127.0.0.1:${port}/fixture/${action}" \
  -H 'content-type: application/json' \
  -d "{\"target\":\"${target}\",\"transport\":\"${transport}\"}"
