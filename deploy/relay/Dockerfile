# syntax=docker/dockerfile:1

ARG RUST_BUILDER_IMAGE=rust:1.97-slim-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim

FROM ${RUST_BUILDER_IMAGE} AS builder

ARG WASM_BINDGEN_CLI_VERSION=0.2.125
ARG RUST_STABLE_TOOLCHAIN=1.97.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates clang cmake curl git libssl-dev nodejs pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && if ! rustc --version | grep -q '^rustc 1\.97\.'; then \
         rustup toolchain install "${RUST_STABLE_TOOLCHAIN}" --profile minimal \
         && rustup default "${RUST_STABLE_TOOLCHAIN}"; \
       fi \
    && rustup toolchain install nightly-2026-07-24 --profile minimal --target wasm32-unknown-unknown \
    && cargo install wasm-bindgen-cli --version "${WASM_BINDGEN_CLI_VERSION}" --locked

WORKDIR /workspace
COPY . .

RUN node apps/web/scripts/build.mjs --release
RUN WEB_BUILD_ID="$(node -p "JSON.parse(require('fs').readFileSync('apps/web/dist/build.json')).buildId")" \
    && WEB_GIT_COMMIT="$(node -p "JSON.parse(require('fs').readFileSync('apps/web/dist/build.json')).gitCommit")" \
    && VIBEX_RELAY_WEB_BUILD_ID="${WEB_BUILD_ID}" \
       VIBEX_RELAY_WEB_GIT_COMMIT="${WEB_GIT_COMMIT}" \
       cargo build --release -p vibex-relay-server --locked \
    && node --input-type=module -e "\
      import { readFileSync, writeFileSync } from 'node:fs'; \
      const webBuild = JSON.parse(readFileSync('apps/web/dist/build.json', 'utf8')); \
      writeFileSync('target/relay-image.json', JSON.stringify({ \
        schemaVersion: 'vibex-relay-image.v1', \
        serverVersion: webBuild.packageVersion, \
        sourceRevision: webBuild.gitCommit, \
        webBuild \
      }, null, 2) + '\\n');"

FROM ${RUNTIME_IMAGE} AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /app --shell /usr/sbin/nologin vibex

WORKDIR /app
COPY --from=builder /workspace/target/release/vibex-relay-server /usr/local/bin/vibex-relay-server
COPY --from=builder --chown=vibex:vibex /workspace/apps/web/dist /app/web
COPY --from=builder --chown=vibex:vibex /workspace/target/relay-image.json /app/relay-image.json

ENV VIBEX_RELAY_BIND_ADDR=0.0.0.0:9700
ENV VIBEX_RELAY_WEB_STATIC_DIR=/app/web
EXPOSE 9700

USER vibex
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:9700/health >/dev/null || exit 1

ENTRYPOINT ["vibex-relay-server"]
