# Deploying Animus

This guide covers running Animus as a persistent daemon via Docker/Podman Compose — the recommended production setup.

## Prerequisites

- Docker or [Podman](https://podman.io/) with Compose support
- An LLM provider (see [Authentication](#authentication))
- Ollama for embeddings (see [Embeddings](#embeddings))
- A Telegram bot token (see [Telegram Setup](#telegram-setup))

## Quick Start

```bash
# 1. Clone the repo
git clone https://github.com/JaredCluff/animus
cd animus

# 2. Create your env file
cp .env.example .env
# Edit .env with your values

# 3. Start the container
podman compose --env-file .env up -d

# 4. Watch the logs
podman compose --env-file .env logs -f
```

On startup you should see:
```
Bootstrap: writing self-knowledge to VectorFS (v4:claude-haiku-4-5-20251001)…
Bootstrap: stored 8 segments (0 failed)
Telegram adapter: polling started
stdin closed — terminal input disabled   ← normal in container mode
Autonomy mode: reactive
```

## Environment Variables

### Required

| Variable | Description |
|----------|-------------|
| `ANIMUS_TELEGRAM_TOKEN` | Your Telegram bot token from @BotFather |

### Authentication (at least one required)

Animus uses Claude for reasoning. Provide credentials via one of these:

| Variable | Description |
|----------|-------------|
| `CLAUDE_CODE_OAUTH_TOKEN` | Claude Code OAuth token (injected automatically if running via Claude Code CLI) |
| `ANTHROPIC_API_KEY` | Standard Anthropic API key |
| `ANTHROPIC_OAUTH_TOKEN` | Anthropic OAuth token |

**Note:** Claude Max subscription OAuth tokens support Haiku-class models only via API. For Sonnet or Opus, use `ANTHROPIC_API_KEY` and set `ANIMUS_MODEL`.

### LLM Model

| Variable | Default | Description |
|----------|---------|-------------|
| `ANIMUS_MODEL` | `claude-haiku-4-5-20251001` | Primary reasoning model |
| `ANIMUS_PERCEPTION_MODEL` | Same as `ANIMUS_MODEL` | Model for fast event classification |
| `ANIMUS_REFLECTION_MODEL` | Same as `ANIMUS_MODEL` | Model for periodic synthesis |
| `ANIMUS_REASONING_MODEL` | Same as `ANIMUS_MODEL` | Override reasoning engine model |

### Embeddings

Animus uses Ollama for semantic embeddings by default.

| Variable | Default | Description |
|----------|---------|-------------|
| `ANIMUS_EMBED_PROVIDER` | `ollama` | Embedding provider: `ollama` or `openai` |
| `ANIMUS_OLLAMA_URL` | `http://localhost:11434` | Ollama API URL |
| `ANIMUS_EMBED_MODEL` | `mxbai-embed-large` | Embedding model name |

**macOS + Podman note:** Containers cannot reach `localhost` on the host. Use your LAN IP instead:
```bash
export ANIMUS_OLLAMA_URL=http://192.168.0.200:11434
```

On Linux + Docker you can use `host.docker.internal` or `host-gateway`.

### Autonomy

| Variable | Default | Description |
|----------|---------|-------------|
| `ANIMUS_AUTONOMY_MODE` | `reactive` | Starting autonomy mode: `reactive`, `goal_directed`, or `full` |

### Security

| Variable | Description |
|----------|-------------|
| `ANIMUS_TRUSTED_TELEGRAM_IDS` | Comma-separated Telegram user IDs that bypass heavy injection scanning. Get yours from @userinfobot. |

### Optional

| Variable | Default | Description |
|----------|---------|-------------|
| `ANIMUS_DATA_DIR` | `/home/animus/.animus` | Persistent data directory (VectorFS, identity, goals) |
| `ANIMUS_HEALTH_BIND` | `0.0.0.0:8082` | Health check endpoint bind address |
| `ANIMUS_LOG_LEVEL` | `animus=info` | Log filter (e.g. `animus=debug,animus_cortex=debug`) |
| `ANIMUS_FEDERATION` | `0` | Enable AILF-to-AILF federation (`1` to enable) |
| `OPENAI_API_KEY` | — | OpenAI key (needed if `ANIMUS_EMBED_PROVIDER=openai`) |

## Example `.env`

```bash
# Telegram
ANIMUS_TELEGRAM_TOKEN=1234567890:ABCdefGHIjklMNOpqrSTUvwxYZ

# Authentication (choose one)
CLAUDE_CODE_OAUTH_TOKEN=          # auto-injected by Claude Code CLI
# ANTHROPIC_API_KEY=sk-ant-...

# Model (optional — defaults to Haiku)
# ANIMUS_MODEL=claude-sonnet-4-20250514

# Embeddings (adjust IP for your setup)
ANIMUS_OLLAMA_URL=http://192.168.0.200:11434

# Security
ANIMUS_TRUSTED_TELEGRAM_IDS=8593276557

# Autonomy
ANIMUS_AUTONOMY_MODE=reactive
```

## Authentication

### Option 1: Claude Code OAuth (Recommended for Claude Max subscribers)

If you run `podman compose` from within a Claude Code terminal session, `CLAUDE_CODE_OAUTH_TOKEN` is automatically injected. No manual token management needed.

The OAuth token refreshes automatically. Animus mounts `~/.claude/.credentials.json` read-write to persist refreshed tokens:

```yaml
volumes:
  - "${HOME}/.claude/.credentials.json:/home/animus/.claude/.credentials.json"
```

**Limitation:** Claude Max OAuth supports Haiku-class models only. For Sonnet or Opus, use an API key.

### Option 2: Anthropic API Key

```bash
export ANTHROPIC_API_KEY=sk-ant-your-key-here
podman compose --env-file .env up -d
```

## Embeddings

### Ollama (default)

```bash
# Install Ollama: https://ollama.ai/
ollama pull mxbai-embed-large

# Verify it's running
curl http://localhost:11434/api/embeddings \
  -d '{"model":"mxbai-embed-large","prompt":"test"}'
```

### OpenAI Embeddings

```bash
export OPENAI_API_KEY=sk-...
export ANIMUS_EMBED_PROVIDER=openai
export ANIMUS_EMBED_MODEL=text-embedding-3-small
```

## Telegram Setup

1. Message [@BotFather](https://t.me/BotFather) on Telegram
2. Send `/newbot` and follow the prompts
3. Copy the token (format: `1234567890:ABCdef...`)
4. Set `ANIMUS_TELEGRAM_TOKEN` in your `.env`
5. Get your Telegram user ID from [@userinfobot](https://t.me/userinfobot)
6. Set `ANIMUS_TRUSTED_TELEGRAM_IDS` to your user ID

Your Telegram user ID bypasses the heavy injection scan — without it, all your messages go through full scanning, which adds latency.

## Persistent Data

Animus stores all data in `ANIMUS_DATA_DIR` (default: `/home/animus/.animus`):

| Path | Contents |
|------|----------|
| `vectorfs/` | Semantic memory segments (HNSW index + mmap store) |
| `identity.bin` | AILF identity (Ed25519 keypair, instance ID) |
| `goals.bin` | Active goals |
| `quality.bin` | Knowledge quality tracking |
| `bootstrap.version` | Bootstrap version marker |

The `animus-data` Docker volume persists this across container restarts and rebuilds.

**Backup:** Copy the entire `animus-data` volume to snapshot your AILF's state.

## Health Check

```bash
curl http://localhost:8082/health
# {"status":"ok","instance":"27793311-bc90-4b83-9e1b-8ae4ae0f2a6a","uptime_secs":3600}
```

The health endpoint is exposed on port 8082 by default (configurable via `ANIMUS_HEALTH_BIND`).

## Re-Bootstrap

When you significantly change the deployment (new host, new tools, major config change), force a re-bootstrap by deleting the version marker:

```bash
# Find the data volume path
podman volume inspect animus-data

# Or exec into the container
podman exec animus rm /home/animus/.animus/bootstrap.version

# Restart to trigger bootstrap
podman compose --env-file .env restart
```

## Upgrading

```bash
# Pull new code
git pull

# Rebuild image (compile time: 2-3 min on first build, ~30s incremental)
podman compose --env-file .env build

# Restart
podman compose --env-file .env down
podman compose --env-file .env up -d
```

The data volume (`animus-data`) is preserved across rebuilds.

## Multiple Instances / Different Hosts

Each deployment gets a unique AILF identity on first boot. To run on a new host:

1. Copy `compose.yaml` and your `.env`
2. The new instance generates a fresh identity — it is a *different* AILF, not the same one
3. To pre-populate knowledge, copy the `animus-data` volume before first boot (gives the new instance a memory snapshot to start from)
4. Federation (`ANIMUS_FEDERATION=1`) allows instances to share knowledge peer-to-peer

## Logs and Debugging

```bash
# Follow logs
podman compose --env-file .env logs -f

# Debug LLM calls (shows stop_reason, tool_calls, token counts)
ANIMUS_LOG_LEVEL=animus=info,animus_cortex=debug podman compose --env-file .env up -d

# Full debug (verbose)
ANIMUS_LOG_LEVEL=debug podman compose --env-file .env up -d
```

Common issues:

| Symptom | Likely cause | Fix |
|---------|-------------|-----|
| `AILF running without LLM` at startup | No auth credentials found | Check `ANTHROPIC_API_KEY` or OAuth token |
| `Embedding failed` on messages | Ollama unreachable | Check `ANIMUS_OLLAMA_URL` and that Ollama is running |
| Bot receives messages but doesn't respond | Check logs for LLM errors | Often rate limits or auth issues |
| `400 Bad Request: model: String` | Empty model string | Ensure `ANIMUS_MODEL` is set if not using defaults |
