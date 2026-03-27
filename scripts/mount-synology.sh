#!/usr/bin/env bash
# Mounts the Synology 'containers' SMB share at ~/synology/containers.
# Idempotent — safe to call multiple times. Retries 3x on failure.
# NOTE: mount_smbfs on macOS embeds credentials in the process argv; this is a
# known macOS limitation with no workaround via this tool. The window is narrow
# (duration of the mount_smbfs call only) and the risk is low on a home network.
set -uo pipefail

MOUNT_POINT="${HOME}/synology/containers"
SYNOLOGY_HOST="192.168.1.135"
SYNOLOGY_USER="animus"
SYNOLOGY_SHARE="containers"
CREDS_FILE="${HOME}/.animus-synology-pass"
MAX_RETRIES=3
RETRY_DELAY=5

mkdir -p "${MOUNT_POINT}"

# Already mounted — nothing to do
if mount | grep -qF "${MOUNT_POINT}"; then
  exit 0
fi

[[ -f "${CREDS_FILE}" ]] || { echo "ERROR: ${CREDS_FILE} not found. Create it with the Synology password (chmod 600)." >&2; exit 1; }

PERMS=$(stat -f "%OLp" "${CREDS_FILE}")
[[ "${PERMS}" == "600" ]] || { echo "ERROR: ${CREDS_FILE} must be chmod 600 (currently ${PERMS})." >&2; exit 1; }

PASS=$(cat "${CREDS_FILE}")
[[ -n "${PASS}" ]] || { echo "ERROR: ${CREDS_FILE} is empty." >&2; exit 1; }

for (( i=1; i<=MAX_RETRIES; i++ )); do
  if mount_smbfs "//${SYNOLOGY_USER}:${PASS}@${SYNOLOGY_HOST}/${SYNOLOGY_SHARE}" "${MOUNT_POINT}" 2>/dev/null; then
    echo "Mounted //${SYNOLOGY_HOST}/${SYNOLOGY_SHARE} at ${MOUNT_POINT}"
    exit 0
  fi
  if (( i < MAX_RETRIES )); then
    echo "Mount attempt $i failed — retrying in ${RETRY_DELAY}s..."
    sleep ${RETRY_DELAY}
  fi
done

echo "ERROR: Failed to mount Synology after ${MAX_RETRIES} attempts." >&2
exit 1
