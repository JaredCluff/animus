FROM docker.io/library/rust:1.86-bookworm

RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    cmake \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy manifests first for layer caching
COPY Cargo.toml ./
COPY crates/ crates/

# Build dependencies first (cache layer)
RUN cargo build 2>/dev/null || true

# Copy everything
COPY . .

# Build and test
CMD ["cargo", "test", "--all"]
