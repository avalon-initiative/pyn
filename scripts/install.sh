#!/usr/bin/env bash
# Interactive installer for a pyn release: server, web app or CLI, with Docker or as a native systemd service.
# Run it from a checkout, copy it to a host, or pipe it: curl -fsSL <raw url> | bash   (prompts read from the terminal).
set -euo pipefail

repo=avalon-initiative/pyn
shape="" method="" version_in="" dir="" public_url="" api_url="" database_url=""
yes=0 no_install=0 interactive=0 tmp=""
root=${PYN_INSTALL_ROOT:-}
bin_dir="$root/usr/local/bin" etc_dir="$root/etc/pyn" unit_dir="$root/etc/systemd/system" state_dir="$root/var/lib/pyn"

usage() {
  cat <<'EOF'
Usage: install.sh [options]

Asks what to install, how to run it and which release, checks the host, offers to install missing tools and starts it.
Every answer has a flag; with --yes (or no terminal) a missing answer takes its default or the script stops.

  --shape full|server|web|cli   full: server and web app (default); server; web: web app only; cli: the pyn command only
  --method docker|native        docker (default); native runs the release pyn-server binary under systemd (server and cli only)
  --version TAG                 a full tag such as v0.1.2-alpha, or a bare version such as 0.1.2 (default: the newest release,
                                pre-releases included)
  --dir DIR                     where the Docker files live (default /opt/pyn)
  --public-url URL              the address people type in the browser (default http://<this host>:8080, or :7878 for a server)
  --api-url URL                 web: the pyn server the web app talks to (PYN_API_URL); cli: the server to point the command at
  --database-url URL            native: use this PostgreSQL (postgres://...) instead of creating a local database
  --yes                         never ask: take defaults, and install missing tools without asking
  --no-install                  never install anything; only report what is missing
  -h, --help                    this text

Re-running on an existing install upgrades it: settings and secrets are kept and only the version moves.
EOF
}

say() { echo "pyn-install: $*"; }
die() { echo "pyn-install: $*" >&2; exit 1; }

cleanup() { [ -z "$tmp" ] || rm -rf "$tmp"; }
trap cleanup EXIT
tmp=$(mktemp -d)

while [ $# -gt 0 ]; do
  case $1 in
    --shape) shape=${2:?--shape needs a value}; shift 2 ;;
    --method) method=${2:?--method needs a value}; shift 2 ;;
    --version) version_in=${2:?--version needs a value}; shift 2 ;;
    --dir) dir=${2:?--dir needs a value}; shift 2 ;;
    --public-url) public_url=${2:?--public-url needs a value}; shift 2 ;;
    --api-url) api_url=${2:?--api-url needs a value}; shift 2 ;;
    --database-url) database_url=${2:?--database-url needs a value}; shift 2 ;;
    --yes | -y) yes=1; shift ;;
    --no-install) no_install=1; shift ;;
    -h | --help) usage; exit 0 ;;
    *) die "unknown option $1 (see --help)" ;;
  esac
done

# Prompts

open_terminal() {
  local src=${PYN_INSTALL_TTY:-/dev/tty}
  [ "$yes" -eq 0 ] || return 0
  if ( : <"$src" ) 2>/dev/null; then
    exec 3<"$src"
    interactive=1
  fi
}

read_answer() {
  local line
  IFS= read -r -u 3 line || die "input ended while waiting for: $1"
  printf '%s' "$line" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//'
}

# ask VAR label default flag: keeps a value already given by flag
ask() {
  local var=$1 label=$2 def=${3:-} flag=${4:-} ans
  [ -z "${!var}" ] || return 0
  if [ "$interactive" -eq 0 ]; then
    [ -n "$def" ] || die "$label is required: pass $flag"
    printf -v "$var" '%s' "$def"
    return 0
  fi
  while :; do
    if [ -n "$def" ]; then printf '%s [%s]: ' "$label" "$def"; else printf '%s: ' "$label"; fi
    ans=$(read_answer "$label")
    [ -n "$ans" ] || ans=$def
    if [ -n "$ans" ]; then printf -v "$var" '%s' "$ans"; return 0; fi
  done
}

# menu VAR title default flag "value|text"...
menu() {
  local var=$1 title=$2 def=$3 flag=$4 opt i ans v
  shift 4
  [ -z "${!var}" ] || return 0
  if [ "$interactive" -eq 0 ]; then
    [ -n "$def" ] || die "$title is required: pass $flag"
    printf -v "$var" '%s' "$def"
    return 0
  fi
  echo "$title"
  i=1
  for opt in "$@"; do
    v=${opt%%|*}
    if [ "$v" = "$def" ]; then echo "  $i) ${opt#*|} (default)"; else echo "  $i) ${opt#*|}"; fi
    i=$((i + 1))
  done
  while :; do
    printf 'Choice: '
    ans=$(read_answer "$title")
    [ -n "$ans" ] || ans=$def
    i=1
    for opt in "$@"; do
      v=${opt%%|*}
      if [ "$ans" = "$v" ] || [ "$ans" = "$i" ]; then printf -v "$var" '%s' "$v"; return 0; fi
      i=$((i + 1))
    done
    echo "Enter a number or one of the names above."
  done
}

# confirm question default(y|n): --yes accepts the default, no terminal takes it too
confirm() {
  local ans
  [ "$interactive" -eq 1 ] || { [ "$2" = y ]; return; }
  while :; do
    printf '%s [%s]: ' "$1" "$([ "$2" = y ] && echo Y/n || echo y/N)"
    ans=$(read_answer "$1")
    case $ans in
      "") [ "$2" = y ]; return ;;
      y | Y | yes | YES) return 0 ;;
      n | N | no | NO) return 1 ;;
    esac
  done
}

# Files and settings

set_env() { # set_env FILE KEY VALUE: replaces the active line, else the first commented one, else appends
  local f=$1 t="$1.tmp"
  K=$2 V=$3 awk '
    BEGIN { k = ENVIRON["K"]; v = ENVIRON["V"] }
    { line[NR] = $0; if (index($0, k "=") == 1) active = 1 }
    END {
      for (i = 1; i <= NR; i++) {
        l = line[i]
        if (index(l, k "=") == 1) { print k "=" v; continue }
        s = l; sub(/^#[ ]*/, "", s)
        if (!active && !done && index(s, k "=") == 1) { print k "=" v; done = 1; continue }
        print l
      }
      if (!active && !done) print k "=" v
    }' "$f" >"$t"
  cat "$t" >"$f"
  rm -f "$t"
}

get_env() { sed -n "s/^$2=//p" "$1" 2>/dev/null | head -n1 || true; }

fill_secret() { # fill_secret FILE KEY BYTES: only when the value is empty
  [ -n "$(get_env "$1" "$2")" ] || set_env "$1" "$2" "$(openssl rand -hex "$3")"
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'; else shasum -a 256 "$1" | awk '{print $1}'; fi
}

default_host() {
  local h
  h=$(hostname -I 2>/dev/null | awk '{print $1}') || true
  [ -n "$h" ] || h=$(hostname 2>/dev/null) || true
  echo "${h:-localhost}"
}

is_local_url() {
  case $1 in
    http://localhost* | http://127.* | http://\[::1\]*) return 0 ;;
  esac
  return 1
}

# Host and tools

is_debian() {
  local f=${PYN_INSTALL_OS_RELEASE:-/etc/os-release}
  [ -r "$f" ] && command -v apt-get >/dev/null 2>&1 && grep -Eq '^(ID|ID_LIKE)=.*(debian|ubuntu)' "$f"
}

as_root() {
  if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo "$@"; fi
}

have_postgres() {
  command -v psql >/dev/null 2>&1 || return 1
  systemctl is-active --quiet postgresql 2>/dev/null || { command -v pg_isready >/dev/null 2>&1 && pg_isready -q; }
}

have_tool() {
  case $1 in
    sha256) command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1 ;;
    compose) docker compose version >/dev/null 2>&1 ;;
    postgres) have_postgres ;;
    *) command -v "$1" >/dev/null 2>&1 ;;
  esac
}

tool_label() {
  case $1 in
    sha256) echo "sha256sum (or shasum)" ;;
    compose) echo "docker compose plugin" ;;
    postgres) echo "PostgreSQL server and psql, running" ;;
    *) echo "$1" ;;
  esac
}

needed_tools() {
  local t="curl"
  case $method in
    docker)
      [ "$shape" = web ] || t="$t openssl"
      t="$t docker compose"
      ;;
    native)
      t="$t tar sha256"
      if [ "$shape" != cli ]; then
        t="$t openssl systemctl"
        [ -n "$database_url" ] || [ -f "$etc_dir/pyn.env" ] || t="$t postgres"
      fi
      ;;
  esac
  echo "$t"
}

check_tools() {
  local t missing="" label
  echo "Checking this host:"
  for t in $(needed_tools); do
    label=$(tool_label "$t")
    if have_tool "$t"; then
      echo "  ok       $label"
    else
      echo "  missing  $label"
      missing="$missing $t"
    fi
  done
  [ -z "$missing" ] || install_missing "${missing# }"
}

stop_missing() { # stop_missing "<tools>" reason
  local t
  {
    echo "pyn-install: these must be installed first ($2):"
    for t in $1; do echo "  - $(tool_label "$t")"; done
    echo "On Ubuntu or Debian: apt-get install curl tar coreutils openssl postgresql; Docker: https://docs.docker.com/engine/install/"
  } >&2
  exit 1
}

install_missing() {
  local missing=$1 t pkgs="" docker=0 why=""
  for t in $missing; do
    case $t in
      curl) pkgs="$pkgs curl ca-certificates" ;;
      tar) pkgs="$pkgs tar" ;;
      sha256) pkgs="$pkgs coreutils" ;;
      openssl) pkgs="$pkgs openssl" ;;
      postgres) pkgs="$pkgs postgresql" ;;
      docker | compose) docker=1 ;;
      systemctl) why="systemd is not available, which the native method needs" ;;
    esac
  done
  if [ -n "$why" ]; then stop_missing "$missing" "$why"; fi
  if [ "$no_install" -eq 1 ]; then why="--no-install"
  elif ! is_debian; then why="this host is not Ubuntu or Debian, so nothing is installed for you"
  elif [ "$(id -u)" -ne 0 ] && ! command -v sudo >/dev/null 2>&1; then why="run as root or with sudo so packages can be installed"
  elif [ "$interactive" -eq 0 ] && [ "$yes" -eq 0 ]; then why="no terminal to ask on; pass --yes to install"
  fi
  [ -z "$why" ] || stop_missing "$missing" "$why"
  confirm "Install the missing tools now?" y || stop_missing "$missing" "installation declined"
  if [ -n "$pkgs" ]; then
    say "installing packages:$pkgs"
    as_root apt-get update -qq
    # shellcheck disable=SC2086
    as_root apt-get install -y -qq $pkgs
  fi
  if [ "$docker" -eq 1 ]; then
    say "installing Docker with get.docker.com; in an LXC container the container needs security.nesting=true"
    curl -fsSL https://get.docker.com -o "$tmp/get-docker.sh" || die "could not download https://get.docker.com"
    as_root sh "$tmp/get-docker.sh"
  fi
  for t in $missing; do
    have_tool "$t" || die "$(tool_label "$t") is still missing after the install"
  done
}

# Releases

api_tags() { # api_tags COUNT: tag names, newest first
  local out code
  out="$tmp/releases.json"
  code=$(curl -sSL -H 'Accept: application/vnd.github+json' -o "$out" -w '%{http_code}' \
    "https://api.github.com/repos/$repo/releases?per_page=$1") || code=000
  case $code in
    200) ;;
    403 | 429) die "GitHub's release API refused the request (rate limit: 60 an hour without a token); try again later" ;;
    000) die "could not reach api.github.com; check the network connection" ;;
    *) die "GitHub's release API answered HTTP $code" ;;
  esac
  grep -o '"tag_name": *"[^"]*"' "$out" | sed 's/.*"\([^"]*\)"$/\1/'
}

match_tag() { # match_tag INPUT: a full tag, or a bare version mapped to its release tag
  local want=${1#v} t tags
  [[ $want =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9._-]+)?$ ]] || die "'$1' is not a version: use a tag like v0.1.2-alpha or a version like 0.1.2"
  tags=$(api_tags 100)
  for t in $tags; do
    if [ "${t#v}" = "$want" ]; then echo "$t"; return 0; fi
  done
  for t in $tags; do
    case ${t#v} in "$want"-*) echo "$t"; return 0 ;; esac
  done
  die "no release matches '$1'; see https://github.com/$repo/releases"
}

resolve_version() {
  local newest
  if [ -z "$version_in" ]; then
    newest=$(api_tags 1 | head -n1)
    [ -n "$newest" ] || die "GitHub lists no releases for $repo"
    if [ "$interactive" -eq 1 ]; then
      echo "Which release? Enter for the newest, or type a full tag (v0.1.1-alpha) or a version (0.1.1)."
      ask version_in "Version" "$newest" --version
    else
      version_in=$newest
    fi
    if [ "$version_in" = "$newest" ]; then tag=$newest; else tag=$(match_tag "$version_in"); fi
  else
    tag=$(match_tag "$version_in")
  fi
  [[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9._-]+)?$ ]] || die "unexpected release tag '$tag'"
  ver=${tag#v}
  ver=${ver%%-*}
}

# Downloads and checksums

fetch() { curl -fsSL -o "$2" "$1" || die "download failed: $1"; }

platform_for() { # platform_for shape
  local os arch
  os=$(uname -s) arch=$(uname -m)
  case "$os $arch" in
    "Linux x86_64" | "Linux amd64") echo linux-x86_64 ;;
    "Linux aarch64" | "Linux arm64") echo linux-arm64 ;;
    "Darwin arm64")
      [ "$1" = cli ] || die "the native server has no macOS build; use Docker, or run it on Linux"
      echo macos-arm64
      ;;
    *) die "no release binaries for $os $arch (Linux x86_64 and arm64; macOS arm64 for the CLI)" ;;
  esac
}

# fetch_verified KIND: downloads the archive and SHA256SUMS into $tmp, checks the sum, prints the extracted directory
fetch_verified() {
  local kind=$1 name base want got
  name="pyn-$kind-$tag-$platform"
  base="https://github.com/$repo/releases/download/$tag"
  mkdir -p "$tmp/dl"
  say "downloading $name.tar.gz" >&2
  fetch "$base/$name.tar.gz" "$tmp/dl/$name.tar.gz"
  fetch "$base/SHA256SUMS" "$tmp/dl/SHA256SUMS"
  want=$(awk -v f="$name.tar.gz" '$2 == f || $2 == "*" f { print $1 }' "$tmp/dl/SHA256SUMS" | head -n1)
  [ -n "$want" ] || die "SHA256SUMS has no entry for $name.tar.gz; nothing was installed"
  got=$(sha256_of "$tmp/dl/$name.tar.gz")
  [ "$got" = "$want" ] || die "checksum mismatch for $name.tar.gz (expected $want, got $got); nothing was installed"
  say "checksum verified" >&2
  tar -xzf "$tmp/dl/$name.tar.gz" -C "$tmp/dl"
  [ -d "$tmp/dl/$name" ] || die "unexpected archive layout in $name.tar.gz"
  echo "$tmp/dl/$name"
}

install_binary() { # install_binary SRC DEST
  install -m 0755 "$1" "$2.new"
  mv -f "$2.new" "$2"
}

wait_health() {
  local i=0 max=${PYN_INSTALL_HEALTH_TRIES:-60}
  until curl -fsS -o /dev/null "http://127.0.0.1:7878/healthz" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -lt "$max" ] || return 1
    sleep "${PYN_INSTALL_POLL:-1}"
  done
}

token_block() { grep -A3 "one-time setup token" | tail -n 4 || true; }

# Docker

docker_flow() {
  local compose envfile old="" env_path base fresh=1 bind="" file
  case $shape in
    full) compose=docker-compose.yml envfile=.env.example ;;
    server) compose=docker-compose.server.yml envfile=.env.server.example ;;
    web) compose=docker-compose.web.yml envfile=.env.web.example ;;
  esac
  mkdir -p "$dir" 2>/dev/null || die "cannot create $dir; run with sudo or pass --dir"
  [ -w "$dir" ] || die "cannot write to $dir; run with sudo or pass --dir"
  env_path="$dir/.env"
  base="https://raw.githubusercontent.com/$repo/$tag/deploy"
  [ ! -f "$env_path" ] || fresh=0

  file="$tmp/compose.yml"
  fetch "$base/$compose" "$file"
  if [ -f "$dir/docker-compose.yml" ] && ! cmp -s "$file" "$dir/docker-compose.yml"; then
    cp "$dir/docker-compose.yml" "$dir/docker-compose.yml.bak"
    say "docker-compose.yml changed in $tag; the old one is saved as docker-compose.yml.bak"
  fi
  cat "$file" >"$dir/docker-compose.yml"

  if [ "$fresh" -eq 1 ]; then
    if ! curl -fsSL -o "$tmp/env" "$base/$envfile" 2>/dev/null; then
      say "$envfile is not in release $tag; using the one from main"
      if ! curl -fsSL -o "$tmp/env" "https://raw.githubusercontent.com/$repo/main/deploy/$envfile" 2>/dev/null; then
        say "no $envfile found; starting from an empty .env"
        : >"$tmp/env"
      fi
    fi
    ( umask 077; cat "$tmp/env" >"$env_path" )
  else
    old=$(get_env "$env_path" PYN_VERSION)
  fi
  chmod 600 "$env_path"
  set_env "$env_path" PYN_VERSION "v$ver"
  if [ "$shape" != web ]; then
    fill_secret "$env_path" POSTGRES_PASSWORD 24
    fill_secret "$env_path" PYN_SECRET_KEY 32
  fi
  if [ "$fresh" -eq 1 ]; then
    if ! is_local_url "$public_url" && [ "${public_url#https://}" = "$public_url" ]; then bind=0.0.0.0; fi
    case $shape in
      full) set_env "$env_path" PYN_PUBLIC_URL "$public_url"; [ -z "$bind" ] || set_env "$env_path" PYN_WEB_PUBLISH "$bind:8080" ;;
      server) set_env "$env_path" PYN_PUBLIC_URL "$public_url"; [ -z "$bind" ] || set_env "$env_path" PYN_SERVER_PUBLISH "$bind:7878" ;;
      web) set_env "$env_path" PYN_API_URL "$api_url"; [ -z "$bind" ] || set_env "$env_path" PYN_WEB_PUBLISH "$bind:8080" ;;
    esac
  fi

  if [ "$fresh" -eq 0 ]; then
    say "upgrade: keeping $env_path, PYN_VERSION ${old:-unset} -> v$ver"
  else
    say "wrote $env_path (mode 600) with generated secrets; PYN_VERSION v$ver"
  fi
  say "starting the stack"
  ( cd "$dir" && docker compose pull && docker compose up --detach --wait ) ||
    die "the stack did not come up; see: cd $dir && docker compose logs"
  say "up. Files are in $dir"
  if [ "$shape" = web ]; then
    say "add this web address to PYN_ALLOWED_ORIGINS on the server at $api_url, then restart it: $public_url"
  elif [ "$fresh" -eq 1 ]; then
    say "one-time setup token (finish setup in the browser, or run: pyn setup <username>):"
    ( cd "$dir" && docker compose logs server 2>&1 ) | token_block
  fi
}

# Native

native_unit() {
  cat <<'EOF'
[Unit]
Description=pyn server
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
User=pyn
Group=pyn
EnvironmentFile=/etc/pyn/pyn.env
ExecStart=/usr/local/bin/pyn-server
Restart=on-failure
RestartSec=2
StateDirectory=pyn
StateDirectoryMode=0750
UMask=0077
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
CapabilityBoundingSet=
AmbientCapabilities=
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
RestrictNamespaces=true
RestrictSUIDSGID=true
LockPersonality=true
SystemCallArchitectures=native

[Install]
WantedBy=multi-user.target
EOF
}

pg_sql() { su postgres -c 'psql -v ON_ERROR_STOP=1 -tA'; }

create_local_database() { # prints the database URL
  local pw
  [ "$(printf "select 1 from pg_roles where rolname='pyn';\n" | pg_sql)" != 1 ] ||
    die "a PostgreSQL role named pyn already exists; pass --database-url, or remove it yourself first"
  [ "$(printf "select 1 from pg_database where datname='pyn';\n" | pg_sql)" != 1 ] ||
    die "a PostgreSQL database named pyn already exists; pass --database-url, or remove it yourself first"
  pw=$(openssl rand -hex 24)
  printf "create role pyn login password '%s';\ncreate database pyn owner pyn;\n" "$pw" | pg_sql >/dev/null
  echo "postgres://pyn:$pw@127.0.0.1:5432/pyn"
}

native_flow() {
  local env_path="$etc_dir/pyn.env" old="" src fresh=1 addr
  [ "$(id -u)" -eq 0 ] || die "the native install needs root: re-run with sudo"
  [ ! -f "$env_path" ] || fresh=0
  [ ! -f "$etc_dir/version" ] || old=$(cat "$etc_dir/version")

  src=$(fetch_verified server)

  if ! id pyn >/dev/null 2>&1; then
    say "creating the system user pyn"
    useradd --system --user-group --home-dir /var/lib/pyn --no-create-home --shell /usr/sbin/nologin pyn
  fi
  mkdir -p "$bin_dir" "$state_dir" "$etc_dir" "$unit_dir"
  chmod 0755 "$etc_dir"
  chmod 0750 "$state_dir"
  chown pyn:pyn "$state_dir"
  install_binary "$src/pyn-server" "$bin_dir/pyn-server"

  if [ "$fresh" -eq 1 ]; then
    [ -n "$database_url" ] || database_url=$(create_local_database)
    addr=0.0.0.0:7878
    ! is_local_url "$public_url" || addr=127.0.0.1:7878
    ( umask 077
      {
        echo "PYN_ADDR=$addr"
        echo "PYN_DATABASE_URL=$database_url"
        echo "PYN_SECRET_KEY=$(openssl rand -hex 32)"
        echo "PYN_PUBLIC_URL=$public_url"
        echo "PYN_DATA_DIR=/var/lib/pyn"
      } >"$env_path" )
    chmod 600 "$env_path"
    say "wrote $env_path (mode 600) with generated secrets"
  else
    say "upgrade: keeping $env_path, ${old:-unknown version} -> $tag"
  fi
  if [ ! -f "$unit_dir/pyn.service" ]; then
    native_unit >"$unit_dir/pyn.service"
    say "wrote $unit_dir/pyn.service"
  fi
  echo "$tag" >"$etc_dir/version"

  systemctl daemon-reload
  systemctl enable pyn
  systemctl restart pyn
  say "waiting for http://127.0.0.1:7878/healthz"
  wait_health || die "the server did not become healthy; see: journalctl -u pyn -n 50"
  say "up (pyn $tag, systemd unit pyn)"
  if [ "$fresh" -eq 1 ]; then
    say "one-time setup token (finish setup with: pyn setup <username>, or in the browser if a web app is attached):"
    journalctl -u pyn --no-pager -o cat 2>&1 | token_block
    say "setup from the pyn command: PYN_SETUP_TOKEN=<token> pyn --server $public_url setup <username> --public-url $public_url"
  fi
}

# CLI

cli_flow() {
  local src dest
  src=$(fetch_verified cli)
  if [ "$(id -u)" -eq 0 ]; then dest=$bin_dir; else dest=$HOME/.local/bin; fi
  mkdir -p "$dest"
  install_binary "$src/pyn" "$dest/pyn"
  say "installed $dest/pyn ($tag)"
  case ":$PATH:" in *":$dest:"*) ;; *) say "add $dest to your PATH" ;; esac
  say "next: pyn config set --global server ${api_url:-<server-url>}"
  say "      pyn login <username>"
}

# Main

explain_web_native() {
  cat <<'EOF'
The web app is Docker-only for now: its nginx proxy configuration ships in the pyn-web image.
Without Docker you could serve the pyn-web build output (npm run build in a pyn-web checkout) with your own web server
that proxies /v1 and /healthz to the server; this script does not set that up.
EOF
}

detect_existing() {
  local c
  if [ -z "$method" ]; then
    if [ -f "$etc_dir/pyn.env" ]; then method_default=native; elif [ -f "${dir:-/opt/pyn}/.env" ]; then method_default=docker; fi
  fi
  c="${dir:-/opt/pyn}/docker-compose.yml"
  if [ -z "$shape" ] && [ -f "$c" ]; then
    if grep -q pyn-core "$c" && grep -q pyn-web "$c"; then shape_default=full
    elif grep -q pyn-core "$c"; then shape_default=server
    elif grep -q pyn-web "$c"; then shape_default=web
    fi
  fi
}

choose_database() {
  local choice=""
  [ -z "$database_url" ] && [ ! -f "$etc_dir/pyn.env" ] || return 0
  menu choice "PostgreSQL for the server: how should the database be set up?" local --database-url \
    "local|Create a role and database on this host, with a generated password" \
    "url|Use an existing PostgreSQL I will give the address of"
  [ "$choice" != url ] || ask database_url "PostgreSQL address (postgres://user:password@host:5432/db)" "" --database-url
}

main() {
  local method_default=docker shape_default=full platform="" tag="" ver="" newshape=""
  open_terminal
  detect_existing

  menu shape "What do you want to install?" "$shape_default" --shape \
    "full|Server and web app" "server|Server only" \
    "web|Web app only, for a server running elsewhere" "cli|The pyn command only"
  case $shape in full | server | web | cli) ;; *) die "--shape must be full, server, web or cli" ;; esac
  if [ "$shape" = cli ]; then
    method=native
  else
    menu method "How do you want to run it?" "$method_default" --method \
      "docker|Docker (release images and Compose files)" \
      "native|Native: the release pyn-server binary as a systemd service, with PostgreSQL"
  fi
  case $method in docker | native) ;; *) die "--method must be docker or native" ;; esac

  if [ "$method" = native ] && { [ "$shape" = full ] || [ "$shape" = web ]; }; then
    explain_web_native
    if [ "$interactive" -eq 1 ]; then
      if [ "$shape" = full ]; then
        menu newshape "What now?" docker --method \
          "docker|Run the server and web app with Docker" "server|Install only the server natively"
        if [ "$newshape" = docker ]; then method=docker; else shape=server; fi
      else
        confirm "Run the web app with Docker instead?" y || die "the web app needs Docker; nothing was installed"
        method=docker
      fi
    else
      die "choose --method docker, or --shape server for a native server; nothing was installed"
    fi
  fi

  if [ -n "$database_url" ] && [[ ! $database_url =~ ^postgres(ql)?://[^[:space:]]+$ ]]; then
    die "--database-url must look like postgres://user:password@host:5432/database"
  fi
  if [ "$method" = native ]; then platform=$(platform_for "$shape"); fi
  if [ "$method" = native ] && [ "$shape" = server ]; then choose_database; fi
  [ -n "$dir" ] || dir=/opt/pyn

  echo
  check_tools
  echo
  resolve_version

  case $shape in
    web)
      ask api_url "Address of the pyn server (PYN_API_URL)" "" --api-url
      ;;
    cli)
      ask api_url "Address of your pyn server (for the next step, optional)" "<server-url>" --api-url
      [ "$api_url" != "<server-url>" ] || api_url=""
      ;;
  esac
  api_url=${api_url%/}
  if [ -n "$api_url" ] && [ "$shape" != cli ] && [[ ! $api_url =~ ^https?://[^[:space:]/]+(:[0-9]+)?$ ]]; then
    die "--api-url must be an address like https://pyn.example.com with no path"
  fi
  case $shape in
    full | web | server)
      local port=8080 host
      { [ "$method" = native ] || [ "$shape" = server ]; } && port=7878
      host=$(default_host)
      echo "The browser's address must match PYN_PUBLIC_URL exactly (scheme, host and port); use https://... behind a TLS proxy."
      ask public_url "Address people will use (PYN_PUBLIC_URL)" "http://$host:$port" --public-url
      public_url=${public_url%/}
      [[ $public_url =~ ^https?://[^[:space:]/]+(:[0-9]+)?$ ]] || die "--public-url must look like http://host:port or https://host, with no path"
      ;;
  esac

  echo
  say "plan: $shape via $method, release $tag${ver:+ (image v$ver)}"
  confirm "Proceed?" y || die "stopped; nothing was changed"

  case $shape in
    cli) cli_flow ;;
    *) if [ "$method" = docker ]; then docker_flow; else native_flow; fi ;;
  esac
  say "done"
}

main
