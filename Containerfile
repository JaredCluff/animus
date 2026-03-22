FROM docker.io/library/rust:1.87-bookworm

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    cmake \
    && rm -rf /var/lib/apt/lists/*

# Run as non-root user
RUN useradd -m -s /bin/bash animus
USER animus

WORKDIR /home/animus/app

# Copy manifests first for layer caching
COPY --chown=animus:animus Cargo.toml Cargo.lock ./
COPY --chown=animus:animus crates/ crates/

# Build dependencies first (cache layer)
RUN cargo build 2>/dev/null || true

# Copy source
COPY --chown=animus:animus . .

# Build and test
CMD ["cargo", "test", "--all"]
