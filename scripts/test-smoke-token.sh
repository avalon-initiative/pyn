#!/usr/bin/env bash
# Tests the setup token extraction of scripts/smoke-release.sh on logs with and without interleaved lines.
set -euo pipefail

smoke="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/smoke-release.sh"
token=515841651deddcc5097c0338a5f862507551858db4a6b6c2

extract() { bash "$smoke" --setup-token-from-log; }
expect() {
  local name=$1 want=$2 got
  got=$(extract)
  [[ $got == "$want" ]] || { echo "test-smoke-token: $name: wanted '$want', got '$got'" >&2; exit 1; }
}

expect "plain block" "$token" <<LOG
----- pyn one-time setup token -----
$token
------------------------------------
finish setup in the browser at http://127.0.0.1:8080, or run: pyn setup <username>
LOG

expect "log lines between the header and the token" "$token" <<LOG
2026-11-20T22:02:09.655212Z  WARN pyn_server: this server is not set up yet; the one-time setup token is printed on stderr
----- pyn one-time setup token -----
2026-11-20T22:02:09.655239Z  WARN pyn_server: PYN_PUBLIC_URL is not https and the server is bound beyond loopback
$token
2026-11-20T22:02:09.655257Z  INFO pyn_server: passkeys are off
------------------------------------
LOG

expect "carriage returns" "$token" <<LOG
----- pyn one-time setup token -----$(printf '\r')
$token$(printf '\r')
LOG

expect "no token" "" <<'LOG'
----- pyn one-time setup token -----
2026-11-20T22:02:09.655239Z  INFO pyn_server: listening
------------------------------------
LOG

echo "test-smoke-token: ok"
