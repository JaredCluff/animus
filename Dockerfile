# ── Stage 1: Build ───────────────────────────────────────────────────────────
FROM rust:bookworm AS builder

WORKDIR /src

# Cache dependency compilation by copying manifests first
COPY Cargo.toml Cargo.lock ./
COPY crates/animus-core/Cargo.toml          crates/animus-core/
COPY crates/animus-vectorfs/Cargo.toml      crates/animus-vectorfs/
COPY crates/animus-mnemos/Cargo.toml        crates/animus-mnemos/
COPY crates/animus-embed/Cargo.toml         crates/animus-embed/
COPY crates/animus-cortex/Cargo.toml        crates/animus-cortex/
COPY crates/animus-sensorium/Cargo.toml     crates/animus-sensorium/
COPY crates/animus-interface/Cargo.toml     crates/animus-interface/
COPY crates/animus-runtime/Cargo.toml       crates/animus-runtime/
COPY crates/animus-federation/Cargo.toml    crates/animus-federation/
COPY crates/animus-tests/Cargo.toml         crates/animus-tests/

# Stub out all lib/main entry points so the dep layer caches correctly
RUN for crate in animus-core animus-vectorfs animus-mnemos animus-embed animus-cortex \
        animus-sensorium animus-interface animus-federation animus-tests; do \
      mkdir -p crates/$crate/src && \
      echo "pub fn _stub() {}" > crates/$crate/src/lib.rs; \
    done && \
    mkdir -p crates/animus-runtime/src && \
    echo "fn main() {}" > crates/animus-runtime/src/main.rs

RUN cargo build --release --bin animus 2>&1 || true

# Now copy real source and rebuild
COPY crates/ crates/
RUN touch crates/*/src/*.rs crates/*/src/**/*.rs 2>/dev/null || true
RUN cargo build --release --bin animus

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user for running animus
RUN useradd -m -s /bin/bash animus
USER animus

WORKDIR /home/animus

COPY --from=builder /src/target/release/animus /usr/local/bin/animus

# Data directory — mount a volume here for persistence
RUN mkdir -p /home/animus/.animus
VOLUME ["/home/animus/.animus"]

# Health endpoint
EXPOSE 8080

# Default environment (override at runtime)
ENV ANIMUS_DATA_DIR=/home/animus/.animus
ENV ANIMUS_HEALTH_BIND=0.0.0.0:8080
ENV ANIMUS_OLLAMA_URL=http://localhost:11434
ENV ANIMUS_LOG_LEVEL=animus=info

ENTRYPOINT ["/usr/local/bin/animus"]
