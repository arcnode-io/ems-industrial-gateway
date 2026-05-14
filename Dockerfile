FROM rust:1.95.0-slim-bookworm
WORKDIR /app
COPY src src
COPY cfg.yml .
COPY Cargo.* ./
RUN cargo install --path .
CMD industrial-gateway
