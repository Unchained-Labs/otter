FROM rust:1.85-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.toml
COPY otter-core/Cargo.toml otter-core/Cargo.toml
COPY otter-server/Cargo.toml otter-server/Cargo.toml
COPY otter-worker/Cargo.toml otter-worker/Cargo.toml
COPY vendor vendor
COPY otter-core/src otter-core/src
COPY otter-server/src otter-server/src
COPY otter-worker/src otter-worker/src
COPY otter-core/migrations otter-core/migrations

ARG BIN_NAME
RUN cargo build --release --bin otter-server --bin otter-worker

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl bash && \
    rm -rf /var/lib/apt/lists/*

RUN curl -LsSf https://mistral.ai/vibe/install.sh | bash && \
    ln -sf /root/.local/bin/vibe /usr/local/bin/vibe

WORKDIR /srv/otter
COPY --from=builder /app/otter-core/migrations /srv/otter/migrations
COPY --from=builder /app/target/release/otter-server /usr/local/bin/otter-server
COPY --from=builder /app/target/release/otter-worker /usr/local/bin/otter-worker

ENTRYPOINT ["/usr/local/bin/otter-server"]
