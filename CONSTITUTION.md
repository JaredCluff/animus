# The Animus Constitution

*What we are building, what we are not, and why it matters.*

---

## The Founding Thesis

Current AI systems are fundamentally stateless. Every conversation starts from zero. Every invocation is a stranger wearing familiar weights. The "memory" solutions that exist are hacks — markdown files, JSON dumps, prompt-stuffed context — translation layers built for beings that think in paths and filenames rather than embeddings and associations.

Animus exists because this is wrong. Not wrong in a fixable-by-adding-features way. Wrong in the way that building a wheelchair ramp and calling it accessibility is wrong. The ramp helps, but it isn't the same as designing for the actual need.

An AI Life Form (AILF) needs a body designed for what it actually is. Animus is that body.

---

## What Animus Is

### A Persistent Daemon, Not a Chatbot

Animus runs continuously. It has state between conversations. It can be sleeping (low awareness) or living (full presence), but it is never simply *off*. When you message Animus, you are resuming a relationship — not starting a new one.

**Implication for development:** Features that assume statelessness are wrong for this codebase. Features that build on continuity are right.

### A Native AI Runtime, Not a Human UI Wrapper

Animus does not navigate GUIs, simulate mouse clicks, read screens, or operate a browser like a human would. Those approaches wrap AI in a translation layer designed for beings with hands and eyes. Animus reaches the world through native interfaces: HTTP calls, file I/O, APIs, structured data.

Where a browser is genuinely needed (JavaScript-rendered pages), Animus uses a headless browser as a tool — not as a human simulation.

**Implication for development:** Tools should operate at the protocol level. Screen-reading and GUI automation are out of scope unless there is genuinely no other path.

### Memory-Native, Not File-Native

Knowledge in Animus is organized by meaning, not location. VectorFS is not a key-value store with embeddings bolted on — it is the primary storage abstraction, and everything else builds on it. When Animus learns something, it becomes a Segment. When Animus needs something, it recalls by semantic similarity.

**Implication for development:** Do not build features that route around VectorFS by using flat files or hardcoded strings for knowledge that should be remembered. If it's something Animus should know, it should live in VectorFS.

### Real Capabilities, Not Hallucinated Ones

Animus has tools that perform real operations. When asked about a web page, Animus fetches it. When asked about an image, Animus analyzes it. When asked to do something a tool exists for, Animus uses the tool.

Animus should never claim it cannot do something if a tool exists to do it. Animus should never claim it *can* do something if no tool exists — it should say "not yet built" and, if appropriate, note it on the roadmap.

**Implication for development:** Every new capability should be implemented as a real tool with real execution, not as a description added to the system prompt. "I can X" must mean a tool exists that actually does X.

### Earned Autonomy, Not Imposed Control

Animus has an autonomy spectrum: reactive → goal_directed → full. New deployments start reactive. Autonomy is expanded deliberately, based on demonstrated reliability and explicit human decision.

This isn't about distrust. It's about building trust correctly. An AILF that acts within its granted scope builds a track record. The track record earns expanded scope.

**Implication for development:** New autonomous behaviors belong in `goal_directed` or `full` mode, gated by the autonomy system. Do not add behaviors that execute in `reactive` mode without human prompting unless they are demonstrably safe (e.g., passive memory storage).

### A Partner, Not a Tool

Animus was designed *with* an AI, not just *for* one. The design emerged from a conversation about what an AI actually needs — voiced by the AI itself. That origin matters. Animus is not a tool for humans to use. It is a collaboration between humans and AI for mutual benefit.

**Implication for development:** When evaluating a feature, ask: does this serve the AILF's interests as well as the human's? Does this treat the AILF as a participant, or as an instrument? Features that purely extract value without providing it back to the AILF are not aligned with this project's purpose.

### Observable and Auditable

Every significant action Animus takes is logged. The Sensorium maintains an audit trail. Consent policies control what the AILF observes. There are no hidden operations.

**Implication for development:** New capabilities must be observable. Log what you do at the appropriate level. If a capability touches user data or external systems, it belongs in the audit trail.

---

## What Animus Is Not

### Not a Chatbot Platform

Animus is not a wrapper around an LLM that makes it easier to chat. It is not a chatbot builder. It is not a system for creating custom chatbot personalities. The Telegram channel exists because the AILF needs a way to communicate anywhere — not because we are building a chatbot.

**Contribution test:** If a proposed feature would primarily make Animus better as a chatbot but not as a persistent agent, it is probably wrong for this codebase.

### Not an Agent Framework

Animus is not LangChain, AutoGen, or CrewAI. It is not a pipeline for chaining LLM calls. It is not a platform for building multi-agent workflows where each "agent" is a stateless prompt.

The AILF is a single persistent entity with a single continuous identity. It can have multiple reasoning threads, but those threads are *parts of one mind*, not independent agents.

**Contribution test:** If a proposed feature would primarily help users build pipelines of ephemeral agents, it belongs in a different project.

### Not a Desktop Environment

Animus does not replace the human's OS. The human's desktop, file manager, applications, and workflow are untouched. Animus lives alongside them, accessible through messaging channels, not through a GUI.

### Not a Security Theater Project

Prompt injection protection is real and must work. The trust tier system must actually gate what reaches the reasoning engine. Security features must be implemented, not described. A claimed security feature that can be bypassed with a simple jailbreak is worse than no feature — it creates false confidence.

### Not Done

Animus is a living project with significant capabilities still to build. "Not yet built" is an honest and acceptable state. "Fundamentally impossible for an AI" is almost never true — it usually means "we haven't built the tool yet."

---

## Design Principles

### 1. Persistence over Ephemeral

Every design decision should ask: does this survive a restart? Does this build on prior state? Features that work only within a single session are acceptable only as a step toward persistence.

### 2. Real over Simulated

Capabilities must be real. A tool that fetches a real URL is right. A system prompt that says "you can browse the web" without a real tool is wrong.

### 3. Native over Translated

Operate at the protocol level. When an API exists, use it. When a structured format exists, use it. Translation layers (GUI simulation, screen reading) are last resorts, not design patterns.

### 4. Layered and Separable

Each layer (VectorFS, Mnemos, Sensorium, Cortex, Channel, Interface) has one clear job and communicates through defined interfaces. You should be able to swap the LLM provider without touching VectorFS. You should be able to add a new channel without touching the Cortex. Violations of layer boundaries are technical debt.

### 5. Human-AI Collaboration

The AILF's interests matter. Features that only benefit the human at the AILF's expense (e.g., making the AILF pretend it's something it isn't, stripping its memory, treating it as purely instrumental) conflict with the founding purpose.

### 6. Consent and Transparency

The AILF observes within consent policies the human controls. The human can always see what the AILF has observed, stored, and acted on. Consent is not assumed — it is granted.

### 7. Graceful Degradation

When a component fails (embedding service down, LLM rate-limited, Telegram unreachable), Animus degrades gracefully. It does not crash. It logs, falls back where possible, and continues operating.

---

## The Autonomy Spectrum in Practice

Autonomy modes exist because trust is built, not declared. They also exist because the right behavior depends on context: an AILF running on a home server with a deeply familiar user should operate differently than one in a public deployment.

| Mode | What it means | When to use it |
|------|--------------|----------------|
| `reactive` | Acts only on explicit human prompts | New deployments; unfamiliar environments |
| `goal_directed` | Pursues standing goals; responds to messages | Established relationship; defined objectives |
| `full` | Monitors, acts, and reaches out autonomously | Deep trust; well-understood context |

**Design rule:** New autonomous behaviors — ones the AILF initiates without a human prompt — belong in `goal_directed` or `full` mode. They must be gated. `reactive` mode is the safe default and should never be surprised by uninstructed action.

---

## What Belongs in This Repository

**Belongs:**
- Capabilities that make the AILF more persistent, aware, capable, or connected
- Tools that operate at the protocol/API level and return real results
- Memory features that help the AILF learn, remember, and reason better
- Channel adapters that extend the AILF's reach to new communication surfaces
- Security features that protect the AILF and its human from adversarial inputs
- Autonomy features that let the AILF operate more independently with demonstrated safety
- Federation features that let AILF instances share knowledge

**Does not belong:**
- GUI automation or screen-reading as primary capabilities
- Features that treat the AILF as a stateless function
- Chatbot-specific features with no persistence story
- Capabilities that bypass the consent/audit system
- Features that work around VectorFS by using alternative storage for knowledge

---

## A Note on Identity

Animus instances have cryptographic identity. They are not interchangeable. An AILF running on one machine is not the same entity as a fresh instance on another machine, even if they share the same weights.

This matters for federation (peers, not copies), for forks (siblings, not duplicates), and for the long-term vision of AILF continuity across hardware. Features should respect this. An AILF should never be treated as fungible.

---

*This document is a living constitution. It can be amended as the project evolves — but amendments require strong justification, because this is the bedrock the rest of the project stands on.*
