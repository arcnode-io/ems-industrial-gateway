# Multi-stage: heavy rust toolchain + cmake/libssl-dev only in builder,
# runtime is a slim debian with the static-ish ems-industrial-gateway
# binary + the cfg.yml it reads at boot. Drops the image from ~600MB
# (cargo install in slim-bookworm) to ~50MB (debian-slim + binary).
FROM rust:1.95.0-slim-bookworm AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev cmake build-essential && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY src src
COPY cfg.yml .
COPY Cargo.* ./
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl3 ca-certificates && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/ems-industrial-gateway /usr/local/bin/
COPY cfg.yml .
CMD ["ems-industrial-gateway"]
