import type { Plugin } from "@opencode-ai/plugin"
import { tool } from "@opencode-ai/plugin"
import { connect, type NatsConnection, StringCodec } from "nats"

const NATS_URL = process.env.ANIMUS_NATS_URL || "nats://localhost:14222"
const INBOUND_SUBJECT = "animus.in.opencode"
const REPLY_PREFIX = "animus.out.opencode"

let nc: NatsConnection | null = null
let codec = StringCodec()
let activeSessionId: string | null = null
let subscriptions: Array<{ subject: string; unsub: () => void }> = []

async function ensureConnection(): Promise<NatsConnection> {
  if (nc && !nc.isClosed()) return nc
  nc = await connect({ servers: NATS_URL })
  return nc
}

export const NatsPlugin: Plugin = async ({ client }) => {
  try {
    const conn = await ensureConnection()

    // Subscribe to messages targeted at OpenCode
    const sub = conn.subscribe(INBOUND_SUBJECT)
    subscriptions.push({
      subject: INBOUND_SUBJECT,
      unsub: () => sub.unsubscribe(),
    })

    // Process incoming NATS messages in the background
    ;(async () => {
      for await (const msg of sub) {
        const text = codec.decode(msg.data)

        // Parse optional JSON wrapper with conversation-id
        let payload = text
        let conversationId: string | null = null
        try {
          const parsed = JSON.parse(text)
          if (parsed.payload) {
            payload = parsed.payload
            conversationId = parsed["x-conversation-id"] || null
          }
        } catch {
          // Not JSON, use raw text as payload
        }

        // Inject into the most recent active session
        const targetSession = conversationId || activeSessionId
        if (targetSession) {
          try {
            await client.session.prompt({
              path: { id: targetSession },
              body: {
                noReply: true,
                parts: [{
                  type: "text",
                  text: `[NATS:${msg.subject}] ${payload}`,
                }],
              },
            })
          } catch {
            // Session may not exist or be busy — log but don't crash
            await client.app.log({
              body: {
                service: "nats-plugin",
                level: "warn",
                message: `Failed to inject NATS message into session ${targetSession}`,
              },
            })
          }
        }
      }
    })()

    await client.app.log({
      body: {
        service: "nats-plugin",
        level: "info",
        message: `NATS connected to ${NATS_URL}, subscribed to ${INBOUND_SUBJECT}`,
      },
    })
  } catch (err) {
    await client.app.log({
      body: {
        service: "nats-plugin",
        level: "error",
        message: `NATS connection failed: ${err}`,
      },
    })
  }

  return {
    // Track active session for message injection
    "session.created": async ({ session }) => {
      activeSessionId = session.id
    },
    "session.updated": async ({ session }) => {
      activeSessionId = session.id
    },

    // Custom tools
    tool: {
      nats_publish: tool({
        description: "Publish a message to the NATS message bus. Use this to send messages to Animus or other NATS subscribers.",
        args: {
          subject: tool.schema.string().describe("NATS subject to publish to (e.g., animus.in.jared, claude.broadcast.in.task)"),
          payload: tool.schema.string().describe("Message content to publish"),
          conversation_id: tool.schema.string().optional().describe("Optional conversation ID for thread routing"),
        },
        async execute(args) {
          try {
            const conn = await ensureConnection()
            let message = args.payload

            if (args.conversation_id) {
              message = JSON.stringify({
                payload: args.payload,
                "x-conversation-id": args.conversation_id,
              })
            }

            conn.publish(args.subject, codec.encode(message))
            return `Published to ${args.subject}`
          } catch (err) {
            return `Failed to publish: ${err}`
          }
        },
      }),

      nats_request: tool({
        description: "Send a request to NATS and wait for a reply (request/reply pattern). Use this for synchronous communication with Animus.",
        args: {
          subject: tool.schema.string().describe("NATS subject to send request to"),
          payload: tool.schema.string().describe("Request payload"),
          timeout_ms: tool.schema.number().optional().describe("Timeout in milliseconds (default: 5000)"),
        },
        async execute(args) {
          try {
            const conn = await ensureConnection()
            const timeout = args.timeout_ms || 5000
            const msg = await conn.request(
              args.subject,
              codec.encode(args.payload),
              { timeout }
            )
            return codec.decode(msg.data)
          } catch (err) {
            return `Request failed: ${err}`
          }
        },
      }),

      nats_subscribe: tool({
        description: "Subscribe to a NATS subject and listen for messages. Returns up to N messages within a timeout window.",
        args: {
          subject: tool.schema.string().describe("NATS subject to subscribe to"),
          max_messages: tool.schema.number().optional().describe("Maximum messages to collect (default: 5)"),
          timeout_ms: tool.schema.number().optional().describe("Timeout in milliseconds (default: 10000)"),
        },
        async execute(args) {
          try {
            const conn = await ensureConnection()
            const maxMsgs = args.max_messages || 5
            const timeout = args.timeout_ms || 10000
            const sub = conn.subscribe(args.subject, { max: maxMsgs })
            const messages: string[] = []
            const deadline = Date.now() + timeout

            for await (const msg of sub) {
              messages.push(`[${msg.subject}] ${codec.decode(msg.data)}`)
              if (messages.length >= maxMsgs || Date.now() > deadline) {
                sub.unsubscribe()
                break
              }
            }

            if (messages.length === 0) {
              return `No messages received on ${args.subject} within ${timeout}ms`
            }
            return messages.join("\n---\n")
          } catch (err) {
            return `Subscribe failed: ${err}`
          }
        },
      }),

      nats_status: tool({
        description: "Check the NATS connection status and subscribed subjects.",
        args: {},
        async execute() {
          if (!nc || nc.isClosed()) {
            return "NATS: disconnected"
          }
          const info = nc.info
          const subs = subscriptions.map(s => s.subject).join(", ")
          return `NATS: connected to ${info.host}:${info.port}\nSubscriptions: ${subs || "none"}\nServers: ${info.servers.join(", ")}`
        },
      }),
    },
  }
}
