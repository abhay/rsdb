# syntax=docker/dockerfile:1.6

FROM oven/bun:1 AS web
WORKDIR /app

COPY package.json bun.lock tsconfig.json ./
COPY README.md AGENTS.md ./
COPY docs ./docs
COPY pi/README.md ./pi/README.md
COPY web ./web
COPY scripts ./scripts
RUN bun install --frozen-lockfile
RUN bun run build:web

FROM rust:1.95-slim-bookworm AS chef
WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked --version 0.1.71

FROM chef AS planner

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder

COPY --from=planner /app/recipe.json recipe.json
RUN RSDB_SKIP_WEB_BUNDLE_CHECK=1 cargo chef cook --release --recipe-path recipe.json -p rsdb-aggregate --bin rsdb-aggregate

COPY Cargo.toml Cargo.lock ./
COPY --from=web /app/package.json /app/bun.lock /app/tsconfig.json ./
COPY crates ./crates
COPY --from=web /app/web/src ./web/src
COPY --from=web /app/web/static ./web/static
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
