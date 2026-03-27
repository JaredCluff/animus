# Animus Snapshot Backup to Synology — Design Spec

**Date:** 2026-03-26
**Status:** Approved, pending implementation

## Problem

Animus is deployed as a Podman pod on the Mac at 192.168.0.200. When the container is rebuilt
or replaced, critical AILF state (VectorFS brain, identity keypair, goals) can be lost. In-container
snapshots are also lost because the snapshot directory is not a named volume — it lives in the
container overlay filesystem and is wiped on every rebuild. This spec defines a backup system that
sends Animus state to a Synology NAS every 6 hours, enabling full recovery.

## What Gets Backed Up

| Item | Source | Contents |
|---|---|---|
| `animus-data` volume | Podman named volume | VectorFS (HNSW index), identity.bin, goals.bin, quality.bin, audit log, bootstrap marker |
| `animus-snapshots` volume | New named volume (see compose fix) | In-container memory checkpoints created by `snapshot_memory` tool |
| `.env` | `~/gitrepos/animus/.env` | All credentials and config needed to restart Animus |
| `animus-comms/` | `~/animus-comms/` | Bind-mounted comms channel between Animus and Claude Code |

## Backup Process (Live, No Downtime)

Each backup run follows this sequence:

1. **Health check** — `curl http://localhost:8082/health`
   - If healthy: run `podman exec animus sync` to flush dirty mmap pages before exporting
   - If unhealthy/dead: skip sync, export anyway (crash-consistent backup beats no backup)

2. **Export volumes** — two separate exports:
   ```
   podman volume export animus-data    | gzip > animus-data-TIMESTAMP.tar.gz
   podman volume export animus-snapshots | gzip > animus-snapshots-TIMESTAMP.tar.gz
   ```

3. **Bundle** — create a timestamped directory:
   ```
   YYYYMMDD-HHMMSS/
   ├── animus-data.tar.gz
   ├── animus-snapshots.tar.gz
   ├── env                      (copy of .env)
   └── animus-comms.tar.gz
   ```

4. **Mount Synology** — `mount_smbfs` the `containers` share:
   ```
   //animus@192.168.1.135/containers/animus-backups/
   ```
   Password retrieved from macOS Keychain (not hardcoded).

5. **Copy** — rsync the bundle directory to the mounted share.

6. **Prune** — keep the 28 most recent bundles (7 days at 4/day), delete older ones.

7. **Log** — append a one-line result (timestamp, status, sizes) to `~/animus-backups.log`.

8. **Unmount** — cleanly unmount the SMB share.

## Restore Procedure

To recover Animus from a backup:

```bash
# 1. Stop running containers
cd ~/gitrepos/animus
podman compose down

# 2. Remove corrupted volume (if any)
podman volume rm animus-data
podman volume rm animus-snapshots

# 3. Mount backup share
mount_smbfs //animus@192.168.1.135/containers/animus-backups /tmp/animus-restore

# 4. Pick a backup (newest = last entry)
ls /tmp/animus-restore/

# 5. Restore volumes
podman volume create animus-data
podman volume import animus-data /tmp/animus-restore/CHOSEN/animus-data.tar.gz

podman volume create animus-snapshots
podman volume import animus-snapshots /tmp/animus-restore/CHOSEN/animus-snapshots.tar.gz

# 6. Restore .env if needed
cp /tmp/animus-restore/CHOSEN/env ~/gitrepos/animus/.env

# 7. Restart
podman compose up -d

# 8. Clean up
umount /tmp/animus-restore
```

## compose.yaml Fix

Add `animus-snapshots` as a named volume so in-container checkpoints survive rebuilds:

```yaml
# In services.animus.volumes, add:
- animus-snapshots:/home/animus/.animus-snapshots

# In top-level volumes, add:
animus-snapshots:
```

## Scheduling

- **Tool:** macOS `launchd` plist at `~/Library/LaunchAgents/ai.animus.backup.plist`
- **Interval:** every 21600 seconds (6 hours)
- **Script location:** `~/gitrepos/animus/scripts/backup-animus.sh`
- **Logs:** stdout/stderr to `~/animus-backup.log`

## Credentials

Synology SMB password stored in macOS Keychain:
```bash
security add-internet-password \
  -a animus \
  -s 192.168.1.135 \
  -P 445 \
  -w '<password>'
```

Script retrieves it at runtime:
```bash
SYNOLOGY_PASS=$(security find-internet-password -a animus -s 192.168.1.135 -w)
```

## Retention Policy

- Keep last **28 bundles** (7 days of coverage at 4 backups/day)
- Prune runs after every successful copy
- If copy fails, skip prune (don't delete old backups when new ones didn't land)

## Files Touched

| File | Change |
|---|---|
| `scripts/backup-animus.sh` | New — backup script |
| `scripts/restore-animus.sh` | New — guided restore helper |
| `compose.yaml` | Add `animus-snapshots` volume |
| `~/Library/LaunchAgents/ai.animus.backup.plist` | New — launchd schedule |
