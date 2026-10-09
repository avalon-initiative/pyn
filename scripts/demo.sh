#!/usr/bin/env bash
# Seeds a running server with a small game-like repository: shared source, exclusive assets, two locks.
set -euo pipefail

PYN=${PYN:-target/debug/pyn}
export PYN_SERVER=${PYN_SERVER:-http://127.0.0.1:7878}
export PYN_REPO=${PYN_REPO:-alice/demo}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

head_rev() {
  "$PYN" --user demo history "$1" | tail -n 1 | cut -f1
}

# save <user> <path> <content> <message> [lock]: check in a new revision, taking the lock first if asked
save() {
  local user=$1 path=$2 content=$3 message=$4 lock=${5:-}
  local base
  base=$(head_rev "$path")
  local args=()
  [ -n "$base" ] && args=(--base "$base")
  if [ -n "$lock" ]; then
    "$PYN" --user "$user" checkout "$path" ${args[@]+"${args[@]}"} >/dev/null
  fi
  printf '%s\n' "$content" > "$tmp/file"
  "$PYN" --user "$user" checkin "$path" "$tmp/file" ${args[@]+"${args[@]}"} -m "$message" >/dev/null
  echo "  $user: $path"
}

echo "creating $PYN_REPO"
"$PYN" --user "${PYN_REPO%%/*}" repo create "${PYN_REPO#*/}" >/dev/null 2>&1 || true

echo "checking in files"
save alice Source/Player.cpp "class Player {};" "player skeleton"
save bob Source/Enemy.cpp "class Enemy {};" "enemy skeleton"
save alice Config/ProductionConfig.cpp 'const char* env = "prod";' "production config" lock
save bob Content/World/Dungeon.umap "dungeon v1" "first dungeon layout" lock
save alice Content/Characters/Knight.uasset "knight v1" "knight mesh" lock

echo "taking locks"
"$PYN" --user alice checkout Content/World/Main.umap >/dev/null && echo "  alice: Content/World/Main.umap"
"$PYN" --user bob checkout Content/Enemies/Boss.uasset >/dev/null && echo "  bob: Content/Enemies/Boss.uasset"

echo
"$PYN" --user demo files
