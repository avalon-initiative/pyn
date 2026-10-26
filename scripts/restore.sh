#!/usr/bin/env bash
# Restore a backup made by scripts/backup.sh into a compose deployment, replacing its database and objects.
# Usage: scripts/restore.sh --yes <backup-dir>
set -euo pipefail

if [ "${1:-}" != "--yes" ] || [ -z "${2:-}" ]; then
	echo "usage: $0 --yes <backup-dir>   (replaces the current database and objects)" >&2
	exit 2
fi
src="$2"
compose=(docker compose)
[ -n "${COMPOSE_FILE:-}" ] || compose+=(-f "$(dirname "$0")/../docker-compose.yml")

( cd "$src" && sha256sum --check --quiet SHA256SUMS )

"${compose[@]}" stop server
"${compose[@]}" up --detach --wait db

"${compose[@]}" exec -T db dropdb --username pyn --if-exists --force pyn
"${compose[@]}" exec -T db createdb --username pyn pyn
"${compose[@]}" exec -T db pg_restore --username pyn --dbname pyn --no-owner <"$src/db.dump"

# The server container is stopped, so write the objects through a one-off container on the same volume.
"${compose[@]}" run -T --rm --no-deps --user root --entrypoint sh server -c \
	'find /data -mindepth 1 -delete && tar --extract --directory /data && chown -R pyn:pyn /data' <"$src/objects.tar"

"${compose[@]}" up --detach --wait server
echo "restored from $src"
