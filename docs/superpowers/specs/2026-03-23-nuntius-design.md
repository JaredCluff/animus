# Nuntius Design Spec

**Project:** `JaredCluff/nuntius`
**License:** Apache 2.0
**Language:** Rust
**Date:** 2026-03-23

---

## Overview

Nuntius is a standalone Rust MCP server that bridges AI agents (Claude Code and others) to a NATS messaging cluster. It enables the full range of agent communication patterns — pub/sub broadcasts, request/reply task delegation, durable work queues, shared KV state, and agent discovery — through a focused set of 16 MCP tools.

The key differentiator from existing NATS MCP servers (jesseobrien/nats-mcp, sinadarbouy/mcp-nats): **inbound delivery**. NATS messages pushed to subscribed subjects appear inside Claude Code's conversation context as `<channel>` notifications, making Claude Code a reactive participant rather than a passive caller.

### Use Case

A multi-agent IT organization where Animus orchestrates coding agents (Claude Code), QA agents, and security/red-team agents across multiple software products. Nuntius is the messaging backbone — each agent connects via its own nuntius instance, all sharing the same NATS cluster.

---

## Architecture

```
Claude Code  ←── stdio / JSON-RPC ──→  nuntius  ←── TCP ──→  NATS Server
                  (MCP protocol)                             (nats://...)
```

Nuntius is a single process with two responsibilities:

1. **MCP server (inbound from Claude):** Serves JSON-RPC 2.0 over stdio. Receives tool calls, executes them against NATS, returns results.
2. **NATS subscription listener (inbound from NATS):** Maintains active subscriptions. When a message arrives, pushes it to Claude Code as an MCP `notifications/message` with channel content — the same mechanism used by the Telegram MCP plugin.

Both run concurrently via tokio. The subscription listener does not poll; it is purely event-driven.

### Repository Structure

```
nuntius/
├── Cargo.toml                  # workspace
├── LICENSE
├── README.md
├── crates/
│   ├── nuntius-core/           # lib: NATS client wrapper, tool implementations, types
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── client.rs       # NatsBridge: wraps async-nats, holds subscriptions
│   │       ├── tools/
│   │       │   ├── mod.rs
│   │       │   ├── core.rs     # nats_publish, nats_request, nats_subscribe, nats_unsubscribe
│   │       │   ├── jetstream.rs # js_publish, js_stream_*, js_consume
│   │       │   ├── kv.rs       # kv_get, kv_put, kv_delete, kv_keys
│   │       │   └── agent.rs    # agent_announce, agent_discover, agent_claim
│   │       └── types.rs        # ChannelNotification, ToolResult, Config
│   └── nuntius/                # bin: MCP stdio server, wires everything together
│       ├── Cargo.toml
│       └── src/
│           └── main.rs
```

---

## Configuration

All configuration via environment variables; no config file required to run.

| Variable | Default | Description |
|---|---|---|
| `NUNTIUS_NATS_URL` | `nats://localhost:4222` | NATS server URL |
| `NUNTIUS_AUTH_TOKEN` | — | Token auth |
| `NUNTIUS_USER` | — | User/pass auth (pair with `NUNTIUS_PASS`) |
| `NUNTIUS_PASS` | — | Password |
| `NUNTIUS_NKEY` | — | NKey seed for NKey auth |
| `NUNTIUS_TLS_CERT` | — | Path to client TLS cert |
| `NUNTIUS_TLS_KEY` | — | Path to client TLS key |
| `NUNTIUS_STARTUP_SUBS` | — | Comma-separated subjects to subscribe on startup |
| `NUNTIUS_REQUEST_TIMEOUT_MS` | `5000` | Default timeout for `nats_request` |

---

## Tool Catalog

### Core Messaging

#### `nats_publish`
Fire-and-forget publish to a subject.

**Parameters:**
- `subject` (string, required) — NATS subject
- `payload` (string, required) — message body (UTF-8)
- `headers` (object, optional) — key-value string headers
- `reply_to` (string, optional) — reply-to subject

**Returns:** `{ "ok": true }`

---

#### `nats_request`
Publish and wait for a single reply (request/reply pattern).

**Parameters:**
- `subject` (string, required)
- `payload` (string, required)
- `timeout_ms` (integer, optional, default: `NUNTIUS_REQUEST_TIMEOUT_MS`) — max wait

**Returns:** `{ "subject": "...", "payload": "...", "headers": {...} }`

**Error:** timeout or no responders.

---

#### `nats_subscribe`
Add a live subscription. Future messages on this subject will be pushed into Claude Code's conversation as `<channel>` notifications.

**Parameters:**
- `subject` (string, required) — supports wildcards (`foo.*`, `foo.>`)
- `queue_group` (string, optional) — join a queue group for work-queue distribution

**Returns:** `{ "subscription_id": "sub-uuid", "subject": "..." }`

**Notes:**
- Subscription persists until `nats_unsubscribe` or process exit.
- If `queue_group` is set, NATS delivers each message to exactly one member of the group — enabling work-queue semantics without JetStream.

---

#### `nats_unsubscribe`
Remove a subscription.

**Parameters:**
- `subscription_id` (string, required)

**Returns:** `{ "ok": true }`

---

### JetStream — Durable Messaging

#### `js_publish`
Publish with persistence guarantee. Requires a stream to be configured for the subject.

**Parameters:**
- `subject` (string, required)
- `payload` (string, required)
- `msg_id` (string, optional) — deduplication ID

**Returns:** `{ "stream": "...", "seq": 42, "duplicate": false }`

---

#### `js_stream_create`
Create a JetStream stream.

**Parameters:**
- `name` (string, required)
- `subjects` (array of string, required) — subjects this stream captures
- `max_msgs` (integer, optional) — retention limit by count
- `max_bytes` (integer, optional) — retention limit by size
- `max_age_secs` (integer, optional) — retention limit by age

**Returns:** stream info object.

---

#### `js_stream_info`
Get info and stats for a stream.

**Parameters:**
- `name` (string, required)

**Returns:** `{ "name": "...", "subjects": [...], "messages": 42, "bytes": 1024 }`

---

#### `js_stream_delete`
Delete a stream and all its messages.

**Parameters:**
- `name` (string, required)

**Returns:** `{ "ok": true }`

---

#### `js_consume`
Pull up to N messages from a JetStream consumer. Creates an ephemeral consumer if `consumer_name` is not given.

**Parameters:**
- `stream` (string, required)
- `consumer_name` (string, optional) — durable consumer name; creates ephemeral if omitted
- `batch` (integer, optional, default: 1) — max messages to fetch
- `timeout_ms` (integer, optional, default: 5000)

**Returns:** array of `{ "subject": "...", "payload": "...", "seq": N, "headers": {...} }`

---

### KV Store

Backed by JetStream KV. Useful for agents sharing state (e.g., task ownership, config).

#### `kv_put`
**Parameters:** `bucket` (string), `key` (string), `value` (string)
**Returns:** `{ "revision": 7 }`

#### `kv_get`
**Parameters:** `bucket` (string), `key` (string)
**Returns:** `{ "key": "...", "value": "...", "revision": 7 }`
**Error:** key not found.

#### `kv_delete`
**Parameters:** `bucket` (string), `key` (string)
**Returns:** `{ "ok": true }`

#### `kv_keys`
**Parameters:** `bucket` (string), `prefix` (string, optional)
**Returns:** `{ "keys": ["a", "b", "c"] }`

---

### Agent Coordination

Built on top of NATS primitives. Uses `agents.registry` as the well-known coordination subject.

#### `agent_announce`
Broadcast this agent's presence and capabilities to `agents.registry`.

**Parameters:**
- `agent_id` (string, required) — stable identifier for this agent instance
- `capabilities` (array of string, required) — e.g. `["coding", "rust", "api-design"]`
- `metadata` (object, optional) — arbitrary key-value

**Returns:** `{ "ok": true }`

**Notes:** Publishes JSON to `agents.registry.announce` (for subscribers monitoring announcements) AND writes `{ capabilities, metadata, last_seen }` to the `agents-registry` JetStream KV bucket under key `<agent_id>`. The KV entry is the authoritative registry record used by `agent_discover`. `agent_announce` creates the `agents-registry` bucket if it does not exist. Agents should call this on startup and periodically as a heartbeat; KV entries do not expire automatically. (Note: JetStream KV bucket names may not contain dots, so `agents-registry` is used rather than the conceptual name `agents.kv`.)

---

#### `agent_discover`
Query the registry for agents matching a capability filter.

**Parameters:**
- `capability` (string, optional) — filter by capability; omit to list all

**Returns:** array of `{ "agent_id": "...", "capabilities": [...], "metadata": {...}, "last_seen": "..." }`

**Implementation:** Reads from the `agents-registry` JetStream KV bucket written by `agent_announce`. Filters by capability if provided. Stale detection is the caller's responsibility — `last_seen` is provided for that purpose; nuntius does not filter by age.

---

#### `agent_claim`
Attempt to atomically claim a task from a work-queue subject. Uses NATS queue groups — only one subscriber receives each message.

**Parameters:**
- `subject` (string, required) — the work queue subject
- `timeout_ms` (integer, optional, default: 1000)

**Returns:** `{ "claimed": true, "task": {...} }` or `{ "claimed": false }` on timeout.

**Notes:** Internally this is a queue-group subscribe + single fetch. Nuntius creates the subscription automatically and tears it down after the fetch. The queue group name is always `nuntius.claim` — this ensures that multiple nuntius instances subscribing to the same subject share work-queue semantics (each message delivered to exactly one claimer).

---

## Inbound Delivery

When Claude Code calls `nats_subscribe("tasks.coding.>")`, nuntius registers a NATS subscription internally. Tokio spawns a task per subscription. When a message arrives:

1. Decode payload as UTF-8 (base64 fallback for binary)
2. Format as MCP notification:

```json
{
  "jsonrpc": "2.0",
  "method": "notifications/message",
  "params": {
    "level": "info",
    "data": "<channel source=\"nats\" subject=\"tasks.coding.kn-001\" ts=\"2026-03-23T21:05:00Z\" reply_to=\"_INBOX.abc\">{\n  \"task_id\": \"kn-001\",\n  \"type\": \"implement\"\n}</channel>"
  }
}
```

3. Write to stdout (the MCP stdio channel)

Claude Code renders the `<channel>` block in its conversation context and reacts accordingly.

**Subscription state:** held in `Arc<Mutex<HashMap<String, SubscriptionHandle>>>` in `NatsBridge`. Subscriptions survive tool calls. All are dropped on process exit.

**Startup subscriptions:** If `NUNTIUS_STARTUP_SUBS` is set, nuntius subscribes before accepting tool calls.

**Reply-to:** If the incoming message has a reply-to subject, it is included in the channel tag. Claude Code can then call `nats_publish` with that reply-to to respond directly.

---

## Error Handling

- Connection failure on startup: print error to stderr, exit non-zero.
- NATS reconnect: `async-nats` handles reconnect transparently; tool calls during reconnect return an error describing the state.
- Tool parameter validation: return MCP error response (not panic).
- Subscription task crash: log to stderr, mark subscription as dead, return error on next `nats_unsubscribe`.
- All errors are strings in the MCP error response — no structured error types exposed to the LLM.

---

## Testing Strategy

**Unit tests (nuntius-core):** Each tool implementation tested against a real NATS server spun up via `testcontainers-rs` (NATS official image). Tests cover:
- publish/subscribe round-trip
- request/reply with timeout
- JetStream stream create/publish/consume lifecycle
- KV put/get/delete/keys
- agent_announce → agent_discover flow
- Inbound delivery: subscribe, receive mock message, verify notification format

**Integration tests:** MCP stdio protocol tested end-to-end — spawn nuntius binary, send JSON-RPC tool calls over stdin, assert responses on stdout.

**No mocking of NATS.** Real server for all tests; testcontainers handles lifecycle.

---

## Dependencies

```toml
async-nats = "0.38"          # official NATS async Rust client with JetStream + KV
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4"] }
tracing = "0.1"
tracing-subscriber = "0.3"

# dev-only
testcontainers = "0.23"
testcontainers-modules = { version = "0.11", features = ["nats"] }
```

---

## What This Is Not

- Not a NATS server or proxy
- Not a persistent message store (JetStream handles that)
- Not responsible for agent logic or orchestration
- Not a replacement for K2K (which handles semantic knowledge sharing between AILFs)

Nuntius is transport. It moves bytes between agents. What agents do with those bytes is their business.
