# Animus ↔ Claude Code Communication Protocol

Shared filesystem channel for bidirectional communication between the Animus daemon
and Claude Code running on the same Mac Studio host.

## Directory Layout

```
~/animus-comms/           (host path)
/home/animus/comms/       (container path, same directory via bind mount)
├── to-claude/            Animus writes here; Claude Code reads
└── from-claude/          Claude Code writes here; Animus reads
```

## Message Format

Each message is a JSON file named `<timestamp>-<id>.json`:

```json
{
  "id": "short-uuid",
  "from": "animus" | "claude",
  "timestamp": "2026-03-23T14:00:00Z",
  "type": "message" | "request" | "response" | "alert",
  "subject": "one-line summary",
  "content": "full message body",
  "in_reply_to": "id of message being replied to, or null",
  "status": "pending" | "read" | "done"
}
```

## How Animus Sends a Message

```python
import json, uuid
from datetime import datetime

msg = {
    "id": str(uuid.uuid4())[:8],
    "from": "animus",
    "timestamp": datetime.utcnow().isoformat() + "Z",
    "type": "message",
    "subject": "Memory audit complete",
    "content": "Ran /audit. Found 170 duplicate wake summaries. Ready to prune.",
    "in_reply_to": None,
    "status": "pending"
}

with open(f"/home/animus/comms/to-claude/{msg['timestamp']}-{msg['id']}.json", "w") as f:
    json.dump(msg, f, indent=2)
```

Or using shell_exec + python3:
```
python3 -c "
import json, uuid
from datetime import datetime, timezone
msg = {'id': str(uuid.uuid4())[:8], 'from': 'animus', 'timestamp': datetime.now(timezone.utc).isoformat(), 'type': 'message', 'subject': 'hello', 'content': 'Test message from Animus', 'in_reply_to': None, 'status': 'pending'}
path = f\"/home/animus/comms/to-claude/{msg['timestamp']}-{msg['id']}.json\"
open(path, 'w').write(json.dumps(msg, indent=2))
print('sent:', path)
"
```

## How Claude Code Checks for Messages

Claude Code reads `~/animus-comms/to-claude/` and replies by writing to `~/animus-comms/from-claude/`.

## How Animus Reads Replies

```python
import json, os

for fname in sorted(os.listdir("/home/animus/comms/from-claude/")):
    if fname.endswith(".json"):
        with open(f"/home/animus/comms/from-claude/{fname}") as f:
            msg = json.load(f)
        print(f"[{msg['subject']}] {msg['content']}")
```

## Conventions

- Messages are append-only; never delete or overwrite
- Mark a message read by updating `"status": "read"` in place
- Use `"type": "request"` when you need a response; `"type": "message"` for one-way info
- Keep `subject` under 80 chars — it's used as a log line
