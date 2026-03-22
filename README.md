# Animus

**The world's first AI-native operating system layer.**

Current "AI OS" projects build harnesses for AI to act like humans inside computers — clicking buttons, reading screens, navigating file systems designed for human cognition. But AI is digital. It doesn't need a GUI. It doesn't need a file browser. It needs a **nervous system**.

Animus provides that nervous system: native vector storage, persistent memory, ambient awareness, continuous identity, and autonomous reasoning — all built from the ground up for an AI Life Form (AILF).

## Architecture

Animus is a six-layer architecture:

| Layer | Crate | Purpose |
|-------|-------|---------|
| **Storage** | `animus-vectorfs` | AI-native vector-indexed storage (VectorFS) |
| **Memory** | `animus-mnemos` | Context assembly, consolidation, quality tracking |
| **Embedding** | `animus-embed` | Tiered embedding strategy (local + API) |
| **Core** | `animus-core` | Shared types, traits, error handling |
| **Sensorium** | *planned* | Ambient awareness with consent boundaries |
| **Cortex** | *planned* | Autonomous reasoning threads |
| **Telos** | *planned* | Goal system with human-controlled autonomy spectrum |

### VectorFS

Traditional file systems organize data by path — a human abstraction. VectorFS organizes data by **meaning**. Every piece of knowledge is a **Segment**: an embedding vector paired with content, metadata, confidence scores, lineage tracking, and consent policies.

Segments live in three tiers:
- **Hot** — actively in the LLM context window
- **Warm** — vector-indexed, retrievable in <10ms via HNSW
- **Cold** — archived, retained for consolidation or future relevance

A background `TierManager` continuously scores segments on relevance, recency, access frequency, and confidence — promoting and demoting automatically.

### Mnemos (Memory Manager)

Mnemos assembles optimal context windows for LLM reasoning:
1. Anchor segments (conversation history, active goals)
2. Similarity retrieval from Warm storage via HNSW
3. Token budget enforcement with intelligent eviction
4. Eviction summaries — pointers back to full knowledge, not data loss

A `Consolidator` merges near-duplicate segments. A `QualityTracker` adjusts confidence based on human feedback (acceptances vs. corrections).

### Embedding Strategy

Three-tier embedding for balancing capability and portability:
- **Tier 1**: EmbeddingGemma (300M) — fast, local, always available
- **Tier 2**: Nomic Embed Multimodal 3B — richer semantics, local GPU
- **Tier 3**: Gemini Embedding API — highest quality, cloud fallback

## Building

```bash
cargo build --all
cargo test --all
cargo clippy --all -- -D warnings
```

### Container Validation

```bash
podman build -t animus-dev -f Containerfile .
podman run --rm animus-dev
```

## Project Status

**Phase 1 (VectorFS + Mnemos)** — complete. All core storage, memory management, tier management, consolidation, and quality tracking are implemented and tested with 33 tests (21 integration + 12 unit).

**Phase 2 (Sensorium + Cortex + Telos)** — planned.

## Design Documents

- [Genesis Conversation](docs/00-genesis-conversation.md) — the full design rationale
- [Architecture Specification](docs/specs/2026-03-21-animus-design.md) — formal design spec
- [Phase 1 Plan](docs/plans/2026-03-21-phase1-vectorfs-mnemos.md) — implementation plan

## License

MIT
