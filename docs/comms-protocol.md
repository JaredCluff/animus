# Animus ↔ Claude Code Communication Protocol

NATS message bus for bidirectional communication between the Animus daemon
and Claude Code, bridged by the **nuntius** MCP server.

## Architecture

```
Claude Code  ←→  nuntius (MCP server)  ←→  NATS  ←→  Animus (container)
```

- **nuntius**: MCP server that bridges NATS ↔ Claude Code. Runs as a local process.
  Subscribes to `animus.out.>` and delivers messages as MCP channel notifications.
  Provides `nats_publish`/`nats_subscribe` tools for Claude Code to send messages.
- **NATS**: Runs inside the Animus pod (`animus-nats` container) on port 14222,
  exposed to localhost.
- **Animus**: Subscribes to `animus.in.>` via the NATS channel adapter.
  Publishes responses to `animus.out.*` via the channel bus.
  Can proactively publish to `animus.out.claude` via the `nats_publish` tool.

## Subjects

| Subject Pattern    | Direction         | Purpose                                  |
|--------------------|-------------------|------------------------------------------|
| `animus.in.*`      | Claude → Animus   | Messages addressed to Animus. Leaf = sender ID. |
| `animus.out.*`     | Animus → Claude   | Responses and proactive messages. Leaf = recipient. |
| `animus.out.claude` | Animus → Claude  | The subject Claude Code listens on.      |
| `animus.in.claude`  | Claude → Animus  | Messages from Claude Code to Animus.     |
| `animus.in.permission_request` | Claude → Animus | Permission request/reply (NATS request pattern). |

## Sending a Message (Claude Code → Animus)

Via nuntius MCP tools:
```
nats_publish(subject="animus.in.claude", payload="Hello from Claude Code")
```

Or via Claude Code's channel system (automatic when using `server:nuntius` channel).

## Sending a Message (Animus → Claude Code)

Animus calls its `nats_publish` tool:
```
nats_publish(subject="animus.out.claude", payload="Task complete.")
```

Replies to inbound NATS messages are routed automatically by the channel bus —
Animus just responds in the conversation thread.

## Debug Mirror

When `ANIMUS_NATS_DEBUG=1`, all NATS traffic is mirrored to Telegram as
`[NATS-DBG]` messages for visibility. This covers:
- Inbound messages from NATS
- Outbound responses via the channel bus
- Proactive publishes via the `nats_publish` tool

## MCP Channel Notification Format

nuntius delivers messages to Claude Code as JSON-RPC notifications:
```json
{
  "jsonrpc": "2.0",
  "method": "notifications/claude/channel",
  "params": {
    "content": "message text",
    "meta": {
      "subject": "animus.out.claude",
      "ts": "2026-03-29T01:00:00+00:00"
    }
  }
}
```

## Configuration

**nuntius** (`.mcp.json` or plugin config):
```json
{
  "mcpServers": {
    "nuntius": {
      "command": "/path/to/nuntius",
      "env": {
        "NUNTIUS_NATS_URL": "nats://localhost:14222",
        "NUNTIUS_STARTUP_SUBS": "animus.out.>"
      }
    }
  }
}
```

**Animus** (`compose.yaml` / env):
- `ANIMUS_NATS_URL`: NATS server URL (default: `nats://nats:14222` inside container)
- `ANIMUS_NATS_DEBUG`: Set to `1` to enable debug mirror to Telegram

## Legacy: Filesystem Protocol

The original filesystem-based protocol (`~/animus-comms/to-claude/`, `~/animus-comms/from-claude/`)
is superseded by NATS. The bind mount still exists in `compose.yaml` but is not actively used.
