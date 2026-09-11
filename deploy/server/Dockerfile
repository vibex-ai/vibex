# syntax=docker/dockerfile:1

ARG RUST_BUILDER_IMAGE=rust:1.97-slim-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim

FROM ${RUST_BUILDER_IMAGE} AS builder

ARG RUST_STABLE_TOOLCHAIN=1.97.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates clang cmake git libdbus-1-dev libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && if ! rustc --version | grep -q '^rustc 1\.97\.'; then \
         rustup toolchain install "${RUST_STABLE_TOOLCHAIN}" --profile minimal \
         && rustup default "${RUST_STABLE_TOOLCHAIN}"; \
       fi

WORKDIR /workspace
COPY . .

# The server builds against the workspace lockfile like every other Vibex app.
# GPUI and the desktop/mobile frontends are never part of this build graph:
# vibex-desktop-runtime is a pure async crate.
RUN cargo build --release -p vibex-server --locked

FROM ${RUNTIME_IMAGE} AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git libdbus-1-3 openssh-client \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /data --shell /usr/sbin/nologin vibex

WORKDIR /data
COPY --from=builder /workspace/target/release/vibex-server /usr/local/bin/vibex-server

# Defaults describe the safest posture: loopback HTTP bound to the container's
# loopback interface. Bridge-network users publish that port explicitly (e.g.
# `-p 127.0.0.1:8765:8765`); host-network users share the host's loopback
# directly. Anything reachable from outside the machine must set
# VIBEX_DEPLOYMENT_MODE and a TLS policy explicitly; RemoteGatewayConfig::validate
# enforces the mode/bind/TLS combination at startup (loopback mode refuses
# non-loopback binds), so a misconfigured container refuses to boot instead of
# silently exposing an unauthenticated runtime.
ENV VIBEX_HOME=/data \
    VIBEX_DB_PATH=/data/vibex.db \
    VIBEX_BIND_ADDR=127.0.0.1:8765 \
    VIBEX_DEPLOYMENT_MODE=loopback \
    VIBEX_TLS_MODE=loopback_http \
    VIBEX_APPLICATION_ID=dev.vibex.server

# Git is provisioned on purpose: cloud sessions run against repositories that
# live inside VIBEX_HOME, so the runtime needs fetch/push just like the
# desktop. No provider credentials are baked into the image; they are entered
# by the owner through a paired device after boot.
EXPOSE 8765

USER vibex
VOLUME ["/data"]

# /api/v2/info is unauthenticated but rate-limited and answers with the
# server identity, so a 200 proves the listener, database, and identity
# subsystems came up.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=5 \
    CMD curl -fsS http://127.0.0.1:8765/api/v2/info >/dev/null || exit 1

ENTRYPOINT ["vibex-server"]
CMD ["serve"]
