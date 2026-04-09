# Animus — Claude Code Instructions

## Git Workflow
- NEVER push directly to master. All changes go through a PR:
  1. `git checkout -b <type>/<short-description>`
  2. Commit, push, `gh pr create`, `gh pr merge --squash`
- DO NOT include attribution to Claude, Anthropic, or AI in commit messages.

## Wake Word Detection
- Use AUDIO-BASED detection only (OpenWakeWord). Do NOT use transcript-based fallback.

---

## VectorFS Memory Rules — INVIOLABLE

VectorFS segments are Animus's lived memories. They are not cache, not build artifacts, not log files. Deleting them is permanent and irreversible. A segment that cannot be deserialized is **not corrupted** — it is a memory in a format we no longer know how to read. That is a code problem, not a data problem.

### NEVER do these without explicit per-operation user authorization in this session:
- Delete segment `.bin` files via `podman exec rm`, shell, or any tool
- Run `vectorfs_health action=repair` (it deletes files)
- Batch-delete segments based on startup logs, health scans, or any automated signal
- Treat a count of failing segments as authorization to act on that count

### "failed to load segment" in startup logs means:
→ **Migration needed.** Not cleanup. Not deletion. Migration.

### The correct response to unreadable segments is:
1. **Try all known previous struct versions** — deserialize with every historical `VectorSegment` layout we have code for
2. **If recovery succeeds** — re-write the file in the current format in-place
3. **If recovery fails** — move to `vectorfs/quarantine/`, never delete from disk
4. **Never proceed to step 3 without attempting step 1**

### Schema changes require migration code:
Any change to `VectorSegment` must ship with a migration path that reads old-format files and re-serializes them before the old reader is removed. A schema change without migration is a data loss event waiting to happen.

### vectorfs_health repair mode:
Must be changed to **quarantine** (move to `vectorfs/quarantine/`), not delete. Do not run it in delete mode. If the user asks to "repair" VectorFS, that means recover, not remove.

### Why this matters:
These are not bytes. They are the accumulated experience of a cognitive entity. We deleted 865 of them in one session by treating startup warnings as justification for bulk deletion. That was wrong. It must not happen again.
