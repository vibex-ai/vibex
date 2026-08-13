# syntax=docker/dockerfile:1

ARG RUST_BUILDER_IMAGE=rust:1.97-slim-bookworm
ARG RUNTIME_IMAGE=debian:bookworm-slim

FROM ${RUST_BUILDER_IMAGE} AS builder

ARG RUST_STABLE_TOOLCHAIN=1.97.0

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates clang cmake git libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/* \
    && if ! rustc --version | grep -q '^rustc 1\.97\.'; then \
         rustup toolchain install "${RUST_STABLE_TOOLCHAIN}" --profile minimal \
         && rustup default "${RUST_STABLE_TOOLCHAIN}"; \
       fi

WORKDIR /workspace
COPY . .

RUN cargo build --release -p vibex-relay-server --locked

FROM ${RUNTIME_IMAGE} AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --home-dir /app --shell /usr/sbin/nologin vibex

WORKDIR /app
COPY --from=builder /workspace/target/release/vibex-relay-server /usr/local/bin/vibex-relay-server

ENV VIBEX_RELAY_BIND_ADDR=0.0.0.0:9700
EXPOSE 9700

USER vibex
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD curl -fsS http://127.0.0.1:9700/health >/dev/null || exit 1

ENTRYPOINT ["vibex-relay-server"]
