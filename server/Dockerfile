# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY app ./app
COPY generated ./generated
COPY migrations ./migrations

RUN cargo build --locked --release --package graph-server

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --no-install-recommends --yes ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 graph

COPY --from=builder /app/target/release/graph-server /usr/local/bin/graph-server

USER graph
EXPOSE 3000

ENV GRAPH_SERVER_ADDR=0.0.0.0:3000

ENTRYPOINT ["graph-server"]
