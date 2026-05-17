FROM rust:1.95.0-slim-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev cmake build-essential && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY src src
COPY cfg.yml .
COPY Cargo.* ./
RUN cargo install --path .
CMD ["ems-industrial-gateway"]
