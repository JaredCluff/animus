# Watcher Subsystem Design

**Date:** 2026-03-23
**Status:** Approved
**Codebase:** `JaredCluff/animus`

---

## Problem

Animus's existing cognitive loops (Perception, Reflection) consume LLM tokens on every cycle regardless of whether anything meaningful happened. There is no lightweight mechanism for Animus to monitor specific conditions — like an incoming message, a threshold breach, or a file appearing — without routing every check through an LLM.

The result: Animus either misses conditions entirely (no polling) or burns attention on noise (constant LLM classification). Neither is appropriate for a stateful AI entity.

---

## Solution

A **Watcher subsystem** — pre-cognitive, event-driven condition monitors that live in `animus-cortex` and emit `Signal`s directly to the reasoning thread only when a condition is met. No LLM involvement unless triggered. Toggleable at runtime by both Jared (slash command) and Animus (tool call). State persists across restarts.

**Cognitive placement:**

```
Sensorium     → raw observations (peripheral nervous system)
[Watchers]    → subcortical reflexes — condition-matched, no LLM
Perception    → meaning-making from observations (LLM)
Reflection    → synthesis of knowledge (LLM)
Reasoning     → conscious action and response (LLM)
```

Watchers are reflexes. They decide what deserves conscious attention without consuming cortical resources.

---

## Architecture

### The `Watcher` Trait

Defined in `animus-cortex`, compiled-in logic:

```rust
pub trait Watcher: Send + Sync {
    /// Stable identifier — used as the persistence key.
    fn id(&self) -> &str;

    /// Human-readable name.
    fn name(&self) -> &str;

    /// Default poll interval if not overridden in config.
    fn default_interval(&self) -> Duration;

    /// The check — pure Rust, no LLM, no async.
    /// Returns Some(event) if the condition is met, None otherwise.
    fn check(&self, config: &WatcherConfig) -> Option<WatcherEvent>;
}

/// Lightweight output from a watcher check. The registry promotes this
/// to a full Signal (filling in thread IDs) before sending.
pub struct WatcherEvent {
    pub priority: SignalPriority,
    pub summary: String,
    pub segment_refs: Vec<SegmentId>,
}
```

`check()` returns a `WatcherEvent` rather than a `Signal` directly, because watchers have no `ThreadId` at check time. The `WatcherRegistry` wraps `WatcherEvent` into a full `Signal` at dispatch, using a stable `source_thread` ID owned by the registry and `ThreadId::default()` as target (matching the existing convention used by Perception and Reflection).

**Separation of concerns:**
- **Logic is compiled** — the `check()` function is Rust code, versioned with the binary
- **Application is dynamic** — which watchers are enabled, their intervals, their parameters are runtime config persisted to disk

### `WatcherConfig`

```rust
pub struct WatcherConfig {
    pub enabled: bool,
    pub interval: Option<Duration>,      // overrides watcher's default_interval()
    pub params: serde_json::Value,       // watcher-specific settings (paths, thresholds, etc.)
    pub last_checked: Option<DateTime<Utc>>,
    pub last_fired: Option<DateTime<Utc>>,
}
```

### `WatcherRegistry`

Lives in `animus-cortex`, owns all watcher lifecycle:

```rust
pub struct WatcherRegistry {
    watchers: Vec<Box<dyn Watcher>>,
    configs: HashMap<String, WatcherConfig>,  // keyed by watcher.id()
    signal_tx: mpsc::Sender<Signal>,
    source_id: ThreadId,                      // stable ID used as Signal::source_thread
    store_path: PathBuf,                      // ~/.animus/watchers.json
}
```

**Startup sequence:**
1. Registry is constructed with all compiled-in watchers registered
2. `watchers.json` is loaded from `ANIMUS_DATA_DIR`
   - If missing: proceed with empty configs (all watchers get defaults)
   - If present but invalid JSON: log a warning, proceed with empty configs (graceful degradation — never fatal)
   - Unknown watcher IDs in the file (removed from binary): silently ignored
3. Each watcher receives its saved config, or a default `WatcherConfig` (disabled) if none saved
4. Registry spawns a single tokio task for the poll loop

**Poll loop:**
```
const IDLE_SLEEP: Duration = 5s  // used when no watchers are enabled

loop {
    let mut next_wake = now + IDLE_SLEEP

    for watcher in registered_watchers:
        config = configs[watcher.id()]
        if !config.enabled: skip

        effective_interval = config.interval ?? watcher.default_interval()
        due_at = config.last_checked + effective_interval

        if now < due_at:
            next_wake = min(next_wake, due_at)
            skip

        if let Some(event) = watcher.check(config):
            let signal = Signal {
                source_thread: registry.source_id,
                target_thread: ThreadId::default(),
                priority: event.priority,
                summary: event.summary,
                segment_refs: event.segment_refs,
                created: now,
            }
            signal_tx.send(signal)
            config.last_fired = now

        config.last_checked = now
        next_wake = min(next_wake, now + effective_interval)

    sleep(next_wake - now)  // at minimum IDLE_SLEEP, never negative
}
```

Single loop, no per-watcher tasks. A watcher that finds nothing costs microseconds. No LLM calls in the hot path. When no watchers are enabled, the loop sleeps for the 5s idle interval rather than spinning.

**Sleep state interaction:** Watchers continue polling during Animus's sleep state. The main loop's `signal_rx.try_recv()` drain is unconditional — signals accumulate in the inbox and are injected into the next `process_turn()` call when Animus wakes or receives input. This is consistent with how Perception and Reflection signals already behave during sleep.

**Persistence:** any `update_config()` call atomically writes `watchers.json` (write to `.tmp`, rename). Enabled/disabled state, interval overrides, and params survive container restarts.

---

## Control Surfaces

Both surfaces call the same `WatcherRegistry::update_config()` method.

### Slash Commands (Jared)

```
/watch list                              — show all watchers: id, name, enabled, interval, last fired
/watch enable <id> [interval=<dur>]      — enable watcher, optional interval override (e.g. interval=30s)
/watch disable <id>                      — disable watcher
/watch set <id> <key>=<value>            — update a param (e.g. dir=/home/animus/comms/from-claude)
```

### LLM Tool (Animus)

New `manage_watcher` tool registered in `animus-cortex`:

```json
{
  "name": "manage_watcher",
  "description": "Enable, disable, or configure a background watcher. Watchers monitor conditions without LLM involvement and signal you when something requires attention.",
  "parameters": {
    "type": "object",
    "properties": {
      "action":      { "type": "string", "enum": ["enable", "disable", "list", "set_param"] },
      "watcher_id":  { "type": "string", "description": "Required for enable, disable, set_param" },
      "interval_secs": { "type": "integer", "description": "Optional poll interval override in seconds" },
      "params":      { "type": "object", "description": "Key-value pairs to merge into watcher params" }
    },
    "required": ["action"]
  }
}
```

`watcher_id` is required for `enable`, `disable`, and `set_param`; optional for `list`. `interval_secs` provides parity with the slash command's `interval=` flag so Animus can set custom poll intervals autonomously.

Animus can enable a watcher when it recognizes a recurring pattern to monitor, or disable one generating too much noise.

---

## First Concrete Watcher: `CommsWatcher`

The use case that motivated the design: Animus receiving messages from Claude Code via the shared filesystem channel.

```rust
pub struct CommsWatcher;

impl Watcher for CommsWatcher {
    fn id(&self) -> &str { "comms" }
    fn name(&self) -> &str { "Claude Code Comms" }
    fn default_interval(&self) -> Duration { Duration::from_secs(30) }

    fn check(&self, config: &WatcherConfig) -> Option<WatcherEvent> {
        let dir = config.params["dir"].as_str()?;

        // Scan dir for *.json files with "status": "pending"
        // For each found:
        //   - Read content
        //   - Mark as "read" in-place (atomic write: .tmp + rename)
        //   - Collect subject + content into batch

        // If batch is empty: return None (zero LLM cost)

        // If batch non-empty: return Some(WatcherEvent {
        //   priority: Normal (Urgent if any message type == "alert"),
        //   summary: formatted batch of message subjects and content,
        //   segment_refs: vec![],
        // })
    }
}
```

Default config when enabled:
```json
{
  "enabled": true,
  "params": { "dir": "/home/animus/comms/from-claude" }
}
```

The `WatcherEvent` is promoted to a full `Signal` by the registry and flows through the existing `mpsc` channel into the reasoning thread's `pending_signals` inbox, injected into the next LLM prompt as "## Inter-Thread Signals." No new plumbing required.

---

## Data Flow

```
[CommsWatcher.check()]
        │
        │ Some(WatcherEvent) — only when pending messages exist
        ▼
[WatcherRegistry: wraps into Signal with source_id/target ThreadId::default()]
        │
        ▼
[signal_tx: mpsc::Sender<Signal>]     ← same channel Perception and Reflection use
        │
        ▼
[main loop: signal_rx.try_recv()]
        │
        ▼
[scheduler.active_thread_mut().deliver_signal(signal)]
        │
        ▼
[thread.pending_signals inbox]
        │
        ▼
[next process_turn() → injected into system prompt as "## Inter-Thread Signals"]
        │
        ▼
[LLM reasoning turn — Animus responds to the message]
```

---

## File Layout

```
crates/animus-cortex/src/
├── watcher.rs              — Watcher trait, WatcherEvent, WatcherConfig, WatcherRegistry
├── watchers/
│   └── comms.rs            — CommsWatcher implementation
├── tools/
│   └── manage_watcher.rs   — manage_watcher tool
├── perception.rs           (unchanged)
└── reflection.rs           (unchanged)
```

`WatcherRegistry` is wired in `animus-runtime/src/main.rs` alongside the existing loop setup.

---

## Out of Scope

- Dynamic watcher definition at runtime (logic must be compiled)
- Watcher-to-watcher dependencies
- Watcher output other than Signal (e.g. direct tool execution)
- Federation of watchers across Animus instances

---

## Success Criteria

1. A watcher that finds nothing costs zero LLM tokens
2. A watcher that fires delivers a Signal to the reasoning thread indistinguishable from a Perception or Reflection signal
3. `/watch enable comms` survives a container restart (config persisted)
4. Animus can autonomously toggle watchers and set intervals via `manage_watcher` tool
5. `CommsWatcher` correctly detects pending messages from Claude Code and marks them read before signaling
6. A corrupt or missing `watchers.json` degrades gracefully to all-disabled defaults without crashing
