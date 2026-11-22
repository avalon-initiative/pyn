#!/usr/bin/env bash
# Tests check-env.sh against crafted deployment folders and a fake docker on PATH. No Docker, no network.
set -euo pipefail

src=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fails=0
pass() { echo "ok   $1"; }
fail() { echo "FAIL $1" >&2; fails=$((fails + 1)); }
expect() { # expect <name> <condition-command...>
  local name=$1; shift
  if "$@" >/dev/null 2>&1; then pass "$name"; else fail "$name"; fi
}
has() { grep -qF -- "$2" "$1"; }
lacks() { ! grep -qF -- "$2" "$1"; }
ok_rc() { [ "$rc" -eq 0 ]; }
bad_rc() { [ "$rc" -eq 1 ]; }

fake="$work/fake"
mkdir -p "$fake"
cat >"$fake/docker" <<'STUB'
#!/bin/bash
echo "[$PWD] docker $*" >>"$LOG"
case "$*" in
  "compose version") exit 0 ;;
  "compose ps"*) exit 0 ;;
  "compose run"*)
    [ -z "${FAKE_SLEEP:-}" ] || sleep "$FAKE_SLEEP"
    [ -z "${FAKE_OUT:-}" ] || printf '%b\n' "$FAKE_OUT"
    exit "${FAKE_RC:-0}" ;;
esac
exit 0
STUB
cat >"$fake/timeout" <<'STUB'
#!/bin/bash
echo "timeout $1" >>"$LOG"
exec "$REAL_TIMEOUT" "$@"
STUB
chmod +x "$fake/docker" "$fake/timeout"

basics="bash sed grep tail tr awk sort mktemp stat cat head rm dirname ls date env sleep"
mkbin() { # mkbin DIR [with-docker]
  local b=$1 t
  mkdir -p "$b"
  for t in $basics; do ln -sf "$(command -v "$t")" "$b/$t"; done
  [ "${2:-}" != docker ] || cp "$fake/docker" "$fake/timeout" "$b/"
}
mkbin "$work/bin-docker" docker
mkbin "$work/bin-plain"

deploy="$src/deploy"
good=(PYN_SERVER_VERSION=v0.1.5 PYN_WEB_VERSION=v0.1.5 POSTGRES_PASSWORD=pw PYN_SECRET_KEY=sk PYN_PUBLIC_URL=http://localhost:8080)
extra_env=()
bin=$work/bin-docker
out=$work/out
log=$work/log

# newdir NAME COMPOSE-FILE [env lines...]: a deployment folder in $d with mode 600 .env
newdir() {
  d=$work/$1; local f=$2; shift 2
  rm -rf "$d"; mkdir -p "$d"
  cp "$deploy/$f" "$d/docker-compose.yml"
  printf '%s\n' "$@" >"$d/.env"
  chmod 600 "$d/.env"
  : >"$log"
}
run() { # run [options...]: checks $d
  rc=0
  env -i PATH="$bin" HOME="$work" LOG="$log" REAL_TIMEOUT="$(command -v timeout || echo /bin/false)" ${extra_env[@]+"${extra_env[@]}"} \
    "$src/scripts/check-env.sh" --no-color "$@" "$d" >"$out" 2>&1 || rc=$?
}
reset() { extra_env=(); bin=$work/bin-docker; }
free_port() { python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'; }

# A clean install and the version gate
reset
newdir clean docker-compose.yml "${good[@]}"
run
expect "a clean .env passes" ok_rc
expect "a clean .env says ok" has "$out" "pyn-check: ok"
expect "an older server is not asked for --check-config" lacks "$log" "compose run"
expect "and one line says why" has "$out" "needs a release with --check-config (from 0.2.0)"
newdir clean2 docker-compose.yml "${good[@]}" PYN_SERVER_VERSION=v0.1.99
run
expect "v0.1.99 is still older than the first release with the flag" lacks "$log" "compose run"
newdir clean3 docker-compose.yml "${good[@]}" PYN_SERVER_VERSION=v0.9.0
run
expect "a newer release runs the server check" has "$log" "docker compose run --rm --no-deps -T server pyn-server --check-config"
newdir clean4 docker-compose.yml "${good[@]}" PYN_SERVER_VERSION=v0.10.0
run
expect "v0.10.0 counts as newer than v0.2.0, not older" has "$log" "pyn-server --check-config"

# The server check call
reset
newdir srv docker-compose.yml "${good[@]}" PYN_SERVER_VERSION=v0.2.0
extra_env=(FAKE_OUT="configuration ok")
run
expect "the server check passes through" ok_rc
expect "it runs in the deployment folder" has "$log" "[$d] docker compose run --rm --no-deps -T server pyn-server --check-config"
expect "it is wrapped in timeout 60" has "$log" "timeout 60"
expect "its output is shown" has "$out" "configuration ok"
expect "ok is reported" has "$out" "the server accepts this configuration"
extra_env=(FAKE_RC=1 FAKE_OUT="PYN_S3_PATH_STYLE must be true or false\n1 problem(s) found")
run
expect "exit 1 from the server check is a problem" bad_rc
expect "its problem lines are shown" has "$out" "PYN_S3_PATH_STYLE must be true or false"
expect "and the user is told where to change them" has "$out" "change those PYN_* lines"
extra_env=(FAKE_RC=125 FAKE_OUT="image pull failed")
run
expect "a failure to run the check is a warning, not a problem" ok_rc
expect "and says the check could not run" has "$out" "could not run (exit 125"
run --strict
expect "--strict fails on that warning" bad_rc
extra_env=(FAKE_OUT="warning: PYN_FOO is not a setting this server reads")
run
expect "a warning from the server check passes" ok_rc
run --strict
expect "and counts for --strict" bad_rc
extra_env=(PYN_INSTALL_CHECK_TIMEOUT=1 FAKE_SLEEP=5)
run
expect "a hung server check times out as a problem" bad_rc
expect "the timeout is named" has "$out" "did not finish in 1s"
extra_env=(FAKE_OUT="configuration ok")
: >"$log"
run --no-server
expect "--no-server skips the call" lacks "$log" "compose run"
bin=$work/bin-plain
run
expect "without Docker the server check is skipped, not failed" ok_rc
expect "and says so" has "$out" "docker compose was not found, so the server's own check was skipped"
newdir srvprob docker-compose.yml "${good[@]}" PYN_SERVER_VERSION=v0.2.0 POSTGRES_PASSWORD=
bin=$work/bin-docker
run
expect "the server check waits for the file problems to be fixed" lacks "$log" "compose run"

# The file
reset
newdir noenv docker-compose.yml
rm "$d/.env"
run
expect "a missing .env is a problem" bad_rc
expect "and says what to copy" has "$out" ".env does not exist: copy deploy/.env.example"
newdir perm docker-compose.yml "${good[@]}"
chmod 644 "$d/.env"
run
expect "a world-readable .env is only a warning" ok_rc
expect "with the chmod command" has "$out" "run chmod 600 $d/.env"
run --strict
expect "--strict fails on it" bad_rc
chmod 640 "$d/.env"
run
expect "a group-readable .env is warned about too" has "$out" "readable by others (mode 640)"
chmod 400 "$d/.env"
run
expect "an owner-only .env is not" lacks "$out" "readable by others"
newdir nocompose docker-compose.yml "${good[@]}"
rm "$d/docker-compose.yml"
run
expect "a missing Compose file is a problem" bad_rc
expect "and says what to do" has "$out" "no docker-compose.yml next to"

# Required keys, per shape
for k in POSTGRES_PASSWORD PYN_SECRET_KEY PYN_SERVER_VERSION PYN_WEB_VERSION PYN_PUBLIC_URL; do
  newdir "req_$k" docker-compose.yml "${good[@]}" "$k="
  run
  expect "full: an empty $k is a problem" bad_rc
  expect "full: $k is named" has "$out" "$k is missing or empty"
done
newdir req_none docker-compose.yml PYN_SERVER_VERSION=v0.1.5
run
expect "a missing key is also caught" has "$out" "PYN_SECRET_KEY is missing or empty"
newdir srv_only docker-compose.server.yml PYN_SERVER_VERSION=v0.1.5 POSTGRES_PASSWORD=pw PYN_SECRET_KEY=sk PYN_PUBLIC_URL=http://localhost:7878
run
expect "server only: no web version is needed" ok_rc
newdir srv_only2 docker-compose.server.yml PYN_SERVER_VERSION=v0.1.5 POSTGRES_PASSWORD=pw PYN_SECRET_KEY=sk
run
expect "server only: the public address is needed" has "$out" "PYN_PUBLIC_URL is missing or empty"
newdir web_only docker-compose.web.yml PYN_WEB_VERSION=v0.1.5 PYN_API_URL=https://pyn.example.com
run
expect "web only: no database password or secret key is needed" ok_rc
newdir web_only2 docker-compose.web.yml PYN_WEB_VERSION=v0.1.5
run
expect "web only: PYN_API_URL is needed" has "$out" "PYN_API_URL is missing or empty"
expect "web only: PYN_SECRET_KEY is not asked for" lacks "$out" "PYN_SECRET_KEY"
newdir web_shape docker-compose.yml PYN_WEB_VERSION=v0.1.5 PYN_API_URL=https://pyn.example.com
run --shape web
expect "--shape overrides what the Compose file says" ok_rc

# Version tags
for v in 0.2.0 v0.2.0-alpha latest; do
  newdir "ver_$v" docker-compose.yml "${good[@]}" PYN_SERVER_VERSION=$v
  run
  expect "version $v is a problem" bad_rc
done
newdir ver_a docker-compose.yml "${good[@]}" PYN_SERVER_VERSION=0.2.0
run
expect "a bare version gets the v" has "$out" "set PYN_SERVER_VERSION=v0.2.0"
newdir ver_b docker-compose.yml "${good[@]}" PYN_WEB_VERSION=v0.2.0-alpha
run
expect "a title suffix is explained" has "$out" "set PYN_WEB_VERSION=v0.2.0"
newdir ver_c docker-compose.yml "${good[@]}" PYN_SERVER_VERSION=latest
run
expect "a non-version names the form" has "$out" "set it to v followed by the version"
newdir ver_d docker-compose.yml "${good[@]}" PYN_SERVER_VERSION=\"v0.1.5\"
run
expect "quotes around a good tag are fine" ok_rc

# Renamed and removed keys
newdir ren docker-compose.yml "${good[@]}" PYN_VERSION=v0.1.5
run
expect "PYN_VERSION is a problem" bad_rc
expect "and says it became two keys" has "$out" "became PYN_SERVER_VERSION"
newdir ren2 docker-compose.yml "${good[@]}" PYN_IMAGE=ghcr.io/x/y:v1 PYN_WEB_IMAGE=ghcr.io/x/z:v1
run
expect "PYN_IMAGE is renamed" has "$out" "PYN_IMAGE is no longer read: set PYN_SERVER_VERSION"
expect "PYN_WEB_IMAGE is renamed" has "$out" "PYN_WEB_IMAGE is no longer read: set PYN_SERVER_VERSION (server) and PYN_WEB_VERSION"
expect "a renamed key is not also reported as unknown" lacks "$out" "is not a setting pyn reads"
for pair in "PYN_TLS=auto|PYN_TLS was never part of a release" \
  "PYN_TRUSTED_PROXY_RANGES=10.0.0.0/8|PYN_TRUSTED_PROXY_RANGES was removed" \
  "PYN_CF_TOKEN_FILE=/x|PYN_CF_TOKEN_FILE was removed" \
  "PYN_EMAIL=a@b.c|PYN_EMAIL is not a setting"; do
  newdir rem docker-compose.yml "${good[@]}" "${pair%%|*}"
  run
  expect "${pair%%=*}: a warning that says what to do" has "$out" "${pair#*|}"
  expect "${pair%%=*}: warnings alone pass" ok_rc
done
newdir stale docker-compose.yml "${good[@]}"
sed -i 's/PYN_SERVER_VERSION/PYN_VERSION/' "$d/docker-compose.yml"
run
expect "a Compose file that still reads PYN_VERSION is stale" has "$out" "is from before PYN_SERVER_VERSION and PYN_WEB_VERSION"
expect "and says how to refresh it" has "$out" "install.sh --reuse refresh"

# Caddy
caddy_env=(PYN_SERVER_VERSION=v0.1.5 PYN_WEB_VERSION=v0.1.5 POSTGRES_PASSWORD=pw PYN_SECRET_KEY=sk COMPOSE_PROFILES=caddy PYN_SITE=pyn.example.com PYN_TRUSTED_PROXIES=2)
newdir cad docker-compose.yml "${caddy_env[@]}"
printf 'pyn.example.com {\n}\n' >"$d/Caddyfile"
run --no-ports
expect "the installer's Caddy .env (no public address, no trust key) passes" ok_rc
newdir cad1 docker-compose.yml "${caddy_env[@]}"
run --no-ports
expect "Caddy on without a Caddyfile is a problem" has "$out" "needs $d/Caddyfile next to the Compose file"
newdir cad2 docker-compose.yml PYN_SERVER_VERSION=v0.1.5 PYN_WEB_VERSION=v0.1.5 POSTGRES_PASSWORD=pw PYN_SECRET_KEY=sk COMPOSE_PROFILES=caddy PYN_TRUSTED_PROXIES=2 PYN_PUBLIC_URL=https://x.example.com
touch "$d/Caddyfile"
run --no-ports
expect "Caddy on without PYN_SITE is a problem" has "$out" "needs PYN_SITE: set PYN_SITE"
newdir cad3 docker-compose.yml "${caddy_env[@]}" PYN_SITE=https://pyn.example.com
touch "$d/Caddyfile"
run --no-ports
expect "a PYN_SITE with a scheme is a problem" has "$out" "must be a bare host name or address"
newdir cad4 docker-compose.yml "${caddy_env[@]}"
touch "$d/Caddyfile"
sed -i '/^  caddy:/,/^volumes:/d' "$d/docker-compose.yml"
run --no-ports
expect "a Compose file without the caddy service is a problem" has "$out" "has no caddy service with a persistent caddy-data volume"
newdir cad5 docker-compose.server.yml "${caddy_env[@]}" PYN_PUBLIC_URL=http://h:7878
touch "$d/Caddyfile"
run --no-ports
expect "the profile on a server-only file is a problem" has "$out" "server-only Compose file has no caddy service"
newdir cad6 docker-compose.yml "${caddy_env[@]}" PYN_PUBLIC_URL=http://pyn.example.com
touch "$d/Caddyfile"
run --no-ports
expect "an http public address with Caddy on is a problem" bad_rc
expect "and says what to set" has "$out" "PYN_PUBLIC_URL=http://pyn.example.com is wrong with Caddy on (browsers arrive over https): set PYN_PUBLIC_URL=https://pyn.example.com"
newdir cad7 docker-compose.yml "${caddy_env[@]}" PYN_PUBLIC_URL=https://pyn.example.com
touch "$d/Caddyfile"
printf 'pyn.example.com {\n}\n' >"$d/Caddyfile"
run --no-ports
expect "an explicit https public address matching the site passes" ok_rc
newdir cad8 docker-compose.yml "${caddy_env[@]}" PYN_PUBLIC_URL=https://other.example.com
printf 'pyn.example.com {\n}\n' >"$d/Caddyfile"
run --no-ports
expect "a different https host is only a warning" has "$out" "differs from PYN_SITE=pyn.example.com"
newdir cad9 docker-compose.yml "${caddy_env[@]}" PYN_TRUSTED_PROXIES=1
printf 'pyn.example.com {\n}\n' >"$d/Caddyfile"
run --no-ports
expect "PYN_TRUSTED_PROXIES=1 with Caddy on is a problem" bad_rc
expect "and says to set 2" has "$out" "PYN_TRUSTED_PROXIES=1 is wrong with Caddy on: set PYN_TRUSTED_PROXIES=2"
newdir cad10 docker-compose.yml PYN_SERVER_VERSION=v0.1.5 PYN_WEB_VERSION=v0.1.5 POSTGRES_PASSWORD=pw PYN_SECRET_KEY=sk COMPOSE_PROFILES=caddy PYN_SITE=pyn.example.com
printf 'pyn.example.com {\n}\n' >"$d/Caddyfile"
run --no-ports
expect "Caddy on with no PYN_TRUSTED_PROXIES is a problem" has "$out" "PYN_TRUSTED_PROXIES is not set with Caddy on: set PYN_TRUSTED_PROXIES=2"
newdir cad11 docker-compose.yml "${caddy_env[@]}"
printf 'other.example.com {\n}\n' >"$d/Caddyfile"
run --no-ports
expect "a Caddyfile for another name is a warning" has "$out" "does not mention PYN_SITE=pyn.example.com"
newdir cad12 docker-compose.web.yml PYN_WEB_VERSION=v0.1.5 PYN_API_URL=https://pyn.example.com COMPOSE_PROFILES=caddy PYN_SITE=pyn.example.com
printf 'pyn.example.com {\n}\n' >"$d/Caddyfile"
run --no-ports
expect "web only with Caddy needs no proxy count and no public address" ok_rc

# Proxies and the public address without Caddy
newdir prox docker-compose.yml "${good[@]}" PYN_TRUSTED_PROXIES=2 PYN_TRUST_FORWARDED_FOR=false
run
expect "PYN_TRUSTED_PROXIES without PYN_TRUST_FORWARDED_FOR=true is a problem" bad_rc
expect "and says the server refuses it" has "$out" "PYN_TRUSTED_PROXIES is set but PYN_TRUST_FORWARDED_FOR=false: the server refuses that"
newdir prox2 docker-compose.yml "${good[@]}" PYN_TRUSTED_PROXIES=2 PYN_TRUST_FORWARDED_FOR=true
run
expect "PYN_TRUSTED_PROXIES with trust on passes" ok_rc
newdir prox3 docker-compose.yml "${good[@]}" PYN_TRUSTED_PROXIES=abc
run
expect "a non-number proxy count is a problem" has "$out" "must be a whole number from 1 to 16"
newdir prox4 docker-compose.web.yml PYN_WEB_VERSION=v0.1.5 PYN_API_URL=https://pyn.example.com PYN_TRUSTED_PROXIES=2
run
expect "a proxy count in a web-only .env is a warning" has "$out" "are server settings and do nothing in a web-only .env"
newdir tls docker-compose.yml PYN_SERVER_VERSION=v0.1.5 PYN_WEB_VERSION=v0.1.5 POSTGRES_PASSWORD=pw PYN_SECRET_KEY=sk PYN_PUBLIC_URL=https://pyn.example.com
run
expect "an https address without Caddy is only a warning" ok_rc
expect "and mentions a proxy elsewhere" has "$out" "fine behind a TLS proxy of your own"
newdir url docker-compose.yml "${good[@]}" PYN_PUBLIC_URL=http://localhost:8080/app
run
expect "a public address with a path is a warning" has "$out" "should look like http://host:port or https://host with no path"
newdir siteonly docker-compose.yml PYN_SERVER_VERSION=v0.1.5 PYN_WEB_VERSION=v0.1.5 POSTGRES_PASSWORD=pw PYN_SECRET_KEY=sk PYN_SITE=pyn.example.com
run
expect "PYN_SITE alone supplies the public address" lacks "$out" "PYN_PUBLIC_URL is missing"

# Unknown keys
newdir unk docker-compose.yml "${good[@]}" PYN_PUBLC_URL=x PYN_TOTALLY_MADE_UP=1 PYN_INSTALL_PROBE_PORTS=1 PYN_SMTP_HOST=h
run
expect "a misspelt key gets a suggestion" has "$out" "PYN_PUBLC_URL is not a setting pyn reads: did you mean PYN_PUBLIC_URL?"
expect "a stray key says to delete it" has "$out" "PYN_TOTALLY_MADE_UP is not a setting pyn reads: delete it"
expect "the installer's test variables are never reported" lacks "$out" "PYN_INSTALL_PROBE_PORTS is not"
expect "a real setting is not reported" lacks "$out" "PYN_SMTP_HOST is not"
expect "unknown keys are warnings" ok_rc
newdir unk2 docker-compose.yml "${good[@]}" "# PYN_COMMENTED=1" "OTHER_THING=1"
run
expect "comments and non-PYN keys are ignored" lacks "$out" "is not a setting"

# Ports
reset
p1=$(free_port)
python3 -c "
import socket, sys, time
s = socket.socket(); s.bind(('127.0.0.1', $p1)); s.listen(1); time.sleep(30)" &
listener=$!
sleep 1
newdir ports docker-compose.yml "${caddy_env[@]}"
printf 'pyn.example.com {\n}\n' >"$d/Caddyfile"
extra_env=(PYN_INSTALL_PROBE_PORTS="$p1")
run
expect "a busy port with Caddy on is a warning" has "$out" "port $p1 already in use on this host"
expect "and passes without --strict" ok_rc
run --strict
expect "--strict fails on it" bad_rc
run --no-ports
expect "--no-ports does not probe" lacks "$out" "already in use"
newdir ports2 docker-compose.yml "${good[@]}"
run
expect "without Caddy ports are not probed" lacks "$out" "already in use"
kill "$listener" 2>/dev/null || true
wait "$listener" 2>/dev/null || true
newdir ports3 docker-compose.yml "${caddy_env[@]}"
printf 'pyn.example.com {\n}\n' >"$d/Caddyfile"
extra_env=(PYN_INSTALL_PROBE_PORTS="$(free_port)")
run
expect "free ports raise nothing" lacks "$out" "already in use"

# The known list stays complete
reset
bash "$src/scripts/check-env.sh" --known | sort >"$work/known"
docs_missing=$(grep -o '^| `PYN_[A-Z0-9_]*`' "$src/docs/configuration.md" | tr -d '|` ' | sort -u | comm -23 - "$work/known" | tr '\n' ' ')
expect "every variable in the docs/configuration.md table is accepted by the script (missing: ${docs_missing:-none})" test -z "$docs_missing"
rs="$src/crates/pyn-server/src/known_settings.rs"
rust_names=$(awk '/pub const (KNOWN|OTHER_COMPONENTS)/{f=1;next} /^\];/{f=0} f&&/"PYN_/{gsub(/[ ",]/,"");print}' "$rs" | sort -u)
rust_missing=$(echo "$rust_names" | comm -23 - "$work/known" | grep -v '^PYN_IMAGE$\|^PYN_WEB_IMAGE$' | tr '\n' ' ' || true)
expect "every name the server knows is accepted by the script (missing: ${rust_missing:-none})" test -z "$rust_missing"
extra_names=$(comm -13 <(echo "$rust_names") "$work/known" | tr '\n' ' ')
expect "the script accepts nothing the server does not know (extra: ${extra_names:-none})" test -z "$extra_names"
expect "the list is not empty" test "$(wc -l <"$work/known")" -gt 80

# Usage
run --bogus
expect "an unknown option fails with a usage error" test "$rc" -eq 2
expect "--help prints usage" bash -c '"$0" --help | grep -q "Usage: check-env.sh"' "$src/scripts/check-env.sh"

bash -n "$src/scripts/check-env.sh" && pass "check-env.sh parses" || fail "check-env.sh parses"
if [ "$fails" -ne 0 ]; then echo "$fails test(s) failed" >&2; exit 1; fi
echo "check-env tests passed"
