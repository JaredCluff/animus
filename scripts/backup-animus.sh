#!/usr/bin/env bash
# Backs up Animus Podman volumes, .env, and comms to the Synology NAS.
# Runs live — no container stop. Flushes mmap pages if Animus is healthy.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)
MOUNT_POINT="${HOME}/synology/containers"
BACKUP_BASE="${MOUNT_POINT}/animus-backups"
BUNDLE="${BACKUP_BASE}/${TIMESTAMP}"
LOG_FILE="${HOME}/animus-backup.log"
ANIMUS_REPO="${HOME}/gitrepos/animus"
ANIMUS_COMMS="${HOME}/animus-comms"
MAX_BACKUPS=28
STATUS="SUCCESS"

# podman volume export does not stream through the macOS socket layer — run
# inside the Podman VM via SSH and pipe the tar back to the host.
pm_vol_export() {
  podman machine ssh "podman volume export $1" 2>/dev/null
}

log() {
  echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" | tee -a "${LOG_FILE}"
}

# Ensure Synology is mounted
if ! mount | grep -qF "${MOUNT_POINT}"; then
  log "Synology not mounted — attempting to mount..."
  if ! "${SCRIPT_DIR}/mount-synology.sh" >> "${LOG_FILE}" 2>&1; then
    log "FAIL: Could not mount Synology. Backup aborted."
    exit 1
  fi
fi

log "=== Backup started: ${TIMESTAMP} ==="
mkdir -p "${BUNDLE}"

# Flush dirty mmap pages if Animus is healthy
if curl -sf --max-time 3 http://localhost:8082/health > /dev/null 2>&1; then
  log "Animus healthy — flushing dirty pages before export"
  podman exec animus /bin/sync 2>/dev/null || true
else
  log "Animus not healthy — proceeding with crash-consistent export"
  STATUS="DEGRADED"
fi

# Export animus-data volume
log "Exporting animus-data..."
if pm_vol_export animus-data | gzip > "${BUNDLE}/animus-data.tar.gz"; then
  SIZE=$(du -sh "${BUNDLE}/animus-data.tar.gz" | cut -f1)
  log "  animus-data: ${SIZE}"
else
  log "  WARN: animus-data export failed"
  STATUS="PARTIAL"
fi

# Export animus-snapshots volume (present after compose.yaml fix)
if podman volume inspect animus-snapshots > /dev/null 2>&1; then
  log "Exporting animus-snapshots..."
  if pm_vol_export animus-snapshots | gzip > "${BUNDLE}/animus-snapshots.tar.gz"; then
    SIZE=$(du -sh "${BUNDLE}/animus-snapshots.tar.gz" | cut -f1)
    log "  animus-snapshots: ${SIZE}"
  else
    log "  WARN: animus-snapshots export failed"
  fi
fi

# Copy .env
if [[ -f "${ANIMUS_REPO}/.env" ]]; then
  cp "${ANIMUS_REPO}/.env" "${BUNDLE}/env"
  log "  .env copied"
fi

# Archive animus-comms
if [[ -d "${ANIMUS_COMMS}" ]]; then
  tar czf "${BUNDLE}/animus-comms.tar.gz" \
    -C "$(dirname "${ANIMUS_COMMS}")" \
    "$(basename "${ANIMUS_COMMS}")" 2>/dev/null || true
  log "  animus-comms archived"
fi

# Write manifest
cat > "${BUNDLE}/MANIFEST" <<EOF
timestamp: ${TIMESTAMP}
status: ${STATUS}
host: $(hostname)
EOF

BUNDLE_SIZE=$(du -sh "${BUNDLE}" | cut -f1)
log "Bundle written: ${BUNDLE_SIZE} at ${BUNDLE}"

# Prune old bundles — only when backup did not partially fail
if [[ "${STATUS}" != "PARTIAL" ]]; then
  EXISTING=$(ls -1d "${BACKUP_BASE}"/[0-9]* 2>/dev/null | wc -l | tr -d ' ')
  if (( EXISTING > MAX_BACKUPS )); then
    log "Pruning (have ${EXISTING}, keeping ${MAX_BACKUPS})..."
    ls -1d "${BACKUP_BASE}"/[0-9]* 2>/dev/null | sort | head -n "-${MAX_BACKUPS}" | while read -r old; do
      rm -rf "${old}"
      log "  Pruned: $(basename "${old}")"
    done
  fi
fi

log "=== Backup ${STATUS}: ${TIMESTAMP} ==="
