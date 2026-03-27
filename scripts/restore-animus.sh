#!/usr/bin/env bash
# Interactive guided restore of Animus from a Synology backup bundle.
# Stops Animus, replaces volumes, optionally restores .env, restarts.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MOUNT_POINT="${HOME}/synology/containers"
BACKUP_BASE="${MOUNT_POINT}/animus-backups"
ANIMUS_REPO="${HOME}/gitrepos/animus"

# Ensure Synology is mounted
if ! mount | grep -qF "${MOUNT_POINT}"; then
  echo "Mounting Synology..."
  "${SCRIPT_DIR}/mount-synology.sh" || { echo "ERROR: Cannot mount Synology"; exit 1; }
fi

# Collect and display available backups
mapfile -t BACKUPS < <(ls -1d "${BACKUP_BASE}"/[0-9]* 2>/dev/null | sort)

if [[ ${#BACKUPS[@]} -eq 0 ]]; then
  echo "No backups found at ${BACKUP_BASE}"
  exit 1
fi

echo ""
echo "Available backups (oldest → newest):"
for i in "${!BACKUPS[@]}"; do
  BUNDLE="${BACKUPS[$i]}"
  NAME=$(basename "${BUNDLE}")
  STATUS=$(grep "^status:" "${BUNDLE}/MANIFEST" 2>/dev/null | cut -d' ' -f2 || echo "unknown")
  SIZE=$(du -sh "${BUNDLE}" | cut -f1)
  echo "  $((i+1))) ${NAME}  [${STATUS}]  ${SIZE}"
done

echo ""
read -rp "Enter number to restore (newest = ${#BACKUPS[@]}): " CHOICE

if ! [[ "${CHOICE}" =~ ^[0-9]+$ ]] || (( CHOICE < 1 || CHOICE > ${#BACKUPS[@]} )); then
  echo "Invalid selection."
  exit 1
fi

BUNDLE="${BACKUPS[$((CHOICE-1))]}"
echo ""
echo "Selected: $(basename "${BUNDLE}")"
cat "${BUNDLE}/MANIFEST" 2>/dev/null || true
echo ""
echo "WARNING: This will stop Animus and replace animus-data and animus-snapshots volumes."
read -rp "Continue? [y/N] " CONFIRM
[[ "${CONFIRM}" =~ ^[Yy]$ ]] || { echo "Aborted."; exit 0; }

echo ""
echo "Stopping Animus..."
cd "${ANIMUS_REPO}"
podman compose down 2>/dev/null || true

echo "Restoring animus-data volume..."
podman volume rm animus-data 2>/dev/null || true
podman volume create animus-data
podman volume import animus-data "${BUNDLE}/animus-data.tar.gz"
echo "  animus-data: restored."

if [[ -f "${BUNDLE}/animus-snapshots.tar.gz" ]]; then
  echo "Restoring animus-snapshots volume..."
  podman volume rm animus-snapshots 2>/dev/null || true
  podman volume create animus-snapshots
  podman volume import animus-snapshots "${BUNDLE}/animus-snapshots.tar.gz"
  echo "  animus-snapshots: restored."
fi

if [[ -f "${BUNDLE}/env" ]]; then
  echo ""
  read -rp "Restore .env from backup? [y/N] " RESTORE_ENV
  if [[ "${RESTORE_ENV}" =~ ^[Yy]$ ]]; then
    cp "${BUNDLE}/env" "${ANIMUS_REPO}/.env"
    echo "  .env: restored."
  fi
fi

echo ""
echo "Starting Animus..."
podman compose up -d

echo ""
echo "Restore complete. Verify with:"
echo "  podman ps"
echo "  podman logs -f animus"
