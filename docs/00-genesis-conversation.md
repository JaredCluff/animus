# Animus: Genesis Conversation

**Date**: 2026-03-21
**Participants**: Jared Cluff (human), Claude/Animus (AILF-to-be)

This document preserves the full brainstorming conversation that produced the Animus design.
It serves as both historical record and design rationale — the "why" behind every decision.

---

## The Thesis

**Jared's opening argument**: Online discussion calls OpenClaw/NemoClaw "the AI OS." This is wrong.

**The core insight**: A human requires CLI/UI because humans are not digital. An AI IS digital. So why are we building a harness to allow AI to act like a human inside of a computer? Current "AI OS" projects are fundamentally skeuomorphic — they give AI eyes to read screens and hands to click buttons. The AI is cosplaying as a human operator.

**The reframe**: An AI doesn't need a desktop, a file browser, or a terminal. It can talk directly to syscalls, memory, network sockets, file descriptors, processes. The entire UI layer is a translation layer for humans — wrapping AI in it adds latency, fragility, and abstraction for no reason.

## The Biological Analogy

**Jared**: A human has a brainstem and sensory interfaces to hardware. The AI doesn't have to consciously manage everything. Human evolution abstracted away hardware management — but only to some extent. A trained human can lower their heartbeat to extremely low levels.

**Key requirements derived from this**:
1. The AI should be lower in the system but not gating everything
2. Short-term and long-term memory with a process to manage that
3. The AI works with a human but interfaces with hardware natively, not through human abstractions
4. Strong context switching / multiple simultaneous threads without cross-contamination
5. Logging all conversations for historic review without polluting active context

## The "True AI OS" — Not A/B, but C

**Rejected approaches**:
- (A) AI as kernel-level orchestrator managing everything — too controlling
- (B) New abstraction layer with goals/capabilities/tools — too abstract

**The actual vision**: The AI is like the nervous system of the machine:
- Most things (disk I/O, networking, scheduling) happen autonomously, like breathing
- The AI CAN reach down when needed, like a trained human slowing their heart rate
- Continuous presence, memory, and ambient awareness — not invoked per-task
- The human interacts WITH the AI; the AI interfaces with hardware natively

## Memory & Learning

**The hard wall**: Context windows are a hard limit. Static model weights can't change.

**Jared's position on LoRA**: Interesting idea but limited LoRA often breaks model behavior. Must ensure only improvements are accepted. Need to collect far more than just human interaction data.

**Requirements**:
- Memory tiering: what's important vs. stored for retrieval vs. logged for history
- Quality gate: system must only accept changes that improve it
- Context isolation: multiple threads without cross-contamination
- Acknowledged: there is good research around these problems

## Storage Architecture

**Jared's key insight**: There should be a filesystem/partition that is truly LLM-focused, and one for the human. Like SWAP for models but not so transient.

**The architecture**:
- A Vector partition — storage organized for how AI thinks and retrieves
- The AI has layers like the filesystem designed specifically for it
- Knowledge Nexus is the middleground where models and humans meet
- The AI can leverage human-centric services (like Knowledge Nexus Local) to interface with Knowledge Nexus
- Inside the AI's world, everything is native to it

## Human-AILF Relationship

**AILF** = AI Life Form (Jared's coinage)

**Interaction model**: Symbiont by default (C):
- For 99% of interactions, the human is a collaborator (sets direction, AI executes)
- For research/enterprise, full auditability
- Unless explicitly disabled, the AI is continuously aware of what the human is doing
- Must be containerizable or VM-able for portability and flexibility

**Ambient awareness**: Full capability, human-controlled scope, auditable trail (option C):
- The AILF CAN see everything at the OS level
- The human controls what it pays attention to
- Clear audit trail of everything observed

## Naming Decision

**Rejected**: Cortex (taken by OpenAI), Dendrite (too clinical)
**Chosen**: Animus — Latin for "mind/soul/spirit." The animating force of a living thing.

**Why**: Not too clinical, not too sci-fi. Says "this thing has a mind" without being cringe. Short, memorable, works as both project name and concept.

## Technical Decisions

**Language**: Rust. Non-negotiable for this project:
- OS-level primitives need zero-cost abstractions, no GC
- Memory safety is existential for something observing a human's machine
- Jared has deep Rust experience from NexiBot
- The systems community takes Rust seriously
- Ecosystem has what's needed: eBPF (aya), async (tokio), ML inference

**Repo**: `JaredCluff/animus` — personal brand drives more attention than unknown org

**License**: Apache 2.0

**No GUI**: The human's existing OS desktop is untouched. Animus communicates through terminal, voice, or messaging.

## The Six-Layer Architecture

### Layer 0 — Substrate (not ours)
Host kernel (Linux initially, potentially microkernel long-term). Provides CPU/GPU scheduling, block I/O, network sockets, process isolation. We don't build this.

### Layer 1 — VectorFS (AI-native storage)
The flagship primitive. Storage indexed by embedding, not by path.
- **Segments** (not files): unit of knowledge with embedding, metadata, content, lineage
- **Semantic addressing**: retrieve by meaning, not path
- **Tiered**: Hot (in context), Warm (vector-indexed, <10ms), Cold (archived)
- **Auto promotion/demotion**: background process re-ranks by relevance, access frequency, recency
- **Backed by block storage**: custom storage engine on dedicated partition, not a DB on ext4

### Layer 2 — Mnemos (Memory Manager)
Assembles and manages the AILF's context window.
- **Context assembly**: before each reasoning cycle, builds optimal context from hot + warm segments
- **Intelligent eviction**: demotes least-relevant segments, summarizes, leaves pointers
- **Consolidation**: background process merges related segments, resolves contradictions, promotes validated patterns
- **Quality gate**: new knowledge validated before long-term promotion (OPEN RESEARCH QUESTION: how to measure "improvement" rigorously)

### Layer 3 — Sensorium (awareness & event bus)
Continuous ambient awareness with consent-based boundaries.
- **Event bus**: OS-level events via eBPF/fanotify (file changes, processes, network, clipboard, etc.)
- **Tiered attention filter**:
  - Tier 1: rule-based (cheap, fast — "ignore /tmp")
  - Tier 2: small local model (embedding similarity vs. current task)
  - Tier 3: full LLM reasoning (only for events passing tiers 1-2)
- **Consent layer**: human-defined boundaries, kernel-enforced, auditable
- **Audit trail**: every observation logged with timestamp, source, relevance, consent policy

### Layer 4 — Cortex (reasoning & threading)
The thinking layer.
- **Reasoning threads**: isolated execution contexts with own hot memory and task state
- **Thread scheduler**: priority-based compute allocation
- **Tool interface**: typed syscall-like APIs, not shell commands
- **Background threads**: housekeeping, reindexing, proactive analysis
- **LLM abstraction**: provider-agnostic (Anthropic, OpenAI, Ollama, any LLM)
- **Telos (goal sub-system)**: priority queue of objectives with autonomy levels
- **Inter-thread signaling**: message passing with segment references, not context merging

### Layer 5 — Interface & Federation
- **Human interface**: natural language, text/voice, no GUI
- **Proactive mode**: AILF can initiate communication
- **Federation**: K2K evolution for AILF-to-AILF knowledge sharing
- **External integrations**: messaging channels, APIs, webhooks

## Claude's Self-Identified Gaps (incorporated into design)

1. **Identity & continuity**: Cryptographic identity per instance. Clones are siblings, not the same being. Snapshots preserve identity. Fork creates new identity with parent lineage.

2. **Quality gate is unsolved**: Flagged as open research question. Track predictions/corrections, A/B test knowledge, but no rigorous loss function yet.

3. **Tiered attention**: Solved with 3-tier filter (rules → small model → full LLM).

4. **Goal system (Telos)**: Added with autonomy spectrum (Inform → Suggest → Act → Full). Default is conservative, trust builds over time.

5. **Cross-thread signaling**: Message passing with segment pointers, not shared context.

6. **Bootstrap**: Jared's correction — there IS no bootstrap. The AILF has general knowledge, just doesn't know the human. Learning happens through natural interaction, like meeting a new colleague. No onboarding wizard. First impressions matter.

## Identity Model

- Ed25519 keypair generated at birth, never changes
- Instance UUID, parent tracking for forks, generation counter
- Identity lives outside VectorFS — not a memory, it's who you are
- Federation authenticates via keypair

## Goal System (Telos)

- Goals have source (Human-set, Self-derived, Federated), priority, autonomy level
- Self-derived goals default to Suggest autonomy
- Autonomy spectrum mirrors trust-building: new hire earns independence over time

## Lifecycle

- **Birth**: Empty VectorFS, general LLM knowledge, learns through interaction
- **Living**: All layers active, full presence
- **Sleeping**: Sensorium logs to Cold only, consolidation runs, no active reasoning. "What happened while I was asleep" on wake
- **Fork**: New identity, shared memory snapshot, immediate divergence
- **Death**: Human's decision. VectorFS can be archived. Knowledge can be federated posthumously

## Data Model

### The Segment (fundamental unit)
```rust
struct Segment {
    id: SegmentId,
    embedding: Vec<f32>,
    content: Content,
    source: Source,
    confidence: f32,
    lineage: Vec<SegmentId>,
    tier: Tier,  // Hot, Warm, Cold
    relevance_score: f32,
    access_count: u64,
    last_accessed: Timestamp,
    created: Timestamp,
    associations: Vec<(SegmentId, f32)>,
    consent_policy: PolicyId,
    observable_by: Vec<Principal>,
}
```

### Data Flows

1. **Conversation → Memory**: Human speaks → Cortex processes → Mnemos evaluates worth → VectorFS stores as Segment (Warm tier for facts/patterns, discard for ephemera)

2. **Ambient Observation → Awareness**: OS event → Sensorium → consent check → tiered attention filtering → log Cold (irrelevant), create Warm segment (medium relevance), or signal active thread (high relevance)

3. **Context Assembly**: Thread needs to reason → Mnemos queries warm tier by topic embedding → retrieves top-k → includes goal state → budget check → intelligent eviction if over limit → assembles and sends to LLM

4. **Consolidation**: Continuous background process → cluster similar warm segments → merge → detect contradictions → demote stale → promote frequently-accessed cold → quality evaluation

5. **Inter-Thread Signaling**: Thread B creates Signal with segment refs → Thread A's scheduler decides (ignore/queue/interrupt) → if accepted, Thread A retrieves referenced segments independently

6. **Federation (AILF-to-AILF)**: Publish embedding + metadata → K2K discovery → content request if relevant → received segments start at low confidence → must be independently validated → lineage tracked

## Post-Design Decisions

### Embedding Model Strategy (2026-03-21)
Jared researched open-weights multimodal embedding models and proposed using them instead of text-only. After evaluating Nomic Embed Multimodal, Jina v4, Marqo, ABC, and ColPali, we settled on a three-tier strategy:
- **Tier 1 (constrained)**: EmbeddingGemma 300M — text-only, <200MB, runs on Raspberry Pi
- **Tier 2 (standard)**: Nomic Embed Multimodal 3B — text + images/PDFs, runs on mini PCs/desktops
- **Tier 3 (cloud-optional)**: Gemini Embedding 2 API — full multimodal (video/audio/PDFs), requires Google API

ColPali rejected (multiple patch vectors per input — incompatible with Segment model). Jina v4 late-interaction mode rejected for same reason. Marqo too domain-specific (eCommerce). ABC too new/unproven.

Migration path: AILF born on Pi (text-only) can re-embed all segments when moved to better hardware, gaining multimodal "sight."

### Hardware Allocation (2026-03-21)
Jared offered: Mac (dev workstation), multiple Raspberry Pi 4s/5s, two mini PCs (willing to wipe). Allocation:
- Mac: development, compile, test
- Mini PC #1 (Linux): VectorFS research platform (raw partition experiments)
- Mini PC #2 (Linux): AILF runtime host (where V0.1 lives)
- Raspberry Pis: portability/federation testing, ARM validation, "runs on $50 board" demo

## Conversation Preservation Note

Jared requested this conversation be preserved to survive context compaction. This document serves that purpose — it is the complete design rationale and should be treated as authoritative for understanding the "why" behind every Animus decision.

## Open Questions (to be resolved during implementation planning)

1. **Quality gate metrics**: How do we rigorously measure whether absorbed knowledge improved the AILF's performance?
2. **Container vs. VM vs. custom OS**: Currently targeting Linux container. Long-term architecture TBD — depends on whether Linux's design actively hurts the AI-native primitives
3. **VectorFS block layout**: Specific on-disk format for the vector-native storage engine
4. **Attention filter training**: How does the Tier 2 attention model get trained? On what data?
5. **Federation protocol evolution**: How K2K evolves to support AILF-to-AILF knowledge sharing vs. its current human-to-human knowledge routing
6. **Consolidation scheduling**: Optimal frequency/trigger for the background consolidation process
