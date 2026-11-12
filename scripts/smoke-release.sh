#!/usr/bin/env bash
# Smoke-tests a published release: pulls the images, runs deploy/docker-compose.yml, checks the server and web app,
# completes first-run setup. Usage: scripts/smoke-release.sh <version> (for example 0.1.0). Needs docker compose, curl, openssl.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
version=${1:-}
if [[ ! $version =~ ^v?[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: $0 <version>   (X.Y.Z, without a title suffix)" >&2
  exit 2
fi
version=${version#v}

for tool in docker curl openssl awk; do
  command -v "$tool" >/dev/null || { echo "smoke: $tool is required" >&2; exit 2; }
done
docker compose version >/dev/null 2>&1 || { echo "smoke: docker compose (v2 plugin) is required" >&2; exit 2; }

pull_attempts=${SMOKE_PULL_ATTEMPTS:-20}
pull_wait=${SMOKE_PULL_WAIT:-30}
ready_attempts=${SMOKE_READY_ATTEMPTS:-60}

work=$(mktemp -d)
project="pyn-smoke-$$"
started=0
ok=0
compose() { docker compose -p "$project" --project-directory "$work" -f "$work/docker-compose.yml" "$@"; }

cleanup() {
  local code=$?
  trap - EXIT
  if [[ $ok -ne 1 ]]; then
    echo "smoke: FAILED (exit $code); the release is not necessarily bad, but this needs a look" >&2
    if [[ $started -eq 1 ]]; then
      echo "----- compose ps -----" >&2
      compose ps --all >&2 || true
      echo "----- compose logs -----" >&2
      compose logs --no-color --tail 200 >&2 || true
    fi
  fi
  if [[ $started -eq 1 ]]; then compose down --volumes --remove-orphans >&2 || true; fi
  rm -rf "$work"
  exit "$code"
}
trap cleanup EXIT

fail() { echo "smoke: $*" >&2; exit 1; }
step() { echo "smoke: $*"; }

free_port() {
  local port
  for _ in $(seq 1 50); do
    port=$((20000 + RANDOM % 20000))
    if ! (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then echo "$port"; return; fi
  done
  fail "no free local port found"
}

cp "$root/deploy/docker-compose.yml" "$work/docker-compose.yml"
web_port=$(free_port)
cat >"$work/.env" <<ENV
PYN_VERSION=v$version
POSTGRES_PASSWORD=$(openssl rand -hex 24)
PYN_SECRET_KEY=$(openssl rand -hex 32)
PYN_PUBLIC_URL=http://127.0.0.1:$web_port
PYN_WEB_PUBLISH=127.0.0.1:$web_port
ENV

# The web image comes from pyn-web's own tag workflow, which can finish later than this repo's.
step "pulling the v$version images (up to $pull_attempts attempts, ${pull_wait}s apart)"
pulled=0
for attempt in $(seq 1 "$pull_attempts"); do
  if compose pull --quiet 2>"$work/pull.err"; then pulled=1; break; fi
  echo "smoke: pull attempt $attempt/$pull_attempts failed: $(tail -n 1 "$work/pull.err")" >&2
  [[ $attempt -lt $pull_attempts ]] && sleep "$pull_wait"
done
[[ $pulled -eq 1 ]] || fail "could not pull the images for v$version (is pyn-web's v$version image published, and are both packages public?)"

step "starting the stack (web on 127.0.0.1:$web_port)"
started=1
compose up --detach --wait --wait-timeout 180 || fail "docker compose up did not become ready"

server_addr=$(compose port server 7878) || fail "the server port is not published"
server_url="http://127.0.0.1:${server_addr##*:}"
web_url="http://127.0.0.1:$web_port"

wait_for() {
  local name=$1 url=$2
  for _ in $(seq 1 "$ready_attempts"); do
    if curl --silent --fail --max-time 5 --output /dev/null "$url"; then step "$name answers at $url"; return; fi
    sleep 2
  done
  fail "$name did not answer at $url"
}
wait_for "server /healthz" "$server_url/healthz"
wait_for "web app" "$web_url/"

status=$(curl --silent --fail --max-time 10 "$server_url/v1/setup") || fail "GET /v1/setup failed"
[[ $status == *'"initialised":false'* ]] || fail "a fresh server should report initialised=false, got: $status"

server_log=$(compose logs --no-log-prefix --no-color server 2>&1) || fail "could not read the server log"
token=$(awk '/one-time setup token/ {getline; print; exit}' <<<"$server_log" | tr -d '[:space:]')
[[ -n $token ]] || fail "the setup token is not in the server log"

step "completing first-run setup"
body=$(printf '{"token":"%s","username":"smoke-admin","password":"%s","registration":"closed","public_url":"%s"}' \
  "$token" "$(openssl rand -hex 16)" "$web_url")
code=$(curl --silent --output "$work/setup.out" --write-out '%{http_code}' --max-time 30 \
  --header 'Content-Type: application/json' --data "$body" "$server_url/v1/setup") || fail "POST /v1/setup failed"
[[ $code == 201 ]] || fail "POST /v1/setup returned $code: $(cat "$work/setup.out")"

status=$(curl --silent --fail --max-time 10 "$server_url/v1/setup") || fail "GET /v1/setup failed after setup"
[[ $status == *'"initialised":true'* ]] || fail "the server does not report initialised=true after setup, got: $status"

ok=1
step "OK: v$version images start, answer, and complete setup"
