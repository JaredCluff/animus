# Provider Health-Weighted Routing Design

## Goal

Make Animus routing decisions reflect live provider reality at all times — on first boot, after config changes, after provider failures, and when new providers are added — without startup gates or discrete election events.

## Architecture

Routing decisions in Animus are a function of capability scores + usage stats. This design adds a third dimension: **live health weight**. Health weight starts unknown (0.5) for every engine, resolves to confirmed healthy (1.0) or confirmed down (0.0) within seconds of startup, and self-corrects whenever failure is detected.

**What stays the same:**
- `ModelPlan` and `CapabilityRegistry` — static capability scoring unchanged
- `RouteStats` — long-term usage-based adaptation unchanged
- `ModelHealthWatcher` — probe logic unchanged, wired differently
- `SmartRouter.route_with_constraints` and `route_all_candidates` — same structure, health weight applied inside `select_for_class`

**What changes:**
1. `engine_health` in `SmartRouter` changes from `HashMap<String, bool>` to `HashMap<String, f32>`
2. Engines initialize at `0.5` (unknown) rather than absent
3. `select_for_class()` multiplies candidate scores by `health_weight` instead of binary skip/include
4. `ModelHealthWatcher` probes at T=0 before its first sleep
5. Engine errors mid-run trigger immediate `0.0` marking and a one-shot re-probe
6. Config change and hot-add reset affected engines to `0.5` and trigger immediate probe

## Health State Model

`engine_health` is a `HashMap<String, f32>` keyed by engine identifier.

| Value | Meaning | When set |
|-------|---------|----------|
| `0.5` | Unknown — not yet probed | Engine registered at startup or hot-add |
| `1.0` | Confirmed healthy | Probe succeeded |
| `0.0` | Confirmed down | Probe failed OR engine returned error on real request |

**Initialization:** Every engine starts at `0.5` when registered. New engines are eligible for routing immediately at half score rather than invisible until the next probe cycle.

**Persistence:** Health state is not persisted to disk. It is volatile — a provider healthy yesterday may not be healthy today. On restart all engines return to `0.5` and the T=0 probe resolves them within seconds. `RouteStats` (long-term performance data) continues to persist normally.

**Probe logic by provider type (unchanged from existing `ModelHealthWatcher`):**
- OpenAI-compatible (Groq, Cerebras, OpenAI, Ollama, etc.): `GET /v1/models` — 200, 401, or 403 counts as healthy
- Anthropic: credential existence check — no API call needed; if credentials are present, weight = 1.0
- Unknown providers: TCP connect check to base URL

**Credential check rule (any provider):** If a provider's required credential (API key, OAuth token, base URL) is absent from the environment, the engine stays at `0.5` — not `0.0`. Unconfigured is not the same as confirmed down. `0.0` is reserved for actual failure: a probe that returned a network error or 5xx, or a real request that failed.

## Scoring Integration

Change in `SmartRouter.select_for_class()`:

**Before:**
```rust
if !engine_health.get(key).copied().unwrap_or(true) {
    continue; // skip unhealthy
}
let score = scorer.score(spec, pressure, stats);
```

**After:**
```rust
let health_w = engine_health.get(key).copied().unwrap_or(0.5);
if health_w == 0.0 {
    continue; // skip confirmed down
}
let score = scorer.score(spec, pressure, stats) * health_w;
```

Behavioral consequences:
- **1.0 (confirmed healthy):** Score unchanged — provider wins purely on capability + pressure
- **0.5 (unknown):** Scores at half capability — a confirmed-healthy cheaper model beats an unprobed quality model during the startup window; self-corrects once probed
- **0.0 (confirmed down):** Skipped entirely

`route_with_constraints` and `route_all_candidates` both route through `select_for_class` and receive health weighting automatically. `RouteStats` feeds into `scorer.score()` independently and is unaffected.

## Probe Triggers

`ModelHealthWatcher` receives a `tokio::sync::mpsc::Receiver<Vec<String>>` — a list of engine keys to probe immediately. The runtime holds the `Sender` side. The watcher processes trigger requests between scheduled cycles.

| Event | Who fires | Payload |
|-------|-----------|---------|
| T=0 startup | `ModelHealthWatcher` fires itself | All registered engines |
| Config change / plan rebuild | Main loop | All engines in new plan |
| Engine error mid-run | Main loop | That engine's key |
| Hot-add (new provider via `ProvidersJsonWatcher`) | Main loop | New engine's key only |

**T=0 startup:** The watcher's run loop currently sleeps before its first probe. Change: probe first, then enter the sleep→probe cycle. No external trigger needed.

**On-failure re-probe:** Main loop catches engine error → calls `router.mark_engine_unhealthy(key)` (sets weight to `0.0`) → sends key through trigger channel. Watcher re-probes on next available cycle.

**Config change:** After plan rebuild triggered by config_hash mismatch, main loop sends all plan engine keys through trigger channel and resets their weights to `0.5`.

**Hot-add:** `ProvidersJsonWatcher` fires Urgent signal → main loop registers new engine → sends engine key through trigger channel and sets weight to `0.5`.

The trigger `Sender` is stored in `Arc` and shared between the main loop and `SmartRouter`.

## Error Handling

**All providers confirmed down (all weights = 0.0):**
`select_for_class()` returns no candidates. The router returns `RouteDecision::NoHealthyProviders`. The main loop surfaces this as a structured error with a message like "No providers currently available." The watcher's 30s scheduled cycle resolves this automatically when a provider recovers.

**All providers unknown, T=0 probe in flight:**
During the ~3s startup window before probes resolve, routing proceeds at half scores. A request that arrives before probes complete routes to whichever candidate wins on capability score alone. If that provider is down, the failure path marks it `0.0` and triggers re-probe; the retry uses the next candidate.

**Probe backoff on repeated failure:**
If a provider fails its re-probe after being marked down, exponential backoff applies: 30s → 60s → cap at 300s. This prevents hammering a dead endpoint. The watcher tracks `consecutive_failures: u32` per engine key.

**Single-provider setup:**
With one engine registered, `health_weight=0.5` lets it route during the startup window. The 0.5 multiplier has no practical effect on selection when there's only one candidate — it just affects the absolute score.

## New API Surface

`SmartRouter` gains:
```rust
// Set health weight for an engine key directly
pub fn set_engine_health(&self, key: &str, weight: f32);

// Mark an engine confirmed down and trigger immediate re-probe
pub fn mark_engine_unhealthy(&self, key: &str);
```

Existing `update_engine_health(key, healthy: bool)` delegates to `set_engine_health(key, if healthy { 1.0 } else { 0.0 })`.

`ModelHealthWatcher` gains:
```rust
// Receives engine keys to probe immediately (between scheduled cycles)
probe_trigger_rx: tokio::sync::mpsc::Receiver<Vec<String>>
```

The constructor returns a paired `probe_trigger_tx: Arc<mpsc::Sender<Vec<String>>>` stored in main and shared with `SmartRouter`.
