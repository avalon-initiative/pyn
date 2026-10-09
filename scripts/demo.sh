#!/usr/bin/env bash
# Seeds a running server with two repositories, three accounts and a few held locks. Safe to run again.
set -euo pipefail

PYN=${PYN:-target/debug/pyn}
export PYN_SERVER=${PYN_SERVER:-http://127.0.0.1:7878}
ADMIN=${PYN_BOOTSTRAP_ADMIN:-admin}
ADMIN_PASSWORD=${PYN_BOOTSTRAP_PASSWORD:-demo-password}
RUN_DIR=${RUN_DIR:-_running}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Each account keeps its own saved sign-in so the real ~/.config/pyn is untouched.
as() {
  local user=$1
  shift
  env "PYN_CONFIG_DIR=$RUN_DIR/demo-config/$user" ${REPO:+"PYN_REPO=$REPO"} "$PYN" "$@"
}

REPO=
password_of() {
  if [ "$1" = "$ADMIN" ]; then echo "$ADMIN_PASSWORD"; else echo "$1-password"; fi
}

signin() {
  password_of "$1" | as "$1" login "$1" --password-stdin >/dev/null
}

for _ in $(seq 1 60); do
  curl -fs "$PYN_SERVER/healthz" >/dev/null 2>&1 && break
  sleep 0.5
done
curl -fs "$PYN_SERVER/healthz" >/dev/null || { echo "no server at $PYN_SERVER" >&2; exit 1; }

echo "accounts"
for user in alice bob; do
  password_of "$user" | as "$user" register "$user" --password-stdin >/dev/null 2>&1 || true
done
for user in "$ADMIN" alice bob; do
  signin "$user"
  echo "  $user"
done

# save <user> <path> <content> <message> [lock]: first revision of a path, taking its lock first if asked
save() {
  local user=$1 path=$2 content=$3 message=$4 lock=${5:-}
  if [ -n "$(as "$user" history "$path" 2>/dev/null)" ]; then return; fi
  [ -n "$lock" ] && as "$user" checkout "$path" >/dev/null
  printf '%s\n' "$content" > "$tmp/file"
  as "$user" checkin "$path" "$tmp/file" -m "$message" >/dev/null
  echo "  $user: $path"
}

# hold <user> <path>: take the lock on an existing file at its head revision
hold() {
  local base
  base=$(as "$1" history "$2" | tail -n 1 | cut -f1)
  as "$1" checkout "$2" --base "$base" >/dev/null 2>&1 || true
}

seed_game() {
  REPO=$ADMIN/demo
  echo "repository $REPO"
  as "$ADMIN" repo create demo >/dev/null 2>&1 || true
  as "$ADMIN" member set alice writer >/dev/null
  as "$ADMIN" member set bob writer >/dev/null
  save alice Source/Player.cpp "class Player {};" "player skeleton"
  save bob Source/Enemy.cpp "class Enemy {};" "enemy skeleton"
  save bob Source/Systems/Combat/Damage.cpp "int damage(int hp) { return hp - 1; }" "damage rules"
  save alice docs/Design/Overview.md "# Overview" "design overview"
  save alice README.md "# Demo game" "readme"
  save alice Config/ProductionConfig.cpp 'const char* env = "prod";' "production config" lock
  save bob Content/World/Dungeon.umap "dungeon v1" "first dungeon layout" lock
  save alice Content/Characters/Knight.uasset "knight v1" "knight mesh" lock
  save bob Content/Characters/Props/Sword.uasset "sword v1" "sword mesh" lock

  echo "locks"
  as alice checkout Content/World/Main.umap >/dev/null 2>&1 || true
  as bob checkout Content/Enemies/Boss.uasset >/dev/null 2>&1 || true
  hold alice Content/Characters/Knight.uasset
  hold bob Content/World/Dungeon.umap
}

seed_tools() {
  REPO=bob/tools
  echo "repository $REPO"
  as bob repo create tools --visibility public >/dev/null 2>&1 || true
  as bob member set alice reader >/dev/null
  save bob docs/Usage.md "# Tools" "usage notes"
  save bob Source/build.cpp "int main() {}" "build script"
}

seed_game
seed_tools

REPO=$ADMIN/demo
echo
as "$ADMIN" ls
echo
as "$ADMIN" locks
