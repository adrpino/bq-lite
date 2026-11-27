# Stage 1: Cargo-chef base
FROM rust:slim-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

# Stage 2: Recipe planner
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# Stage 3: Dependency builder
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json

RUN cargo chef cook --release --recipe-path recipe.json

# Build the main binary
COPY . .
RUN cargo build --release --bin bq-lite-server

# Stage 4: Minimal runtime container
FROM debian:bookworm-slim AS runtime
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/bq-lite-server /usr/local/bin/bq-lite-server

# Add secure non-root user
RUN groupadd -g 10001 bqlite && \
    useradd -u 10001 -g bqlite -m -s /bin/sh bqlite
RUN mkdir -p /app/data && chown -R bqlite:bqlite /app/data
VOLUME /app/data
USER bqlite

EXPOSE 9050 9060

# Standard environment defaults
ENV BQ_LITE_HOST=0.0.0.0
ENV BQ_LITE_PORT=9050
ENV BQ_LITE_GRPC_PORT=9060
ENV BQ_LITE_DATA_DIR=/app/data

# Labels to automatically link this image to your GitHub repo
LABEL org.opencontainers.image.source="https://github.com/adrpino/bq-lite"
LABEL org.opencontainers.image.description="Pure-Rust local BigQuery REST v2 & gRPC Storage API emulator"
LABEL org.opencontainers.image.licenses="MIT"

ENTRYPOINT ["/usr/local/bin/bq-lite-server"]
