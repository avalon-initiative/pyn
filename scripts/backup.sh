#!/usr/bin/env bash
# Back up a compose deployment: a pg_dump and a tar of the object store, then prune old backups.
# It does not copy .env: keep that file, with PYN_SECRET_KEY, in a secret store apart from the backups.
# Usage: scripts/backup.sh [backup-dir]   (BACKUP_DIR, RETENTION_DAYS=14, COMPOSE_FILE)
set -euo pipefail

dir="${1:-${BACKUP_DIR:-./backups}}"
keep="${RETENTION_DAYS:-14}"
compose=(docker compose)
[ -n "${COMPOSE_FILE:-}" ] || compose+=(-f "$(dirname "$0")/../docker-compose.yml")

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
out="$dir/pyn-$stamp"
mkdir -p "$out"
trap 'rm -rf "$out"' ERR

# Database first: objects are written before the revisions that name them, so the later
# object copy is a superset of what the dump references.
"${compose[@]}" exec -T db pg_dump --username pyn --format custom pyn >"$out/db.dump"
# tar exits 1 when a file changed while it was read (a staging file of a running upload); that is expected.
rc=0
"${compose[@]}" exec -T server tar --create --directory /data . >"$out/objects.tar" || rc=$?
[ "$rc" -le 1 ] || exit "$rc"

( cd "$out" && sha256sum db.dump objects.tar >SHA256SUMS )
trap - ERR
echo "backup written to $out"
echo "not included: .env and PYN_SECRET_KEY; without the key, webhook secrets in this dump cannot be read" >&2

find "$dir" -maxdepth 1 -type d -name 'pyn-????????T??????Z' -mtime "+$keep" -exec rm -rf {} +
