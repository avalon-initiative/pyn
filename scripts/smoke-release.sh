#!/usr/bin/env bash
# Smoke-tests a published release: pulls the images, runs deploy/docker-compose.yml, checks the server and web app,
# completes first-run setup, then does the same over https through the optional caddy service with an internal certificate
# (redirect, exported root, forwarded protocol across both proxies, the certificate's expiry). Usage: scripts/smoke-release.sh <server-version> [<web-version>] (for example 0.1.0 0.1.3).
# The web version defaults to the newest pyn-web release, pre-releases included, since the two are released independently. Needs docker compose, curl, openssl. Set SMOKE_SKIP_TLS=1 to run only the plain pass.
set -euo pipefail

# The first bare-token line under the setup token header; startup log lines may be interleaved in between.
setup_token_from_log() {
  awk '/----- pyn one-time setup token -----/ {found=1; next} found && /^[[:space:]]*[A-Za-z0-9]+[[:space:]]*$/ {gsub(/[[:space:]]/, ""); if (length($0) >= 16) {print; exit}}'
}
if [[ ${1:-} == --setup-token-from-log ]]; then setup_token_from_log; exit 0; fi

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
version=${1:-}
web_version=${2:-}
semver='^v?[0-9]+\.[0-9]+\.[0-9]+$'
if [[ ! $version =~ $semver ]] || { [[ -n $web_version ]] && [[ ! $web_version =~ $semver ]]; }; then
  echo "usage: $0 <server-version> [<web-version>]   (X.Y.Z, without a title suffix; the web version defaults to the newest pyn-web release, pre-releases included)" >&2
  exit 2
fi
version=${version#v}
web_version=${web_version#v}

for tool in docker curl openssl awk; do
  command -v "$tool" >/dev/null || { echo "smoke: $tool is required" >&2; exit 2; }
done
docker compose version >/dev/null 2>&1 || { echo "smoke: docker compose (v2 plugin) is required" >&2; exit 2; }

pull_attempts=${SMOKE_PULL_ATTEMPTS:-20}
pull_wait=${SMOKE_PULL_WAIT:-30}
ready_attempts=${SMOKE_READY_ATTEMPTS:-60}

work=$(mktemp -d)
project="pyn-smoke-$$"
pdir="$work"
started=0
ok=0
compose() { docker compose -p "$project" --project-directory "$pdir" -f "$pdir/docker-compose.yml" "$@"; }

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

latest_web_version() {
  local tag
  tag=$(curl --silent --fail --location --max-time 20 https://api.github.com/repos/avalon-initiative/pyn-web/releases?per_page=1 |
    sed -n 's/.*"tag_name": *"v\{0,1\}\([0-9]*\.[0-9]*\.[0-9]*\)[^"]*".*/\1/p' | head -n 1)
  [[ -n $tag ]] || fail "could not find the newest pyn-web release, pre-releases included; pass the web version as the second argument"
  echo "$tag"
}
[[ -n $web_version ]] || web_version=$(latest_web_version)

free_port() {
  local port
  for _ in $(seq 1 50); do
    port=$((20000 + RANDOM % 20000))
    if ! (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then echo "$port"; return; fi
  done
  fail "no free local port found"
}

write_env() { # write_env extra-lines...
  cat >"$pdir/.env" <<ENV
PYN_SERVER_VERSION=v$version
PYN_WEB_VERSION=v$web_version
POSTGRES_PASSWORD=$(openssl rand -hex 24)
PYN_SECRET_KEY=$(openssl rand -hex 32)
ENV
  printf '%s\n' "$@" >>"$pdir/.env"
}

# The web image comes from pyn-web's own tag workflow, which can finish later than this repo's.
pull_images() {
  step "pulling pyn-core v$version and pyn-web v$web_version (up to $pull_attempts attempts, ${pull_wait}s apart)"
  local pulled=0 attempt
  for attempt in $(seq 1 "$pull_attempts"); do
    if compose pull --quiet 2>"$work/pull.err"; then pulled=1; break; fi
    echo "smoke: pull attempt $attempt/$pull_attempts failed: $(tail -n 1 "$work/pull.err")" >&2
    [[ $attempt -lt $pull_attempts ]] && sleep "$pull_wait"
  done
  [[ $pulled -eq 1 ]] || fail "could not pull pyn-core v$version and pyn-web v$web_version (are both images published, and are both packages public?)"
}

wait_for() { # wait_for NAME URL [curl options...]
  local name=$1 url=$2
  shift 2
  for _ in $(seq 1 "$ready_attempts"); do
    if curl --silent --fail --max-time 5 --output /dev/null "$@" "$url"; then step "$name answers at $url"; return; fi
    sleep 2
  done
  fail "$name did not answer at $url"
}

# complete_setup SETUP_URL PUBLIC_URL [curl options...]: reads the one-time token from the server log and finishes first-run setup
complete_setup() {
  local setup_url=$1 public=$2 server_log token body code
  shift 2
  local status
  status=$(curl --silent --fail --max-time 10 "$@" "$setup_url") || fail "GET $setup_url failed"
  [[ $status == *'"initialised":false'* ]] || fail "a fresh server should report initialised=false, got: $status"
  server_log=$(compose logs --no-log-prefix --no-color server 2>&1) || fail "could not read the server log"
  token=$(setup_token_from_log <<<"$server_log")
  [[ -n $token ]] || fail "the setup token is not in the server log"
  step "completing first-run setup"
  admin_password=$(openssl rand -hex 16)
  body=$(printf '{"token":"%s","username":"smoke-admin","password":"%s","registration":"closed","public_url":"%s"}' "$token" "$admin_password" "$public")
  code=$(curl --silent --output "$work/setup.out" --write-out '%{http_code}' --max-time 30 "$@" \
    --header 'Content-Type: application/json' --data "$body" "$setup_url") || fail "POST $setup_url failed"
  [[ $code == 201 ]] || fail "POST $setup_url returned $code: $(cat "$work/setup.out")"
  status=$(curl --silent --fail --max-time 10 "$@" "$setup_url") || fail "GET $setup_url failed after setup"
  [[ $status == *'"initialised":true'* ]] || fail "the server does not report initialised=true after setup, got: $status"
}

# Moves to a fresh stack in its own folder; the previous one is taken down with its volumes
new_stack() { # new_stack NAME
  if [[ $started -eq 1 ]]; then compose down --volumes --remove-orphans >&2 || true; started=0; fi
  project="pyn-smoke-$$-$1"
  pdir="$work/$1"
  mkdir -p "$pdir"
  cp "$root/deploy/docker-compose.yml" "$pdir/docker-compose.yml"
}

plain_pass() {
  local web_port server_addr server_url web_url
  new_stack plain
  web_port=$(free_port)
  # No caddy profile: this is the stack an existing install runs
  write_env "PYN_PUBLIC_URL=http://127.0.0.1:$web_port" "PYN_WEB_PUBLISH=127.0.0.1:$web_port"
  pull_images

  step "starting the stack (plain HTTP, web on 127.0.0.1:$web_port)"
  started=1
  compose up --detach --wait --wait-timeout 180 || fail "docker compose up did not become ready"

  server_addr=$(compose port server 7878) || fail "the server port is not published"
  server_url="http://127.0.0.1:${server_addr##*:}"
  web_url="http://127.0.0.1:$web_port"
  wait_for "server /healthz" "$server_url/healthz"
  wait_for "web app" "$web_url/"
  complete_setup "$server_url/v1/setup" "$web_url"
  step "plain HTTP pass OK"
}

# Warns loudly, on stderr and as a workflow annotation, when the certificate has less life left than a healthy renewal leaves
check_expiry() { # check_expiry CERT_FILE
  local cert=$1 not_before not_after start end now life left floor=$((7 * 86400)) days
  not_before=$(openssl x509 -in "$cert" -noout -startdate | cut -d= -f2)
  not_after=$(openssl x509 -in "$cert" -noout -enddate | cut -d= -f2)
  step "certificate valid from $not_before until $not_after"
  openssl x509 -in "$cert" -noout -checkend 0 >/dev/null || fail "the served certificate has expired ($not_after): renewal is not working"
  now=$(date -u +%s)
  if start=$(date -u -d "$not_before" +%s 2>/dev/null) && end=$(date -u -d "$not_after" +%s 2>/dev/null); then
    life=$((end - start))
    left=$((end - now))
    # Certificates are renewed with a third of their life left; short-lived ones (the local authority's last 12 hours) get a proportional floor
    [[ $((life / 10)) -ge $floor ]] || floor=$((life / 10))
    days=$((left / 86400))
    if [[ $left -lt $floor ]]; then
      {
        echo "smoke: !!!!!!!!!! WARNING !!!!!!!!!!"
        echo "smoke: the served certificate has only $((left / 3600)) hours left (${days} days), under the floor of $((floor / 3600)) hours: RENEWAL IS NOT HAPPENING"
        echo "::warning::caddy certificate close to expiry ($not_after); automatic renewal may be failing"
      } >&2
      return 0
    fi
    step "certificate has $((left / 3600)) hours left of a $((life / 3600))-hour lifetime"
  elif ! openssl x509 -in "$cert" -noout -checkend "$floor" >/dev/null; then
    {
      echo "smoke: !!!!!!!!!! WARNING !!!!!!!!!!"
      echo "smoke: the served certificate expires within 7 days ($not_after): RENEWAL IS NOT HAPPENING"
      echo "::warning::caddy certificate close to expiry ($not_after); automatic renewal may be failing"
    } >&2
  fi
}

tls_pass() {
  local web_port http_port tls_port web_url https_url redirect root_crt cookie_file cert_file
  new_stack tls
  web_port=$(free_port)
  http_port=$(free_port)
  tls_port=$(free_port)
  while [[ $http_port == "$web_port" ]]; do http_port=$(free_port); done
  while [[ $tls_port == "$web_port" || $tls_port == "$http_port" ]]; do tls_port=$(free_port); done
  web_url="http://127.0.0.1:$http_port"
  https_url="https://127.0.0.1:$tls_port"
  # The public address is deliberately http: the server then marks its session cookie Secure only when the request reaches it
  # with X-Forwarded-Proto: https from caddy, kept by the web container, which is what this pass checks.
  printf '%s\n' '{' $'\tskip_install_trust' $'\tdefault_sni 127.0.0.1' '}' '' '127.0.0.1 {' $'\ttls internal' \
    $'\treverse_proxy web:8080 {' $'\t\tflush_interval -1' $'\t}' '}' >"$pdir/Caddyfile"
  write_env "COMPOSE_PROFILES=caddy" "PYN_SITE=127.0.0.1" "PYN_TRUSTED_PROXIES=2" "PYN_PUBLIC_URL=http://127.0.0.1:$tls_port" \
    "PYN_WEB_PUBLISH=127.0.0.1:$web_port" "PYN_WEB_PUBLISH_HTTP=127.0.0.1:$http_port" "PYN_WEB_PUBLISH_TLS=127.0.0.1:$tls_port"

  step "starting the stack (caddy with an internal certificate, http on 127.0.0.1:$http_port, https on 127.0.0.1:$tls_port)"
  started=1
  compose up --detach --wait --wait-timeout 180 || fail "docker compose up did not become ready (TLS)"

  root_crt="$pdir/root.crt"
  for _ in $(seq 1 "$ready_attempts"); do
    if compose cp caddy:/data/caddy/pki/authorities/local/root.crt "$root_crt" 2>/dev/null && [[ -s $root_crt ]]; then break; fi
    sleep 2
  done
  [[ -s $root_crt ]] || fail "the internal root certificate was not created in the caddy container"
  openssl x509 -in "$root_crt" -noout >/dev/null || fail "the exported root is not a certificate"
  step "fetched the internal root certificate from the container"

  wait_for "web app over https" "$https_url/" --cacert "$root_crt"
  # The redirect omits the port by design (docs/deployment.md), so only the scheme, host and status are compared
  redirect=$(curl --silent --output /dev/null --write-out '%{http_code} %{redirect_url}' --max-time 10 "$web_url/some/route") || fail "http request failed"
  [[ $redirect == "308 https://127.0.0.1/some/route" ]] || fail "http should redirect with 308 to https, got: $redirect"
  step "http redirects to https (308)"
  curl --silent --fail --max-time 10 --cacert "$root_crt" "$https_url/healthz" >/dev/null || fail "the server's /healthz through caddy and the web container over https failed"

  complete_setup "$https_url/v1/setup" "http://127.0.0.1:$tls_port" --cacert "$root_crt"
  cookie_file="$pdir/signin.headers"
  curl --silent --fail --max-time 10 --cacert "$root_crt" --dump-header "$cookie_file" --output /dev/null \
    --header 'Content-Type: application/json' --data "{\"username\":\"smoke-admin\",\"password\":\"$admin_password\"}" \
    "$https_url/v1/session" || fail "signing in over https through caddy and the web container failed"
  grep -qi '^set-cookie:.*; Secure' "$cookie_file" || fail "the server did not see X-Forwarded-Proto: https across caddy and the web container (its session cookie is not Secure)"
  step "signed in over https; the server saw X-Forwarded-Proto: https"

  cert_file="$pdir/served.crt"
  openssl s_client -connect "127.0.0.1:$tls_port" -servername 127.0.0.1 -CAfile "$root_crt" -verify_return_error </dev/null 2>"$pdir/s_client.err" |
    openssl x509 >"$cert_file" || fail "could not read the served certificate: $(tail -n 3 "$pdir/s_client.err")"
  check_expiry "$cert_file"
  step "TLS pass OK"
}

plain_pass
if [[ ${SMOKE_SKIP_TLS:-} != 1 ]]; then tls_pass; fi

ok=1
if [[ ${SMOKE_SKIP_TLS:-} == 1 ]]; then
  step "OK: pyn-core v$version with pyn-web v$web_version start, answer, and complete setup over plain HTTP"
else
  step "OK: pyn-core v$version with pyn-web v$web_version start, answer, and complete setup over plain HTTP and over https through caddy (internal certificate)"
fi
