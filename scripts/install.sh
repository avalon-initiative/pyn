#!/usr/bin/env bash
# Interactive installer for a pyn release: server, web app or CLI, with Docker or as a native systemd service.
# Run it from a checkout, copy it to a host, or pipe it: curl -fsSL <raw url> | bash   (prompts read from the terminal).
set -euo pipefail

repo=avalon-initiative/pyn
shape="" method="" version_in="" dir="" public_url="" api_url="" database_url="" channel="" env_file="" reuse=""
os_kind="" os_id="" arch="" native_env=""
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
  --channel stable|pre          stable: the latest non-pre-release; pre: the newest release of all, pre-releases included
                                (default: stable, or pre while no stable release exists)
  --version TAG                 a full tag such as v0.1.2-alpha, or a bare version such as 0.1.2 (wins over --channel)
  --dir DIR                     where the Docker files live (default /opt/pyn)
  --env-file PATH               an existing env file to reuse: copied to DIR/.env for Docker, used in place as the service's
                                EnvironmentFile for native (a native path that does not exist yet is where one is created)
  --reuse use|refresh|fresh     when DIR (or the native env file) already holds an install: use it as it is, refresh the
                                Compose file or binary from the release and keep the env (default), or start fresh (backs up)
  --public-url URL              the address people type in the browser (default http://<this host>:8080, or :7878 for a server)
  --api-url URL                 web: the pyn server the web app talks to (PYN_API_URL); cli: the server to point the command at
  --database-url URL            native: use this PostgreSQL (postgres://...) instead of creating a local database
  --yes                         never ask: take defaults, and install missing tools without asking
  --no-install                  never install anything; only report what is missing
  -h, --help                    this text

Re-running on an existing install upgrades it: settings and secrets are kept and only the version moves, after confirming.
Nothing is deleted: files that are replaced are copied to NAME.bak.<timestamp> first.
macOS can run Docker deployments (Docker Desktop needed) and the CLI; native hosting needs Linux with systemd.
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
    --channel) channel=${2:?--channel needs a value}; shift 2 ;;
    --env-file) env_file=${2:?--env-file needs a value}; shift 2 ;;
    --reuse) reuse=${2:?--reuse needs a value}; shift 2 ;;
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

ask_optional() { # ask_optional VAR label flag: an empty answer stays empty
  local var=$1 label=$2 ans
  [ -z "${!var}" ] || return 0
  [ "$interactive" -eq 1 ] || return 0
  printf '%s (Enter for none): ' "$label"
  ans=$(read_answer "$label")
  printf -v "$var" '%s' "$ans"
}

timestamp() { date +%Y%m%d%H%M%S; }

backup_file() { # copies FILE to FILE.bak.<timestamp>, owner-only
  local b n=0
  b="$1.bak.$(timestamp)"
  while [ -e "$b" ]; do n=$((n + 1)); b="$1.bak.$(timestamp).$n"; done
  ( umask 077; cp "$1" "$b" )
  say "backed up $1 as $b"
}

file_mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }

# Warns about a world-readable env file; fixes it only when owned, or when asked and agreed
check_env_perms() {
  local mode last
  mode=$(file_mode "$1") || return 0
  last=${mode#"${mode%?}"}
  case $last in 4 | 5 | 6 | 7) ;; *) return 0 ;; esac
  say "warning: $1 is readable by everyone (mode $mode) and holds secrets"
  if [ -O "$1" ]; then
    chmod 600 "$1"
    say "set $1 to mode 600"
  elif confirm "Run chmod 600 on $1 (not owned by you)?" n; then
    chmod 600 "$1"
  else
    say "left $1 as it is; run: chmod 600 $1"
  fi
}

fill_key() { # fill_key FILE KEY VALUE: only when the key is missing or empty
  [ -n "$(get_env "$1" "$2")" ] || [ -z "$3" ] || set_env "$1" "$2" "$3"
}

check_required() { # check_required FILE KEY...: stops, naming the keys still missing
  local f=$1 k missing=""
  shift
  for k in "$@"; do [ -n "$(get_env "$f" "$k")" ] || missing="$missing $k"; done
  [ -z "$missing" ] || die "$f is missing:$missing; set them and re-run (nothing was started)"
}

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

detect_os() {
  local kernel f
  kernel=$(uname -s)
  arch=$(uname -m)
  case $kernel in
    Linux)
      f=${PYN_INSTALL_OS_RELEASE:-/etc/os-release}
      os_id=linux
      if [ -r "$f" ]; then os_id=$(sed -n 's/^ID=//p' "$f" | head -n1 | tr -d '"'); fi
      if is_debian; then
        os_kind=debian
        say "host: Linux ($os_id); missing packages and Docker can be installed for you"
      else
        os_kind=linux
        say "host: Linux (${os_id:-unknown}); everything works when the tools are present, nothing is installed for you"
      fi
      ;;
    Darwin)
      os_kind=darwin
      say "host: macOS. Hosting a server natively is not supported here (macOS has no systemd)."
      say "Offered: Docker deployments if 'docker compose' works (Docker Desktop), and the pyn command on Apple silicon."
      ;;
    MINGW* | MSYS* | CYGWIN*) die "Windows shells ($kernel) are not supported; use WSL2 with Ubuntu, or a Linux host" ;;
    *) die "$kernel is not supported; releases cover Linux (x86_64, arm64) and macOS (arm64, the pyn command only)" ;;
  esac
}

# On macOS only Docker deployments and the CLI work; says why anything else is refused
darwin_guard() {
  [ "$os_kind" = darwin ] || return 0
  if [ "$shape" = cli ]; then
    [ "$arch" = arm64 ] || die "there is no pyn command build for Intel Macs yet (Apple silicon only)"
    return 0
  fi
  [ "$method" != native ] || die "hosting natively is not supported on macOS (no systemd); use --method docker with Docker Desktop, or --shape cli"
  have_tool compose || die "a server or web app on macOS needs Docker: install Docker Desktop, start it, and re-run (this script does not install it)"
}

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

# A local database is needed when no URL was given and the env file does not already name one
needs_local_db() {
  [ -z "$database_url" ] || return 1
  [ "$reuse" != fresh ] || return 0
  [ -z "$(get_env "$root$native_env" PYN_DATABASE_URL)" ]
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
        needs_local_db && t="$t postgres"
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
    if [ "$os_kind" = darwin ]; then
      echo "On macOS install Docker Desktop (https://docs.docker.com/desktop/setup/install/mac-install/), start it, then re-run."
    else
      echo "On Ubuntu or Debian: apt-get install curl tar coreutils openssl postgresql; Docker: https://docs.docker.com/engine/install/"
    fi
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
  elif [ "$os_kind" = darwin ]; then why="nothing is installed for you on macOS"
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

api_get() { # api_get PATH: saves the answer in $tmp/api.json and prints 200 or 404; stops on anything else
  local code
  code=$(curl -sSL -H 'Accept: application/vnd.github+json' -o "$tmp/api.json" -w '%{http_code}' \
    "https://api.github.com/repos/$repo/$1") || code=000
  case $code in
    200 | 404) echo "$code" ;;
    403 | 429) die "GitHub's release API refused the request (rate limit: 60 an hour without a token); try again later" ;;
    000) die "could not reach api.github.com; check the network connection" ;;
    *) die "GitHub's release API answered HTTP $code" ;;
  esac
}

api_tags() { # api_tags COUNT: tag names, newest first
  local code
  code=$(api_get "releases?per_page=$1")
  [ "$code" = 200 ] || die "GitHub's release API found no releases"
  grep -o '"tag_name": *"[^"]*"' "$tmp/api.json" | sed 's/.*"\([^"]*\)"$/\1/'
}

latest_stable() { # the newest non-pre-release tag; empty when none exists
  local code
  code=$(api_get releases/latest)
  [ "$code" != 200 ] || grep -o '"tag_name": *"[^"]*"' "$tmp/api.json" | head -n1 | sed 's/.*"\([^"]*\)"$/\1/'
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
  local stable newest default=pre choice stable_text
  if [ -n "$version_in" ]; then
    [ -z "$channel" ] || say "--version given; --channel is ignored"
    tag=$(match_tag "$version_in")
  else
    stable=$(latest_stable)
    newest=$(api_tags 1 | head -n1)
    [ -n "$newest" ] || die "GitHub lists no releases for $repo"
    [ -z "$stable" ] || default=stable
    case $channel in
      stable) [ -n "$stable" ] || die "no stable release exists yet (every release so far is a pre-release); use --channel pre" ;;
      pre) default=pre ;;
    esac
    choice=$channel
    if [ -z "$choice" ] && [ "$interactive" -eq 1 ]; then
      stable_text="Latest stable release (none yet: every release so far is a pre-release)"
      [ -z "$stable" ] || stable_text="Latest stable release ($stable)"
      while :; do
        choice=""
        menu choice "Which release?" "$default" --channel "stable|$stable_text" \
          "pre|Latest pre-release, the newest release of all ($newest)" "tag|A specific tag or version"
        [ "$choice" != stable ] || [ -n "$stable" ] || { echo "There is no stable release yet; pick 2 or 3."; continue; }
        break
      done
    fi
    [ -n "$choice" ] || { choice=$default; [ -n "$stable" ] || say "no stable release exists yet (every release so far is a pre-release); using the latest pre-release"; }
    case $choice in
      stable) tag=$stable ;;
      pre) tag=$newest ;;
      tag)
        ask version_in "Tag (v0.1.2-alpha) or version (0.1.2)" "" --version
        tag=$(match_tag "$version_in")
        ;;
    esac
  fi
  [[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9._-]+)?$ ]] || die "unexpected release tag '$tag'"
  ver=${tag#v}
  ver=${ver%%-*}
  say "resolved release: $tag (image tag v$ver)"
}

# Downloads and checksums

fetch() { curl -fsSL -o "$2" "$1" || die "download failed: $1"; }

platform_for() { # platform_for shape
  local os arch
  os=$(uname -s) arch=$(uname -m)
  case "$os $arch" in
    "Linux x86_64" | "Linux amd64") echo linux-x86_64 ;;
    "Linux aarch64" | "Linux arm64") echo linux-arm64 ;;
    "Darwin x86_64") die "there is no pyn command build for Intel Macs yet (Apple silicon only)" ;;
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
  until curl -fsS -o /dev/null "http://127.0.0.1:$1/healthz" 2>/dev/null; do
    i=$((i + 1))
    [ "$i" -lt "$max" ] || return 1
    sleep "${PYN_INSTALL_POLL:-1}"
  done
}

token_block() { grep -A3 "one-time setup token" | tail -n 4 || true; }

# Docker

choose_reuse() { # choose_reuse docker|native: asks what to do with an install that is already there
  if [ "$1" = docker ]; then
    menu reuse "An install already exists in $dir. What now?" refresh --reuse \
      "use|Use the files as they are" \
      "refresh|Refresh the Compose file from the release, keep my env" \
      "fresh|Start fresh (the old files are backed up first, never deleted)"
  else
    menu reuse "An install already exists (env file $native_env). What now?" refresh --reuse \
      "use|Use it as it is (binary, unit and env untouched)" \
      "refresh|Replace the binary with the release, keep my env" \
      "fresh|Start fresh (the env file is backed up first, never deleted)"
  fi
}

# Takes the address answers from an env file that is being reused
preload_from_env() {
  [ -f "$1" ] || return 0
  [ -n "$public_url" ] || public_url=$(get_env "$1" PYN_PUBLIC_URL)
  [ "$shape" != web ] || [ -n "$api_url" ] || api_url=$(get_env "$1" PYN_API_URL)
}

docker_prepare() {
  local dc="$dir/docker-compose.yml" have=""
  fetch "https://raw.githubusercontent.com/$repo/$tag/deploy/$compose" "$tmp/compose.yml"
  [ ! -f "$dc" ] || have="docker-compose.yml"
  [ ! -f "$dir/.env" ] || have="$have .env"
  if [ -n "$have" ]; then
    say "$dir already holds: $have"
    if [ -f "$dc" ] && ! cmp -s "$dc" "$tmp/compose.yml"; then
      echo "docker-compose.yml differs from the one in $tag (diff -u, first lines):"
      if command -v diff >/dev/null 2>&1; then diff -u "$dc" "$tmp/compose.yml" | head -n 20 || true; fi
    fi
    choose_reuse docker
  else
    reuse=""
  fi
  if [ -n "$env_file" ]; then preload_from_env "$env_file"
  elif [ "$reuse" != fresh ]; then preload_from_env "$dir/.env"
  fi
}

native_prepare() {
  if [ -f "$root$native_env" ]; then choose_reuse native; else reuse=""; fi
  if [ "$reuse" != fresh ]; then preload_from_env "$root$native_env"; fi
}

docker_flow() {
  local env_path="$dir/.env" dc="$dir/docker-compose.yml" envfile old new_env=1 first_run=1 bind="" base
  case $shape in
    full) envfile=.env.example ;;
    server) envfile=.env.server.example ;;
    web) envfile=.env.web.example ;;
  esac
  mkdir -p "$dir" 2>/dev/null || die "cannot create $dir; run with sudo or pass --dir"
  [ -w "$dir" ] || die "cannot write to $dir; run with sudo or pass --dir"
  base="https://raw.githubusercontent.com/$repo/$tag/deploy"
  [ ! -f "$env_path" ] || first_run=0

  if [ -f "$dc" ] && [ "$reuse" = use ]; then
    say "keeping the existing docker-compose.yml"
  else
    if [ -f "$dc" ] && ! cmp -s "$tmp/compose.yml" "$dc"; then backup_file "$dc"; fi
    cat "$tmp/compose.yml" >"$dc"
  fi

  if [ -n "$env_file" ]; then
    check_env_perms "$env_file"
    if [ -f "$env_path" ] && ! cmp -s "$env_file" "$env_path"; then backup_file "$env_path"; fi
    ( umask 077; cat "$env_file" >"$env_path" )
    new_env=0
    say "copied $env_file to $env_path"
  elif [ -f "$env_path" ] && [ "$reuse" != fresh ]; then
    new_env=0
    check_env_perms "$env_path"
  else
    if [ -f "$env_path" ]; then
      backup_file "$env_path"
      say "a new .env gets new secrets; an existing database volume keeps the old password, so copy POSTGRES_PASSWORD from the backup if you keep its data"
    fi
    if ! curl -fsSL -o "$tmp/env" "$base/$envfile" 2>/dev/null; then
      say "$envfile is not in release $tag; using the one from main"
      if ! curl -fsSL -o "$tmp/env" "https://raw.githubusercontent.com/$repo/main/deploy/$envfile" 2>/dev/null; then
        say "no $envfile found; starting from an empty .env"
        : >"$tmp/env"
      fi
    fi
    ( umask 077; cat "$tmp/env" >"$env_path" )
  fi
  chmod 600 "$env_path"

  if [ "$shape" != web ]; then
    fill_secret "$env_path" POSTGRES_PASSWORD 24
    fill_secret "$env_path" PYN_SECRET_KEY 32
    fill_key "$env_path" PYN_PUBLIC_URL "$public_url"
  else
    fill_key "$env_path" PYN_API_URL "$api_url"
  fi
  if [ "$new_env" -eq 1 ]; then
    if ! is_local_url "$public_url" && [ "${public_url#https://}" = "$public_url" ]; then bind=0.0.0.0; fi
    case $shape in
      full) set_env "$env_path" PYN_PUBLIC_URL "$public_url"; [ -z "$bind" ] || set_env "$env_path" PYN_WEB_PUBLISH "$bind:8080" ;;
      server) set_env "$env_path" PYN_PUBLIC_URL "$public_url"; [ -z "$bind" ] || set_env "$env_path" PYN_SERVER_PUBLISH "$bind:7878" ;;
      web) set_env "$env_path" PYN_API_URL "$api_url"; [ -z "$bind" ] || set_env "$env_path" PYN_WEB_PUBLISH "$bind:8080" ;;
    esac
  fi

  old=$(get_env "$env_path" PYN_VERSION)
  if [ -z "$old" ]; then
    set_env "$env_path" PYN_VERSION "v$ver"
    say "wrote $env_path (mode 600); PYN_VERSION v$ver"
  elif [ "$old" = "v$ver" ]; then
    say "keeping $env_path; PYN_VERSION is already v$ver"
  elif [ "$reuse" = use ]; then
    say "keeping $env_path as it is; PYN_VERSION stays $old"
  elif confirm "Move PYN_VERSION in $env_path from $old to v$ver?" y; then
    set_env "$env_path" PYN_VERSION "v$ver"
    say "upgrade: keeping $env_path, PYN_VERSION $old -> v$ver"
  else
    say "keeping $env_path; PYN_VERSION stays $old"
  fi

  if [ "$shape" = web ]; then
    check_required "$env_path" PYN_VERSION PYN_API_URL
  else
    check_required "$env_path" PYN_VERSION POSTGRES_PASSWORD PYN_SECRET_KEY PYN_PUBLIC_URL
  fi
  say "starting the stack"
  ( cd "$dir" && docker compose pull && docker compose up --detach --wait ) ||
    die "the stack did not come up; see: cd $dir && docker compose logs"
  say "up. Files are in $dir"
  if [ "$shape" = web ]; then
    say "add this web address to PYN_ALLOWED_ORIGINS on the server at $(get_env "$env_path" PYN_API_URL), then restart it: $public_url"
  elif [ "$first_run" -eq 1 ]; then
    say "one-time setup token (finish setup in the browser, or run: pyn setup <username>):"
    ( cd "$dir" && docker compose logs server 2>&1 ) | token_block
  fi
}

# Native

native_unit() {
  cat <<EOF
[Unit]
Description=pyn server
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
User=pyn
Group=pyn
EnvironmentFile=$native_env
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
  local env_real="$root$native_env" unit="$unit_dir/pyn.service" old="" src="" first_run=1 need_bin=1 addr port
  [ "$(id -u)" -eq 0 ] || die "the native install needs root: re-run with sudo"
  [ ! -f "$env_real" ] || first_run=0
  [ ! -f "$etc_dir/version" ] || old=$(cat "$etc_dir/version")
  if [ -x "$bin_dir/pyn-server" ] && [ "$reuse" = use ]; then
    need_bin=0
    say "using the installed pyn-server ${old:-} as it is"
  elif [ -x "$bin_dir/pyn-server" ] && [ -n "$old" ] && [ "$old" != "$tag" ] && ! confirm "Replace pyn-server $old with $tag?" y; then
    need_bin=0
    tag=$old
    say "keeping pyn-server $old"
  fi
  [ "$need_bin" -eq 0 ] || src=$(fetch_verified server)

  if ! id pyn >/dev/null 2>&1; then
    say "creating the system user pyn"
    useradd --system --user-group --home-dir /var/lib/pyn --no-create-home --shell /usr/sbin/nologin pyn
  fi
  mkdir -p "$bin_dir" "$state_dir" "$etc_dir" "$unit_dir" "$(dirname "$env_real")"
  chmod 0755 "$etc_dir"
  chmod 0750 "$state_dir"
  chown pyn:pyn "$state_dir"
  [ "$need_bin" -eq 0 ] || install_binary "$src/pyn-server" "$bin_dir/pyn-server"

  if [ -f "$env_real" ] && [ "$reuse" = fresh ]; then
    backup_file "$env_real"
    ( umask 077; : >"$env_real" )
  fi
  if [ -f "$env_real" ]; then
    check_env_perms "$env_real"
    say "keeping the existing $native_env; only missing settings are filled in"
  else
    ( umask 077; : >"$env_real" )
    chmod 600 "$env_real"
  fi
  addr=0.0.0.0:7878
  ! is_local_url "$public_url" || addr=127.0.0.1:7878
  fill_key "$env_real" PYN_ADDR "$addr"
  if [ -z "$(get_env "$env_real" PYN_DATABASE_URL)" ]; then
    [ -n "$database_url" ] || database_url=$(create_local_database)
    set_env "$env_real" PYN_DATABASE_URL "$database_url"
  fi
  fill_secret "$env_real" PYN_SECRET_KEY 32
  fill_key "$env_real" PYN_PUBLIC_URL "$public_url"
  fill_key "$env_real" PYN_DATA_DIR /var/lib/pyn
  check_required "$env_real" PYN_DATABASE_URL PYN_SECRET_KEY PYN_PUBLIC_URL PYN_DATA_DIR
  if [ "$first_run" -eq 1 ]; then say "wrote $env_real (mode 600) with generated secrets"; fi

  if [ ! -f "$unit" ] || ! grep -qxF "EnvironmentFile=$native_env" "$unit"; then
    if [ -f "$unit" ]; then backup_file "$unit"; fi
    native_unit >"$unit"
    say "wrote $unit"
  fi
  [ "$need_bin" -eq 0 ] || echo "$tag" >"$etc_dir/version"
  if [ "$need_bin" -eq 1 ] && [ -n "$old" ] && [ "$old" != "$tag" ]; then say "upgrade: $old -> $tag"; fi

  port=$(get_env "$env_real" PYN_ADDR)
  port=${port##*:}
  systemctl daemon-reload
  systemctl enable pyn
  systemctl restart pyn
  say "waiting for http://127.0.0.1:${port:-7878}/healthz"
  wait_health "${port:-7878}" || die "the server did not become healthy; see: journalctl -u pyn -n 50"
  say "up (pyn ${tag:-installed}, systemd unit pyn)"
  if [ "$first_run" -eq 1 ]; then
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
  needs_local_db || return 0
  menu choice "PostgreSQL for the server: how should the database be set up?" local --database-url \
    "local|Create a role and database on this host, with a generated password" \
    "url|Use an existing PostgreSQL I will give the address of"
  [ "$choice" != url ] || ask database_url "PostgreSQL address (postgres://user:password@host:5432/db)" "" --database-url
}

main() {
  local method_default=docker shape_default=full platform="" tag="" ver="" newshape="" o ok=0 compose=""
  local shape_opts=()
  open_terminal
  detect_os
  case $channel in "" | stable | pre) ;; *) die "--channel must be stable or pre" ;; esac
  case $reuse in "" | use | refresh | fresh) ;; *) die "--reuse must be use, refresh or fresh" ;; esac
  detect_existing

  if [ "$os_kind" = darwin ]; then
    if have_tool compose; then
      shape_opts=("full|Server and web app (Docker)" "server|Server only (Docker)" "web|Web app only (Docker), for a server running elsewhere")
    else
      say "Docker (docker compose) was not found: install Docker Desktop first to run a server or web app."
    fi
    if [ "$arch" = arm64 ]; then shape_opts+=("cli|The pyn command only"); else say "There is no pyn command build for Intel Macs yet."; fi
    [ "${#shape_opts[@]}" -gt 0 ] || die "nothing can be installed on this Mac yet: install Docker Desktop to run a server or web app (the pyn command has no Intel build)"
  else
    shape_opts=("full|Server and web app" "server|Server only" "web|Web app only, for a server running elsewhere" "cli|The pyn command only")
  fi
  for o in "${shape_opts[@]}"; do [ "${o%%|*}" != "$shape_default" ] || ok=1; done
  [ "$ok" -eq 1 ] || shape_default=${shape_opts[0]%%|*}

  menu shape "What do you want to install?" "$shape_default" --shape "${shape_opts[@]}"
  case $shape in full | server | web | cli) ;; *) die "--shape must be full, server, web or cli" ;; esac
  if [ "$shape" = cli ]; then
    method=native
  elif [ "$os_kind" = darwin ]; then
    [ -n "$method" ] || method=docker
  elif [ -z "$method" ] && ! command -v systemctl >/dev/null 2>&1; then
    say "native needs systemd, which this host does not have; using Docker"
    method=docker
  else
    menu method "How do you want to run it?" "$method_default" --method \
      "docker|Docker (release images and Compose files)" \
      "native|Native: the release pyn-server binary as a systemd service, with PostgreSQL"
  fi
  case $method in docker | native) ;; *) die "--method must be docker or native" ;; esac
  darwin_guard

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
  case $shape in
    full | server | web)
      case $shape in full) compose=docker-compose.yml ;; server) compose=docker-compose.server.yml ;; web) compose=docker-compose.web.yml ;; esac
      if [ "$method" = docker ]; then
        ask dir "Where should the deployment live (Compose and env files)?" /opt/pyn --dir
        ask_optional env_file "Existing env file to reuse" --env-file
        [ -z "$env_file" ] || [ -f "$env_file" ] || die "--env-file $env_file does not exist"
      else
        if [ -n "$env_file" ]; then native_env=$env_file; else
          ask native_env "Env file for the service (an existing one is reused, else it is created there)" /etc/pyn/pyn.env --env-file
        fi
        [[ $native_env =~ ^/[^[:space:]]+$ ]] || die "the env file must be an absolute path without spaces"
        native_prepare
        choose_database
      fi
      ;;
  esac

  echo
  check_tools
  echo
  resolve_version
  if [ "$method" = docker ] && [ "$shape" != cli ]; then docker_prepare; fi

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
  say "plan: $shape via $method, release $tag (image v$ver)${reuse:+, existing install: $reuse}"
  confirm "Proceed?" y || die "stopped; nothing was changed"

  case $shape in
    cli) cli_flow ;;
    *) if [ "$method" = docker ]; then docker_flow; else native_flow; fi ;;
  esac
  say "done"
}

main
