#!/usr/bin/env bash
# Checks a Docker deployment folder (.env and the Compose file next to it) before an upgrade or `docker compose up`.
# Prints every problem and warning with what to change; the server's own `pyn-server --check-config` is authoritative for PYN_* values.
set -euo pipefail

CHECK_CONFIG_SINCE=0.2.0
SERVER_CHECK_TIMEOUT=${PYN_INSTALL_CHECK_TIMEOUT:-60}

dir="" shape_in="" strict=0 no_ports=0 no_server=0 color_flag=""
problems=0 warnings=0
c_bold="" c_dim="" c_red="" c_yellow="" c_green="" c_reset=""

usage() {
  cat <<'EOF'
Usage: check-env.sh [options] [DIR]

Reads DIR/.env (default: the current folder) and the Compose file next to it, and reports what is wrong or needs a manual
update before the stack is started or upgraded. Exit 0 when there are no problems, 1 when there are (or warnings, with --strict).

  --strict       treat warnings as problems
  --no-ports     do not probe ports 80 and 443 (the installer has already done it)
  --shape full|server|web   which Compose file shape this is (default: read from the Compose file)
  --no-server    do not run the server's own check through Docker Compose
  --color, --no-color   force colour on or off
  --known        print the PYN_* names this script accepts, one per line
  -h, --help     this text

The server's own check (pyn-server --check-config, run with `docker compose run`) needs a release that has it.
EOF
}

# Every PYN_* name the server reads or another component owns (crates/pyn-server/src/known_settings.rs); test-check-env.sh keeps it complete
KNOWN_NAMES="
  PYN_ACCESS
  PYN_ACCESS_LOG
  PYN_ACCESS_LOG_CLIENT
  PYN_ACCOUNT_DELETION_WINDOW_DAYS
  PYN_ADDR
  PYN_ALLOWED_ORIGINS
  PYN_API_URL
  PYN_AUDIT_LOG
  PYN_BODY_IDLE_TIMEOUT_SECS
  PYN_BRANCH_STALE_DAYS
  PYN_CADDY_IMAGE
  PYN_CERT_CHECK_INTERVAL_SECS
  PYN_CLI_CONFIG
  PYN_CONFIG
  PYN_CONFIG_DIR
  PYN_CREATE_DATABASE
  PYN_DATABASE_URL
  PYN_DATA_DIR
  PYN_DEBUG_SUBMODULE_CONFIG
  PYN_DEFAULT_MAX_ORG_MEMBERS
  PYN_DEFAULT_MAX_REPOSITORIES
  PYN_DEFAULT_MAX_STORAGE_BYTES
  PYN_DEV_AUTH
  PYN_DEV_AUTH_ALLOW_NON_LOOPBACK
  PYN_DEV_CLOCK_SPEED
  PYN_DIR
  PYN_DISABLED_ACCOUNT_RETENTION_DAYS
  PYN_EDITOR
  PYN_EMAIL_VERIFICATION
  PYN_INSTANCES
  PYN_LOG_FORMAT
  PYN_MAILER
  PYN_MAILER_LOG_ALLOW_NON_LOOPBACK
  PYN_MAX_CONCURRENT_EXPENSIVE
  PYN_MAX_CONCURRENT_MERGES
  PYN_MAX_FORKS_PER_OWNER
  PYN_MAX_FORKS_PER_REPO
  PYN_MAX_LOCKS_ALLOWED_PER_USER
  PYN_MAX_UNREFERENCED_UPLOAD_BYTES
  PYN_MAX_UPLOADS
  PYN_MAX_UPLOADS_PER_USER
  PYN_NEW_SECRET_KEY
  PYN_OIDC_CLIENT_ID
  PYN_OIDC_CLIENT_SECRET
  PYN_OIDC_CREATE_ACCOUNTS
  PYN_OIDC_ISSUER
  PYN_OIDC_NAME
  PYN_OIDC_REDIRECT_URL
  PYN_OIDC_USERNAME_CLAIM
  PYN_ORG_CREATION
  PYN_ORG_INVITATION_DAYS
  PYN_ORG_MEMBER_ADD
  PYN_PASSWORD_HASHES
  PYN_PASSWORD_SIGN_IN
  PYN_PUBLIC_URL
  PYN_PUBLISH
  PYN_RATE_MAIL_PER_EMAIL
  PYN_RATE_PEOPLE_CLIENT
  PYN_RATE_REGISTER_CLIENT
  PYN_RATE_SERVICE_CREDENTIAL
  PYN_RATE_SIGN_IN_ACCOUNT
  PYN_RATE_SIGN_IN_CLIENT
  PYN_REGISTRATION
  PYN_REPO
  PYN_REQUEST_TIMEOUT_SECS
  PYN_REQUIRE_APPROVAL
  PYN_REVISION
  PYN_S3_ACCESS_KEY_ID
  PYN_S3_ACCESS_KEY_ID_FILE
  PYN_S3_BUCKET
  PYN_S3_ENDPOINT
  PYN_S3_MAX_RETRIES
  PYN_S3_PATH_STYLE
  PYN_S3_PREFIX
  PYN_S3_REGION
  PYN_S3_REQUEST_TIMEOUT_SECS
  PYN_S3_SECRET_ACCESS_KEY
  PYN_S3_SECRET_ACCESS_KEY_FILE
  PYN_S3_SPOOL_DIR
  PYN_S3_SPOOL_THRESHOLD
  PYN_SECRET_KEY
  PYN_SERVER
  PYN_SERVER_PUBLISH
  PYN_SERVER_VERSION
  PYN_SESSION_DAYS
  PYN_SESSION_REMEMBER_DAYS
  PYN_SETUP_TOKEN
  PYN_SHUTDOWN_DRAIN_DELAY_SECS
  PYN_SHUTDOWN_GRACE_SECS
  PYN_SITE
  PYN_SMTP_FROM
  PYN_SMTP_HOST
  PYN_SMTP_PASSWORD
  PYN_SMTP_PORT
  PYN_SMTP_TLS
  PYN_SMTP_USER
  PYN_TOKEN
  PYN_TRUSTED_PROXIES
  PYN_TRUST_FORWARDED_FOR
  PYN_UPDATE_CHECK
  PYN_USER
  PYN_WEBAUTHN_RP_ID
  PYN_WEBHOOKS_ALLOWED_HOSTS
  PYN_WEBHOOKS_ALLOW_HTTP
  PYN_WEBHOOKS_ALLOW_LOOPBACK
  PYN_WEBHOOKS_CONCURRENCY
  PYN_WEBHOOK_WORKER
  PYN_WEB_PUBLISH
  PYN_WEB_PUBLISH_HTTP
  PYN_WEB_PUBLISH_TLS
  PYN_WEB_VERSION
"
KNOWN_PREFIXES="PYN_INSTALL_"

setup_color() {
  local esc=$'\033' on=0
  case $color_flag in
    always) on=1 ;;
    never) ;;
    *) if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != dumb ]; then on=1; fi ;;
  esac
  if [ "$on" -eq 1 ]; then
    c_bold="${esc}[1m" c_dim="${esc}[2m" c_red="${esc}[31m" c_yellow="${esc}[33m" c_green="${esc}[32m" c_reset="${esc}[0m"
  fi
}

say() { echo "${c_dim}pyn-check:${c_reset} $*"; }
problem() { problems=$((problems + 1)); echo "${c_dim}pyn-check:${c_reset} ${c_red}problem: $*${c_reset}"; }
warn() { warnings=$((warnings + 1)); echo "${c_dim}pyn-check:${c_reset} ${c_yellow}warning: $*${c_reset}"; }
die() { echo "pyn-check: $*" >&2; exit 2; }

get_env() { # last active KEY=value, outer quotes and trailing blanks dropped
  local v
  v=$(sed -n "s/^$2=//p" "$1" 2>/dev/null | tail -n1 | tr -d '\r' | sed "s/[[:space:]]*\$//; s/^\"\(.*\)\"\$/\1/; s/^'\(.*\)'\$/\1/" || true)
  printf '%s' "$v"
}

file_mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }

ver_lt() { # ver_lt A B: A is older than B (X.Y.Z)
  local IFS=. i x y a b
  read -ra a <<<"$1"
  read -ra b <<<"$2"
  for i in 0 1 2; do
    x=${a[i]:-0} y=${b[i]:-0}
    [ "$x" -eq "$y" ] || { [ "$x" -lt "$y" ]; return; }
  done
  return 1
}

port_busy() { ( exec 4<>"/dev/tcp/${PYN_INSTALL_PROBE_HOST:-127.0.0.1}/$1" ) 2>/dev/null; }

is_known() {
  local n=$1 p
  case " $(echo $KNOWN_NAMES) " in *" $n "*) return 0 ;; esac
  for p in $KNOWN_PREFIXES; do case $n in "$p"*) return 0 ;; esac; done
  return 1
}

suggest() { # closest known name within two edits, or nothing
  echo $KNOWN_NAMES | tr ' ' '\n' | awk -v w="$1" '
    function lev(a, b,   i, j, la, lb, d, c, m) {
      la = length(a); lb = length(b)
      for (i = 0; i <= la; i++) d[i, 0] = i
      for (j = 0; j <= lb; j++) d[0, j] = j
      for (i = 1; i <= la; i++) for (j = 1; j <= lb; j++) {
        c = (substr(a, i, 1) == substr(b, j, 1)) ? 0 : 1
        m = d[i - 1, j] + 1
        if (d[i, j - 1] + 1 < m) m = d[i, j - 1] + 1
        if (d[i - 1, j - 1] + c < m) m = d[i - 1, j - 1] + c
        d[i, j] = m
      }
      return d[la, lb]
    }
    BEGIN { w = toupper(w); best = 3 }
    { n = lev(w, $0); if (n < best) { best = n; hit = $0 } }
    END { if (best <= 2) print hit }'
}

has_caddy_profile() { case ",$(get_env "$1" COMPOSE_PROFILES)," in *,caddy,*) return 0 ;; *) return 1 ;; esac; }

have_docker() { command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; }

check_perms() {
  local mode
  mode=$(file_mode "$env") || return 0
  case $mode in
    *[1-7][0-7] | *[0-7][1-7]) warn "$env is readable by others (mode $mode) and holds secrets: run chmod 600 $env" ;;
  esac
}

check_renamed() {
  local k
  if grep -q '^PYN_VERSION=' "$env"; then
    problem "PYN_VERSION is no longer read: it became PYN_SERVER_VERSION (the server image tag) and PYN_WEB_VERSION (the web image tag), which are versioned separately; set both to the tags you run and delete PYN_VERSION"
  fi
  for k in PYN_IMAGE PYN_WEB_IMAGE; do
    grep -q "^$k=" "$env" || continue
    [ -f "$compose" ] && grep -q "$k" "$compose" && continue
    problem "$k is no longer read: set PYN_SERVER_VERSION (server) and PYN_WEB_VERSION (web) to the image tag, for example v0.2.0, and delete $k"
  done
  grep -q '^PYN_TLS=' "$env" && warn "PYN_TLS was never part of a release and is ignored: delete it; HTTPS is the Caddy option now (scripts/install.sh --caddy --site NAME, docs/deployment.md#https)"
  grep -q '^PYN_TRUSTED_PROXY_RANGES=' "$env" && warn "PYN_TRUSTED_PROXY_RANGES was removed and is ignored: delete it; the proxy count is PYN_TRUSTED_PROXIES (2 with Caddy)"
  grep -q '^PYN_CF_TOKEN_FILE=' "$env" && warn "PYN_CF_TOKEN_FILE was removed and is ignored: delete it; a DNS challenge token goes to the caddy service through docker-compose.override.yml (docs/deployment.md#choosing-a-recipe)"
  grep -q '^PYN_EMAIL=' "$env" && warn "PYN_EMAIL is not a setting: for email use PYN_MAILER=smtp with PYN_SMTP_HOST and PYN_SMTP_FROM, and PYN_EMAIL_VERIFICATION to require verified addresses (docs/deployment.md#email)"
  return 0
}

check_required() {
  local k hint
  for k in "$@"; do
    [ -z "$(get_env "$env" "$k")" ] || continue
    case $k in
      POSTGRES_PASSWORD) hint="set a password: openssl rand -hex 24" ;;
      PYN_SECRET_KEY) hint="set a key and keep it with your backups: openssl rand -hex 32" ;;
      PYN_SERVER_VERSION | PYN_WEB_VERSION) hint="set the image tag you run, for example v0.2.0" ;;
      PYN_API_URL) hint="set the address of the pyn server, for example https://pyn.example.com" ;;
      PYN_PUBLIC_URL) hint="set the address people type in the browser (or set PYN_SITE and let Compose derive it), for example http://localhost:8080" ;;
      *) hint="set it" ;;
    esac
    problem "$k is missing or empty in $env: $hint"
  done
}

check_version_tag() { # check_version_tag KEY: an image tag is v followed by X.Y.Z, nothing else
  local k=$1 v
  v=$(get_env "$env" "$k")
  [ -n "$v" ] || return 0
  [[ $v =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] && return 0
  if [[ $v =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    problem "$k=$v has no leading v (image tags are v0.2.0, not 0.2.0): set $k=v$v"
  elif [[ $v =~ ^v([0-9]+\.[0-9]+\.[0-9]+)- ]]; then
    problem "$k=$v carries a release title: the image tag is the version alone: set $k=v${BASH_REMATCH[1]}"
  else
    problem "$k=$v is not a version tag: set it to v followed by the version, for example v0.2.0"
  fi
  return 0
}

check_url_shape() { # check_url_shape KEY: scheme, host and optional port, no path
  local k=$1 v
  v=$(get_env "$env" "$k")
  [ -n "$v" ] || return 0
  [[ $v =~ ^https?://[^[:space:]/]+$ ]] || warn "$k=$v should look like http://host:port or https://host with no path"
  return 0
}

check_caddy() {
  local site url host cur
  site=$(get_env "$env" PYN_SITE)
  if ! has_caddy_profile "$env"; then
    [ -z "$site" ] || [ ! -f "$compose" ] || say "PYN_SITE is set without the caddy profile: Compose still derives https://$site as PYN_PUBLIC_URL, but no HTTPS proxy runs here"
    url=$(get_env "$env" PYN_PUBLIC_URL)
    case $url in https://*) warn "PYN_PUBLIC_URL is https but Caddy is off: fine behind a TLS proxy of your own (then PYN_TRUSTED_PROXIES must count it, docs/deployment.md#public-address-and-client-addresses-per-layout), otherwise browsers will not reach it" ;; esac
    return 0
  fi
  if [ "$shape" = server ]; then
    problem "the caddy profile is on but a server-only Compose file has no caddy service: remove COMPOSE_PROFILES=caddy, or use the full or web-only Compose file"
    return 0
  fi
  if [ -f "$compose" ]; then
    if ! grep -q '^  caddy:' "$compose" || ! grep -q 'caddy-data:/data' "$compose" || ! grep -q '^  caddy-data:' "$compose"; then
      problem "$compose has no caddy service with a persistent caddy-data volume: refresh it with scripts/install.sh --reuse refresh (certificates would be lost on every recreate)"
    fi
  fi
  [ -f "$dir/Caddyfile" ] || problem "COMPOSE_PROFILES=caddy needs $dir/Caddyfile next to the Compose file: run scripts/install.sh --caddy --site NAME to write it, or see docs/deployment.md#the-installers-caddy-option"
  if [ -z "$site" ]; then
    problem "COMPOSE_PROFILES=caddy needs PYN_SITE: set PYN_SITE to the host name or address people use, with no scheme or port"
  elif [[ ! $site =~ ^(\[[0-9A-Fa-f:.]+\]|[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?)$ ]]; then
    problem "PYN_SITE=$site must be a bare host name or address with no scheme, port or path: set for example PYN_SITE=pyn.example.com"
  elif [ -f "$dir/Caddyfile" ] && ! grep -qiF "$site" "$dir/Caddyfile"; then
    warn "$dir/Caddyfile does not mention PYN_SITE=$site: it serves another name"
  fi
  url=$(get_env "$env" PYN_PUBLIC_URL)
  if [ -n "$url" ]; then
    case $url in
      https://*) host=${url#https://}; host=${host%%[:/]*}
        if [ -n "$site" ] && [ "$(echo "$host" | tr A-Z a-z)" != "$(echo "$site" | tr A-Z a-z)" ]; then
          warn "PYN_PUBLIC_URL host $host differs from PYN_SITE=$site: sign-in, cookies and passkeys follow PYN_PUBLIC_URL"
        fi ;;
      *) problem "PYN_PUBLIC_URL=$url is wrong with Caddy on (browsers arrive over https): set PYN_PUBLIC_URL=https://${site:-<name>}, or delete the line and let Compose derive it from PYN_SITE" ;;
    esac
  fi
  [ "$shape" = web ] && return 0
  cur=$(get_env "$env" PYN_TRUSTED_PROXIES)
  if [ -z "$cur" ]; then
    problem "PYN_TRUSTED_PROXIES is not set with Caddy on: set PYN_TRUSTED_PROXIES=2 (Caddy and the web container are two proxies)"
  elif [ "$cur" = 1 ]; then
    problem "PYN_TRUSTED_PROXIES=1 is wrong with Caddy on: set PYN_TRUSTED_PROXIES=2 (Caddy, then the web container)"
  fi
  return 0
}

check_proxies() {
  local trust cur
  cur=$(get_env "$env" PYN_TRUSTED_PROXIES)
  trust=$(get_env "$env" PYN_TRUST_FORWARDED_FOR)
  if [ "$shape" = web ]; then
    [ -z "$cur$trust" ] || warn "PYN_TRUSTED_PROXIES and PYN_TRUST_FORWARDED_FOR are server settings and do nothing in a web-only .env: set them in the .env of the server behind this web app"
    return 0
  fi
  [ -n "$cur" ] || return 0
  if [[ ! $cur =~ ^[0-9]+$ ]] || [ "$cur" -lt 1 ] || [ "$cur" -gt 16 ]; then
    problem "PYN_TRUSTED_PROXIES=$cur must be a whole number from 1 to 16: set the number of proxies that append to X-Forwarded-For (2 with Caddy)"
  fi
  case $trust in
    true) ;;
    "") ;;
    *) problem "PYN_TRUSTED_PROXIES is set but PYN_TRUST_FORWARDED_FOR=$trust: the server refuses that; set PYN_TRUST_FORWARDED_FOR=true, or delete PYN_TRUSTED_PROXIES" ;;
  esac
  return 0
}

check_unknown() {
  local k hint
  while IFS= read -r k; do
    case $k in PYN_VERSION | PYN_IMAGE | PYN_WEB_IMAGE | PYN_TLS | PYN_TRUSTED_PROXY_RANGES | PYN_CF_TOKEN_FILE | PYN_EMAIL) continue ;; esac
    is_known "$k" && continue
    hint=$(suggest "$k")
    if [ -n "$hint" ]; then
      warn "$k is not a setting pyn reads: did you mean $hint?"
    else
      warn "$k is not a setting pyn reads: delete it if it is a leftover"
    fi
  done < <(sed -n 's/^\(PYN_[A-Za-z0-9_]*\)=.*/\1/p' "$env" | sort -u)
  return 0
}

check_ports() {
  local http tls p busy="" running=""
  has_caddy_profile "$env" || return 0
  [ "$no_ports" -eq 0 ] || return 0
  http=$(get_env "$env" PYN_WEB_PUBLISH_HTTP); http=${http##*:}
  tls=$(get_env "$env" PYN_WEB_PUBLISH_TLS); tls=${tls##*:}
  if have_docker && [ -f "$compose" ]; then
    running=$( (cd "$dir" && docker compose ps --status running -q caddy 2>/dev/null) || true)
  fi
  if [ -n "$running" ]; then
    say "ports for Caddy are already held by this install's running caddy container"
    return 0
  fi
  for p in ${PYN_INSTALL_PROBE_PORTS:-${http:-80} ${tls:-443}}; do
    [ -n "$p" ] || continue
    if port_busy "$p"; then busy="$busy $p"; fi
  done
  [ -z "$busy" ] || warn "port${busy} already in use on this host, and Caddy needs them (another web server or proxy?): stop it, move Caddy with PYN_WEB_PUBLISH_HTTP and PYN_WEB_PUBLISH_TLS, or run without Caddy (docs/deployment.md#ports-and-exposure)"
  return 0
}

check_server() {
  local ver rc=0 out line
  if [ "$no_server" -eq 1 ]; then return 0; fi
  [ "$shape" != web ] || return 0
  if [ "$problems" -gt 0 ]; then
    say "the server's own check was not run: fix the problems above first"
    return 0
  fi
  ver=$(get_env "$env" PYN_SERVER_VERSION)
  ver=${ver#v}
  if [[ ! $ver =~ ^[0-9]+\.[0-9]+\.[0-9]+ ]]; then
    say "the server's own check needs a release with --check-config (from $CHECK_CONFIG_SINCE) and PYN_SERVER_VERSION is not a version, so it was skipped"
    return 0
  fi
  ver=${BASH_REMATCH[0]}
  if ver_lt "$ver" "$CHECK_CONFIG_SINCE"; then
    say "the server's own check needs a release with --check-config (from $CHECK_CONFIG_SINCE); PYN_SERVER_VERSION is $ver, so it was skipped"
    return 0
  fi
  if ! have_docker; then
    say "docker compose was not found, so the server's own check was skipped: run it later with docker compose run --rm --no-deps server --check-config"
    return 0
  fi
  [ -f "$compose" ] || return 0
  say "running the server's own check (docker compose run, up to ${SERVER_CHECK_TIMEOUT}s; a missing image is pulled first)"
  out=$(mktemp)
  if command -v timeout >/dev/null 2>&1; then
    (cd "$dir" && timeout "$SERVER_CHECK_TIMEOUT" docker compose run --rm --no-deps -T server --check-config) >"$out" 2>&1 </dev/null || rc=$?
  else
    (cd "$dir" && docker compose run --rm --no-deps -T server --check-config) >"$out" 2>&1 </dev/null || rc=$?
  fi
  while IFS= read -r line; do echo "    ${c_dim}$line${c_reset}"; done <"$out"
  case $rc in
    0) say "the server accepts this configuration"
       warnings=$((warnings + $(grep -c '^warning:' "$out" || true))) ;;
    124) problem "the server's own check did not finish in ${SERVER_CHECK_TIMEOUT}s: run docker compose run --rm --no-deps server --check-config yourself to see why" ;;
    1) problem "the server's own check found problems (above): change those PYN_* lines in $env" ;;
    *) warn "the server's own check could not run (exit $rc, output above): is the image pullable? Run it yourself: docker compose run --rm --no-deps server --check-config" ;;
  esac
  rm -f "$out"
  return 0
}

main() {
  while [ $# -gt 0 ]; do
    case $1 in
      --strict) strict=1 ;;
      --no-ports) no_ports=1 ;;
      --no-server) no_server=1 ;;
      --shape) shape_in=${2:-}; case $shape_in in full | server | web) ;; *) die "--shape must be full, server or web" ;; esac; shift ;;
      --color) color_flag=always ;;
      --no-color) color_flag=never ;;
      --known) echo $KNOWN_NAMES | tr ' ' '\n'; exit 0 ;;
      -h | --help) usage; exit 0 ;;
      -*) die "unknown option $1 (see --help)" ;;
      *) [ -z "$dir" ] || die "only one folder can be checked"; dir=$1 ;;
    esac
    shift
  done
  setup_color
  dir=${dir:-.}
  dir=${dir%/}
  [ -d "$dir" ] || die "$dir is not a folder"
  env=$dir/.env
  compose=$dir/docker-compose.yml
  say "checking $env"

  if [ ! -f "$env" ]; then
    problem "$env does not exist: copy deploy/.env.example (or .env.server.example, .env.web.example) there and fill it in, or run scripts/install.sh"
  else
    shape=full
    if [ -f "$compose" ]; then
      if grep -q '^  server:' "$compose" && grep -q '^  web:' "$compose"; then shape=full
      elif grep -q '^  server:' "$compose"; then shape=server
      elif grep -q '^  web:' "$compose"; then shape=web; fi
      if grep -q '\${PYN_VERSION' "$compose"; then
        problem "$compose is from before PYN_SERVER_VERSION and PYN_WEB_VERSION: refresh it with scripts/install.sh --reuse refresh"
      fi
    else
      problem "no docker-compose.yml next to $env: run scripts/install.sh, or copy the Compose file of your release from deploy/ there as docker-compose.yml"
      if [ -z "$(get_env "$env" POSTGRES_PASSWORD)" ] && [ -n "$(get_env "$env" PYN_API_URL)" ]; then shape=web; fi
    fi
    [ -z "$shape_in" ] || shape=$shape_in
    check_perms
    check_renamed
    case $shape in
      full) check_required POSTGRES_PASSWORD PYN_SECRET_KEY PYN_SERVER_VERSION PYN_WEB_VERSION ;;
      server) check_required POSTGRES_PASSWORD PYN_SECRET_KEY PYN_SERVER_VERSION ;;
      web) check_required PYN_WEB_VERSION PYN_API_URL ;;
    esac
    if [ "$shape" != web ] && [ -z "$(get_env "$env" PYN_SITE)" ]; then check_required PYN_PUBLIC_URL; fi
    [ "$shape" = web ] || check_version_tag PYN_SERVER_VERSION
    [ "$shape" = server ] || check_version_tag PYN_WEB_VERSION
    check_url_shape PYN_PUBLIC_URL
    [ "$shape" != web ] || check_url_shape PYN_API_URL
    check_caddy
    check_proxies
    check_unknown
    check_ports
    check_server
  fi

  if [ "$problems" -gt 0 ] || { [ "$strict" -eq 1 ] && [ "$warnings" -gt 0 ]; }; then
    echo "${c_red}pyn-check: $problems problem(s), $warnings warning(s): fix them before starting or upgrading${c_reset}"
    exit 1
  fi
  if [ "$warnings" -gt 0 ]; then echo "${c_green}pyn-check: ok${c_reset} ($warnings warning(s))"; else echo "${c_green}pyn-check: ok${c_reset}"; fi
}

main "$@"
