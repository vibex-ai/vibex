#!/usr/bin/env bash
# Recovery hook for scripts/e2e-workflows.mjs. Breaks and restores the
# active transport. Direct runs toggle the harness gateway listener; Relay runs
# additionally kill/restart the local self-hosted relay-server process so the
# client observes a real transport loss.
set -euo pipefail
action="${1:?disconnect|reconnect}"
target="${2:-unknown}"
transport="${3:-unknown}"
port="${VIBEX_E2E_CONTROL_PORT:?VIBEX_E2E_CONTROL_PORT is required}"

control() {
  curl -fsS -X POST "http://127.0.0.1:${port}/recovery/$1" \
    -H 'content-type: application/json' \
    -d "{\"target\":\"${target}\",\"transport\":\"${transport}\"}" >/dev/null
}

if [[ "${transport}" != "relay" ]]; then
  control "${action}"
  exit 0
fi

pidfile="${VIBEX_E2E_RELAY_PIDFILE:?VIBEX_E2E_RELAY_PIDFILE is required for relay}"
relay_bin="${VIBEX_E2E_RELAY_BIN:?VIBEX_E2E_RELAY_BIN is required for relay}"
relay_log="${VIBEX_E2E_RELAY_LOG:?VIBEX_E2E_RELAY_LOG is required for relay}"
relay_port="${VIBEX_E2E_RELAY_PORT:?VIBEX_E2E_RELAY_PORT is required for relay}"

if [[ "${action}" == "disconnect" ]]; then
  control disconnect
  if [[ -f "${pidfile}" ]]; then
    kill "$(cat "${pidfile}")" 2>/dev/null || true
    rm -f "${pidfile}"
  fi
else
  VIBEX_RELAY_BIND_ADDR="127.0.0.1:${relay_port}" \
  VIBEX_RELAY_MAX_TOTAL_CONNECTIONS=64 \
  VIBEX_RELAY_MAX_DEVICES_PER_ROOM=4 \
  VIBEX_RELAY_MAX_QUEUE_BYTES_PER_CONNECTION=262144 \
    nohup "${relay_bin}" >> "${relay_log}" 2>&1 &
  echo $! > "${pidfile}"
  for _ in $(seq 1 100); do
    if curl -fsS "http://127.0.0.1:${relay_port}/health" >/dev/null 2>&1; then
      break
    fi
    sleep 0.1
  done
  control reconnect
fi
