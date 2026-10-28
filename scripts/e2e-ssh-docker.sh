#!/bin/sh
# Real Linux SSH provisioning E2E against a disposable Ubuntu sshd container.
#
# Usage:
#   scripts/e2e-ssh-docker.sh <server-tarball>
#
# The caller builds/packages the server artifact; this script only owns the
# fixture, environment, and test invocation. Docker must be available.
set -eu

ARTIFACT=${1:?usage: scripts/e2e-ssh-docker.sh <server-tarball>}
ARTIFACT=$(cd "$(dirname "$ARTIFACT")" && pwd)/$(basename "$ARTIFACT")
ROOT=$(mktemp -d "${TMPDIR:-/tmp}/vibex-ssh-e2e.XXXXXX")
CONTAINER=vibex-ssh-e2e

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  rm -rf "$ROOT"
}
trap cleanup EXIT INT TERM

ssh-keygen -q -t ed25519 -N '' -f "$ROOT/id"
cat > "$ROOT/config" <<CFG
Host vibex-e2e
  HostName 127.0.0.1
  Port 2222
  User dev
  IdentityFile $ROOT/id
  IdentitiesOnly yes
  StrictHostKeyChecking yes
  UserKnownHostsFile $ROOT/known_hosts
CFG

docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
docker run -d --init --name "$CONTAINER" -p 127.0.0.1:2222:22 ubuntu:24.04 sleep infinity >/dev/null
docker exec "$CONTAINER" sh -c \
  'apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq openssh-server ca-certificates iproute2 >/dev/null 2>&1 && useradd -m -s /bin/bash dev && mkdir -p /home/dev/.ssh /run/sshd'
cat "$ROOT/id.pub" | docker exec -i "$CONTAINER" tee /home/dev/.ssh/authorized_keys >/dev/null
docker exec -u root "$CONTAINER" sh -c \
  'chown -R dev:dev /home/dev/.ssh; chmod 600 /home/dev/.ssh/authorized_keys; /usr/sbin/sshd'
ssh-keyscan -p 2222 127.0.0.1 2>/dev/null >> "$ROOT/known_hosts"

VIBEX_SSH_E2E=1 \
VIBEX_SSH_E2E_CONFIG="$ROOT/config" \
VIBEX_SSH_E2E_IDENTITY="$ROOT/id" \
VIBEX_SSH_E2E_ARTIFACT="$ARTIFACT" \
  cargo test -p vibex-desktop --test ssh_provisioning_e2e -- --ignored --nocapture
