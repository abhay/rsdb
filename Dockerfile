FROM oven/bun:1 AS web
WORKDIR /app

COPY package.json bun.lock tsconfig.json ./
COPY web ./web
COPY scripts ./scripts
RUN bun install --frozen-lockfile
RUN bun run build:web

FROM rust:1.95-slim AS builder
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock package.json bun.lock tsconfig.json ./
COPY crates ./crates
COPY web/src ./web/src
COPY web/static ./web/static
COPY --from=web /app/web/dist ./web/dist

RUN cargo build --release -p rsdb-aggregate --bin rsdb-aggregate

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/rsdb-aggregate /usr/local/bin/rsdb-aggregate
COPY deploy/fly/allowlist.txt /etc/rsdb/allowlist.txt

ENV RSDB_AGGREGATE_HOST=0.0.0.0
ENV RSDB_AGGREGATE_PORT=8080
ENV RSDB_AGGREGATE_DATA_DIR=/data/aggregate
ENV RSDB_AGGREGATE_RETENTION_HOURS=72
ENV RSDB_AGGREGATE_HOT_MAX_MB=250
ENV RSDB_ALLOWLIST=/etc/rsdb/allowlist.txt

EXPOSE 8080
CMD ["rsdb-aggregate", "serve"]
