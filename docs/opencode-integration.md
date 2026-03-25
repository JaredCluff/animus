# OpenCode Integration

Animus integrates with [OpenCode](https://opencode.ai) through NATS messaging, enabling bidirectional communication between the AI Life Form and OpenCode's AI coding agent.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                         NATS Bus                            │
│                    (localhost:14222)                         │
│                                                             │
│  animus.in.>    ← Inbound to Animus                        │
│  animus.out.>   ← Outbound from Animus                     │
│  animus.in.opencode  ← OpenCode inbound                    │
│  animus.out.opencode ← OpenCode outbound                   │
└──────────────┬──────────────────────────┬───────────────────┘
               │                          │
        ┌──────┴──────┐            ┌──────┴──────┐
        │   Animus    │            │  OpenCode   │
        │  (Podman)   │            │  (Local)    │
        │             │            │             │
        │ nats_publish│            │ nats_publish│
        │ nats_subscribe            │ nats_request│
        └─────────────┘            └─────────────┘
```

## Components

### 1. Animus NATS Channel Adapter

**Location:** `crates/animus-channel/src/nats/mod.rs`

Animus runs a NATS sidecar container and exposes a channel adapter that:
- Subscribes to `animus.in.>` (wildcard)
- Routes messages to reasoning threads
- Computes reply subjects: `animus.in.X` → `animus.out.X`
- Supports request/reply patterns

**Configuration:** `compose.yaml` exposes NATS on `127.0.0.1:14222`

### 2. OpenCode NATS Plugin

**Location:** `.opencode/plugins/nats.ts`

A TypeScript plugin that connects OpenCode to the NATS bus. Provides:

| Tool | Description |
|------|-------------|
| `nats_publish` | Publish messages to any NATS subject |
| `nats_request` | Request/reply pattern for synchronous communication |
| `nats_subscribe` | Subscribe and collect messages within a timeout |
| `nats_status` | Check connection and subscription status |

**Auto-injected behavior:**
- Subscribes to `animus.in.opencode` on startup
- Injects received messages into the active OpenCode session as `[NATS:subject] payload`
- Tracks active session for message routing

### 3. Dependencies

**Location:** `.opencode/package.json`

```json
{
  "dependencies": {
    "nats": "^2.29.3"
  }
}
```

OpenCode automatically runs `bun install` at startup to install dependencies.

## Usage

### Starting Animus with NATS

```bash
# Start NATS and Animus containers
podman compose up -d

# Verify NATS is running
curl -s http://localhost:8222/healthz
```

### Starting OpenCode with NATS Plugin

```bash
# OpenCode automatically loads plugins from .opencode/plugins/
opencode

# The plugin will connect to NATS and subscribe to animus.in.opencode
```

### Sending Messages Between Systems

**From Animus to OpenCode:**
```
# Animus LLM uses nats_publish tool
nats_publish("animus.in.opencode", "Hello from Animus!")
```

**From OpenCode to Animus:**
```
# OpenCode LLM uses nats_publish tool
nats_publish("animus.in.jared", "Hello from OpenCode!")
```

**Request/Reply (synchronous):**
```
# OpenCode sends request and waits for response
nats_request("animus.in.jared", "What's the current status?", timeout_ms=5000)
```

## Message Format

### Simple Text
```
Hello from Animus!
```

### JSON with Conversation ID (thread routing)
```json
{
  "payload": "Hello from Animus!",
  "x-conversation-id": "jared"
}
```

## Configuration

### Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `ANIMUS_NATS_URL` | `nats://localhost:14222` | NATS server URL |
| `ANIMUS_NATS_DISABLED` | (unset) | Set to disable NATS |

### Animus Config (`config.toml`)

```toml
[channels.nats]
enabled = true
url = "nats://animus-nats:14222"
subjects = ["animus.in.>"]
reply_prefix = "animus.out"
```

## Troubleshooting

### Plugin not loading
- Check that `bun` is installed (OpenCode uses it for plugins)
- Verify `.opencode/plugins/nats.ts` exists
- Check OpenCode logs for plugin errors

### NATS connection failed
- Verify NATS is running: `podman ps --filter name=animus-nats`
- Check port 14222 is accessible: `nc -zv localhost 14222`
- Verify `ANIMUS_NATS_URL` environment variable if using custom URL

### Messages not received
- Check NATS subject matches (case-sensitive)
- Verify subscription is active with `nats_status` tool
- Check Animus logs for routing information
