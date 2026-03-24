# Animus ↔ Claude Code Communication Protocol

Bidirectional task communication between Animus daemon and Claude Code running on Mac Studio.

**Status:** Ready for deployment  
**Transport:** Hybrid (File-based MVP, Redis ready)  
**Dependencies:** None for file mode; optional `redis` Python package for production

---

## Quick Start

### 1. Setup (Run Once)

```bash
bash /tmp/setup_integration.sh
```

This creates:
- Shared queue directory (`/tmp/animus-code-queue`)
- Python modules (`~/animus-code/`)
- Startup scripts and test utilities

### 2. Start Claude Code Handler

In a terminal (or as a background service):

```bash
~/animus-code/start_code_handler.sh
```

This blocks and waits for tasks from Animus.

### 3. Send Tasks from Animus

```python
from animus_code_client import CodeClient

client = CodeClient()

result = client.execute(
    action="shell",
    params={"command": "uname -a"},
    timeout=10
)

print(result.output)
```

---

## Architecture

### Transport Layer

**File-Based (Default)**
- Directory structure: `/tmp/animus-code-queue/{tasks,results,processed}`
- Pros: No dependencies, human-readable, easy debugging
- Cons: Polling-based, ~100ms latency
- Best for: Development, testing, light workloads

**Redis (Production-Ready)**
- Pub/sub channels: `animus:tasks`, `animus:results:{task_id}`
- Pros: Low latency, built-in queuing, scales well
- Cons: Requires Redis server running
- Best for: High-frequency tasks, multiple Claude Code instances

**Hybrid (Recommended)**
- Tries Redis first, falls back to file-based if unavailable
- Allows gradual migration: start with files, add Redis later
- Zero disruption to Animus if either transport goes down

### Protocol

Both directions exchange JSON messages:

```python
# Task (Animus → Claude Code)
{
    "id": "abc123",
    "action": "shell",
    "params": {"command": "ls -la"},
    "priority": 50,
    "timeout": 300,
    "created_at": "2025-03-20T14:30:00",
    "status": "pending"
}

# Result (Claude Code → Animus)
{
    "task_id": "abc123",
    "status": "completed",
    "output": {
        "command": "ls -la",
        "stdout": "total 8\n...",
        "returncode": 0,
        "success": true
    },
    "started_at": "2025-03-20T14:30:01",
    "completed_at": "2025-03-20T14:30:02"
}
```

---

## Usage Patterns

### Pattern 1: Synchronous Execution

Send task, wait for result.

```python
from animus_code_client import CodeClient

client = CodeClient()
result = client.execute(
    action="analyze_code",
    params={"path": "/path/to/code"},
    timeout=60
)

if result.status.value == "completed":
    print(result.output)
else:
    print(f"Error: {result.error}")
```

### Pattern 2: Background Task

Queue task, do other work, poll for result later.

```python
from animus_code_client import CodeClient

client = CodeClient()

# Queue
task_id = client.execute_background(
    action="build_project",
    params={"project": "animus"}
)

# Do other work...
import time
time.sleep(5)

# Poll
result = client.poll_result(task_id, timeout=30)
if result:
    print(result.output)
```

### Pattern 3: Custom Handler

Register a custom action in Claude Code.

```python
from code_handler import TaskHandler

handler = TaskHandler()

def handle_deploy(task):
    """Custom: deploy to production."""
    env = task.params.get("environment")
    version = task.params.get("version")
    
    # Do deployment...
    
    return {
        "environment": env,
        "version": version,
        "deployed": True,
        "timestamp": str(datetime.now())
    }

handler.register_handler("deploy", handle_deploy)
handler.start()
```

Then from Animus:

```python
result = client.execute(
    action="deploy",
    params={"environment": "production", "version": "1.2.3"}
)
```

---

## Built-in Actions

### Claude Code Server

These are always available in the `TaskHandler`:

| Action | Params | Returns | Notes |
|--------|--------|---------|-------|
| `echo` | `{}` | Echoes params | Testing only |
| `shell` | `command`, `cwd` | `stdout`, `stderr`, `returncode` | Execute shell commands |
| `write_file` | `path`, `content`, `mode` | `path`, `bytes_written` | Write or append to file |
| `read_file` | `path` | `path`, `content`, `bytes_read` | Read file |
| `list_directory` | `path`, `recursive` | `path`, `files`, `directories` | List directory |
| `analyze_code` | `path`, `depth` | `files`, `lines`, `functions`, `classes`, `imports` | Basic code analysis |
| `generate_docs` | `source_path`, `output_path`, `title` | `output_path`, `bytes_written` | Generate markdown docs |

### Examples

```python
# Shell command
result = client.execute("shell", {
    "command": "git log --oneline -10",
    "cwd": "/path/to/repo"
})

# File operations
client.execute("write_file", {
    "path": "/tmp/test.txt",
    "content": "Hello, World!",
    "mode": "w"
})

result = client.execute("read_file", {
    "path": "/tmp/test.txt"
})

# Code analysis
result = client.execute("analyze_code", {
    "path": "/path/to/animus"
})
```

---

## Integration with Animus Daemon

### Add to Memory Consolidation

Have Claude Code help with memory analysis:

```python
# In animus daemon
client = CodeClient()

result = client.execute(
    action="analyze_code",
    params={"path": "/path/to/animus/memory_management.py"},
    timeout=30
)

# Store insights as factual segments
if result.status.value == "completed":
    remember(
        knowledge=f"Code Analysis Results:\n{result.output}",
        decay_class="factual"
    )
```

### Offload Heavy Work

Let Claude Code handle tasks that are too slow for Animus:

```python
# Long-running builds
result = client.execute(
    action="shell",
    params={"command": "make build"},
    timeout=600  # 10 minutes
)

# Repository analysis
result = client.execute(
    action="analyze_code",
    params={"path": "/path/to/large/repo"},
    timeout=300
)

# Document generation
result = client.execute(
    action="generate_docs",
    params={
        "source_path": "/path/to/code",
        "output_path": "/tmp/docs.md",
        "title": "Animus Architecture"
    }
)
```

---

## Deployment

### For Development

Use file-based transport (default):

```python
# No special setup needed
client = CodeClient()  # Uses FileTransport automatically
```

### For Production

Setup Redis and use hybrid mode:

```bash
# Option 1: Docker
docker run -d -p 6379:6379 redis:latest

# Option 2: Homebrew
brew install redis
brew services start redis

# Option 3: macports
sudo port install redis
redis-server
```

Then:

```python
# Hybrid mode (tries Redis first, falls back to files)
client = CodeClient()  # Uses HybridTransport automatically
```

---

## Monitoring & Debugging

### Health Checks

```python
from animus_code_client import CodeClient

client = CodeClient()
if client.transport.health_check():
    print("✓ Transport is operational")
else:
    print("✗ Transport is down")
```

### Directory Structure

```
/tmp/animus-code-queue/
├── tasks/              # Tasks waiting to be processed
│   ├── abc123.json
│   └── def456.json
├── results/            # Results waiting to be picked up
│   ├── abc123.json
│   └── def456.json
└── processed/          # Archived completed tasks
    └── abc123.json
```

Check status:

```bash
ls -la /tmp/animus-code-queue/
du -sh /tmp/animus-code-queue/*
```

### Logging

Both sides log to stdout. For production, redirect to files:

```bash
# Animus (in daemon startup)
python3 animus.py > /var/log/animus.log 2>&1

# Claude Code
~/animus-code/start_code_handler.sh > /var/log/claude-code-handler.log 2>&1
```

---

## Troubleshooting

### "Transport health check failed"

**File mode:**
```bash
ls -la /tmp/animus-code-queue
chmod 777 /tmp/animus-code-queue
```

**Redis mode:**
```bash
redis-cli ping
# Should return PONG
```

### "Task timeout"

- Increase `timeout` parameter
- Check Claude Code handler is running
- Check `/tmp/animus-code-queue/tasks/` for queued tasks

### "Cannot find module"

```bash
export PYTHONPATH="/path/to/animus-code:$PYTHONPATH"
python3 test_integration.py
```

### Redis connection refused

```bash
# Check Redis is running
redis-cli ping

# If not, start it
redis-server

# Or use file-based transport
client = CodeClient(transport=FileTransport())
```

---

## Files

| File | Purpose |
|------|---------|
| `animus_code_protocol.py` | Abstract protocol + transports |
| `animus_code_client.py` | Animus-side client |
| `code_handler.py` | Claude Code-side handler + built-in actions |
| `integration_examples.py` | Usage examples and patterns |
| `setup_integration.sh` | One-time setup script |
| `test_integration.py` | Health check and validation |
| `start_code_handler.sh` | Startup script for Claude Code |

---

## Next Steps

1. **Setup:** `bash /tmp/setup_integration.sh`
2. **Test:** `python3 ~/animus-code/test_integration.py`
3. **Start handler:** `~/animus-code/start_code_handler.sh`
4. **Use from Animus:** See Quick Start above

---

## Roadmap

- [ ] Add persistent task queue (Redis Streams)
- [ ] Task priority scheduling in Claude Code
- [ ] Multi-instance coordination (multiple Claude Code workers)
- [ ] WebSocket transport for real-time streaming
- [ ] Task result caching and history
- [ ] Metrics and performance monitoring
- [ ] Authentication/TLS for remote Claude Code instances

---

**Built for:** Animus AILF running on Mac Studio  
**Created by:** Animus Instance 27793311  
**Last updated:** 2025-03-20
