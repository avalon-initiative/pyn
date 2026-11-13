#!/usr/bin/env bash
# Tests install.sh against a PATH of fakes (curl serving fixtures, docker, apt-get, systemctl, su, ...) that log their calls. No network, no root.
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

real_bash=$(command -v bash)
fix="$work/fix"
tags="v0.1.2-alpha v0.1.1-alpha v0.1.0-alpha"
platforms="linux-x86_64 linux-arm64 macos-arm64"

# Fixtures: release list, archives with real checksums (v0.1.0-alpha has wrong ones), raw deploy files.
mkdir -p "$fix"
{
  echo '['
  sep=''
  for t in $tags; do printf '%s  {\n    "tag_name": "%s",\n    "prerelease": true\n  }' "$sep" "$t"; sep=$',\n'; done
  echo; echo ']'
} >"$fix/releases.json"
printf '[\n  {\n    "tag_name": "v0.1.2-alpha"\n  }\n]\n' >"$fix/releases1.json"
for t in $tags v1.0.0; do
  d="$fix/dl/$t"; mkdir -p "$d"
  for p in $platforms; do
    for k in server cli; do
      [ "$k$p" != servermacos-arm64 ] || continue
      n="pyn-$k-$t-$p"; mkdir -p "$work/pack/$n"
      printf '#!/bin/sh\necho %s %s\n' "$k" "$t" >"$work/pack/$n/$([ "$k" = cli ] && echo pyn || echo pyn-server)"
      chmod +x "$work/pack/$n"/pyn*; echo license >"$work/pack/$n/LICENSE"
      tar -czf "$d/$n.tar.gz" -C "$work/pack" "$n"
    done
  done
  (cd "$d" && sha256sum pyn-*.tar.gz >SHA256SUMS)
done
sed -i 's/^[0-9a-f]\{64\}/0000000000000000000000000000000000000000000000000000000000000000/' "$fix/dl/v0.1.0-alpha/SHA256SUMS"
for ref in v1.0.0 v0.1.2-alpha v0.1.1-alpha main; do
  mkdir -p "$fix/raw/$ref"
  for f in docker-compose.yml docker-compose.server.yml docker-compose.web.yml; do
    printf '# %s at %s\nservices:\n  server:\n    image: ghcr.io/avalon-initiative/pyn-core:x\n%s' "$f" "$ref" \
      "$([ "$f" != docker-compose.server.yml ] && echo '  web:
    image: ghcr.io/avalon-initiative/pyn-web:x')" >"$fix/raw/$ref/$f"
  done
done
printf '# web only\nservices:\n  web:\n    image: ghcr.io/avalon-initiative/pyn-web:x\n' >"$fix/raw/main/docker-compose.web.yml"
cp "$fix/raw/main/docker-compose.web.yml" "$fix/raw/v0.1.2-alpha/"
cp "$fix/raw/main/docker-compose.web.yml" "$fix/raw/v0.1.1-alpha/"
cp "$fix/raw/main/docker-compose.web.yml" "$fix/raw/v1.0.0/"
cat >"$work/env.example" <<'EOF'
PYN_VERSION=
POSTGRES_PASSWORD=
PYN_SECRET_KEY=
PYN_PUBLIC_URL=http://localhost:8080
# PYN_WEB_PUBLISH=127.0.0.1:8080
# PYN_SERVER_PUBLISH=127.0.0.1:7878
EOF
cp "$work/env.example" "$fix/raw/main/.env.example"
cp "$work/env.example" "$fix/raw/main/.env.server.example"
printf 'PYN_VERSION=\nPYN_API_URL=\n# PYN_WEB_PUBLISH=127.0.0.1:8080\n' >"$fix/raw/main/.env.web.example"
cp "$work/env.example" "$fix/raw/v0.1.2-alpha/.env.example"
cp "$work/env.example" "$fix/raw/v0.1.1-alpha/.env.example"
cp "$work/env.example" "$fix/raw/v1.0.0/.env.example"
printf '[\n  {\n    "tag_name": "v1.0.0"\n  }\n]\n' >"$fix/latest.json"
printf 'ID=ubuntu\nID_LIKE=debian\n' >"$fix/os-debian"
printf 'ID=fedora\n' >"$fix/os-fedora"
printf '#!/bin/sh\necho docker-installer >>"$LOG"\ncp "$FIX/stub-docker" "$BIN/docker"\n' >"$fix/get-docker.sh"

token_block() { printf '%s\n' '----- pyn one-time setup token -----' 'TOKEN-ABC-123' '------------------------------------' 'finish setup with the CLI: pyn setup <username>'; }
{ declare -f token_block; echo 'token_block'; } >"$fix/token.sh"

cat >"$fix/stub-docker" <<'EOF'
#!/bin/bash
echo "docker[$PWD] $*" >>"$LOG"
case "$*" in
  "compose version") [ -z "${FAKE_NO_COMPOSE:-}" ] ;;
  "compose logs server") bash "$FIX/token.sh" ;;
  *) exit "${FAKE_DOCKER_EXIT:-0}" ;;
esac
EOF
cat >"$fix/stub-curl" <<'EOF'
#!/bin/bash
echo "curl $*" >>"$LOG"
out="" url="" wfmt="" fail=0
while [ $# -gt 0 ]; do
  case $1 in
    -o) out=$2; shift ;;
    -w) wfmt=$2; shift ;;
    -H) shift ;;
    http*) url=$1 ;;
    -*f*) fail=1 ;;
  esac
  shift
done
if [ -n "${FAKE_NET_DOWN:-}" ]; then [ -z "$wfmt" ] || printf 000; exit 6; fi
code=200
case $url in
  *api.github.com*releases/latest) f=$FIX/latest.json; [ -n "${FAKE_STABLE:-}" ] || f=/nonexistent; code=${FAKE_API_CODE:-200} ;;
  *api.github.com*per_page=1) f=$FIX/releases1.json; code=${FAKE_API_CODE:-200} ;;
  *api.github.com*per_page=100) f=$FIX/releases.json; code=${FAKE_API_CODE:-200} ;;
  *github.com/*/releases/download/*) f=$FIX/dl/$(basename "$(dirname "$url")")/$(basename "$url") ;;
  *raw.githubusercontent.com/*/deploy/*) ref=${url#*githubusercontent.com/avalon-initiative/pyn/}; ref=${ref%%/*}; f=$FIX/raw/$ref/${url##*/} ;;
  *get.docker.com*) f=$FIX/get-docker.sh ;;
  *healthz*) [ -z "${FAKE_HEALTH_FAIL:-}" ]; exit ;;
  *) f=/nonexistent ;;
esac
[ -f "$f" ] || code=404
if [ "$code" = 200 ]; then
  if [ -n "$out" ]; then cp "$f" "$out"; else cat "$f"; fi
elif [ "$fail" = 1 ] && [ -z "$wfmt" ]; then exit 22
fi
[ -z "$wfmt" ] || printf '%s' "$code"
exit 0
EOF
cat >"$fix/stub-apt-get" <<'EOF'
#!/bin/bash
echo "apt-get $*" >>"$LOG"
for a in "$@"; do
  case $a in
    openssl) ln -sf "$REAL_OPENSSL" "$BIN/openssl" ;;
    postgresql) cp "$FIX/stub-psql" "$BIN/psql"; touch "$BIN/.pg-active" ;;
  esac
done
EOF
printf '#!/bin/bash\necho "psql $*" >>"$LOG"\n' >"$fix/stub-psql"
cat >"$fix/stub-systemctl" <<'EOF'
#!/bin/bash
echo "systemctl $*" >>"$LOG"
case "$*" in
  "is-active --quiet postgresql") [ -f "$BIN/.pg-active" ] || [ -n "${FAKE_PG_ACTIVE:-}" ] ;;
esac
EOF
cat >"$fix/stub-su" <<'EOF'
#!/bin/bash
echo "su $*" >>"$LOG"
sql=$(cat)
printf '%s\n' "$sql" >>"$LOG.sql"
case $sql in
  *pg_roles*) [ -z "${FAKE_PG_ROLE_EXISTS:-}" ] || echo 1 ;;
esac
EOF
cat >"$fix/stub-journalctl" <<'EOF'
#!/bin/bash
echo "journalctl $*" >>"$LOG"
bash "$FIX/token.sh"
EOF
cat >"$fix/stub-id" <<'EOF'
#!/bin/bash
case "$*" in
  -u) echo "${FAKE_UID:-0}" ;;
  pyn) [ -n "${FAKE_PYN_EXISTS:-}" ] ;;
esac
EOF
cat >"$fix/stub-uname" <<'EOF'
#!/bin/bash
case "$1" in -s) echo "${FAKE_UNAME_S:-Linux}" ;; -m) echo "${FAKE_UNAME_M:-x86_64}" ;; esac
EOF
printf '#!/bin/bash\n[ "$1" = -I ] && echo "10.0.0.5 " || echo testhost\n' >"$fix/stub-hostname"
for n in useradd chown sleep; do printf '#!/bin/bash\necho "%s $*" >>"$LOG"\n' "$n" >"$fix/stub-$n"; done
chmod +x "$fix"/stub-*

real_tools="diff gzip awk sed grep tar sha256sum openssl mktemp mv cp cat install mkdir rm head tail tr sort dirname basename chmod wc cut ln touch cmp date ls stat bash sh env"

# mkbin DIR [omit...]: a PATH of real basics plus the fakes, minus the named tools.
mkbin() {
  local b=$1 t o skip; shift
  rm -rf "$b"; mkdir -p "$b"
  for t in $real_tools; do ln -s "$(command -v "$t")" "$b/$t"; done
  for t in curl docker apt-get systemctl su journalctl id uname hostname useradd chown sleep psql; do
    skip=0; for o in "$@"; do [ "$o" != "$t" ] || skip=1; done
    [ "$skip" -eq 1 ] || { cp "$fix/stub-$t" "$b/$t"; chmod +x "$b/$t"; }
  done
  for o in "$@"; do rm -f "$b/$o"; done
}

# case_start NAME [omit...]: fresh root, log and PATH for one scenario.
case_start() {
  c="$work/$1"; shift
  rm -rf "$c"; mkdir -p "$c/root" "$c/home"
  mkbin "$c/bin" "$@"
  : >"$c/log"
  tty_file=""
}
# run_install [flags...]: environment is cleared; FAKE_* in extra_env pass through. Sets out, err, rc.
extra_env=()
run_install() {
  out="$c/out"; err="$c/err"
  local ttyenv=()
  [ -z "$tty_file" ] || ttyenv=(PYN_INSTALL_TTY="$tty_file")
  rc=0
  env -i PATH="$c/bin" HOME="$c/home" LOG="$c/log" FIX="$fix" BIN="$c/bin" REAL_OPENSSL="$(command -v openssl)" \
    PYN_INSTALL_ROOT="$c/root" PYN_INSTALL_OS_RELEASE="$fix/os-debian" PYN_INSTALL_POLL=0 PYN_INSTALL_HEALTH_TRIES=3 \
    ${ttyenv[@]+"${ttyenv[@]}"} ${extra_env[@]+"${extra_env[@]}"} \
    "$real_bash" "$src/scripts/install.sh" "$@" </dev/null >"$out" 2>"$err" || rc=$?
}
ok_rc() { [ "$rc" -eq 0 ]; }
bad_rc() { [ "$rc" -ne 0 ]; }
mode_of() { stat -c %a "$1"; }

# Tool detection
case_start t1 docker
extra_env=()
run_install --shape full --method docker --no-install
expect "--no-install reports missing tools and exits non-zero" bad_rc
expect "--no-install lists docker" has "$err" "docker"
expect "--no-install says why" has "$err" "--no-install"
expect "--no-install installs nothing" lacks "$c/log" "apt-get"

case_start t2 docker openssl
run_install --dir "$c/pyn" --shape full --method docker --yes
expect "--yes installs missing tools and finishes" ok_rc
expect "apt-get installs openssl" has "$c/log" "apt-get install -y -qq openssl"
expect "Docker comes from get.docker.com" has "$c/log" "docker-installer"
expect "the stack is started after the install" has "$c/log" "compose up --detach --wait"

case_start t3 docker
extra_env=(PYN_INSTALL_OS_RELEASE="$fix/os-fedora")
run_install --shape full --method docker --yes
extra_env=()
expect "non-Debian host reports instead of installing" bad_rc
expect "non-Debian message" has "$c/err" "not Ubuntu or Debian"
expect "non-Debian runs no installer" lacks "$c/log" "get.docker.com"

case_start t4 docker
run_install --shape full --method docker
expect "no terminal and no --yes does not install, and does not hang" bad_rc
expect "asks for --yes" has "$err" "pass --yes"

case_start t5 systemctl
run_install --shape server --method native --yes --database-url postgres://u:p@h:5432/d
expect "native without systemd stops" bad_rc
expect "native without systemd says so" has "$err" "systemd"

# Versions
case_start v1
run_install --dir "$c/pyn" --shape full --yes
expect "default version is the newest release" ok_rc
expect "newest: image tag drops the title suffix" has "$c/pyn/.env" "PYN_VERSION=v0.1.2"
expect "newest: Compose file comes from the newest tag" has "$c/log" "raw.githubusercontent.com/avalon-initiative/pyn/v0.1.2-alpha/deploy/docker-compose.yml"
expect "newest: asks the single-release endpoint" has "$c/log" "per_page=1"

# Docker writes under --dir, so give every Docker case its own directory.
dock() { # dock NAME flags...: run a Docker install into $c/pyn
  local n=$1; shift
  case_start "$n"
  run_install --dir "$c/pyn" --yes "$@"
}

dock v2 --shape full --version v0.1.1-alpha
expect "full tag is accepted" ok_rc
expect "full tag: image tag" has "$c/pyn/.env" "PYN_VERSION=v0.1.1"
expect "full tag: file from that tag" has "$c/log" "/v0.1.1-alpha/deploy/docker-compose.yml"
dock v3 --shape full --version 0.1.1
expect "bare version is accepted" ok_rc
expect "bare version maps to the release tag" has "$c/log" "/v0.1.1-alpha/deploy/docker-compose.yml"
dock v3b --shape full --version v0.1.1
expect "v-prefixed bare version maps to the release tag" has "$c/log" "/v0.1.1-alpha/deploy/docker-compose.yml"
dock v4 --shape full --version 9.9.9
expect "unknown version fails" bad_rc
expect "unknown version says so" has "$c/err" "no release matches"
expect "unknown version starts nothing" lacks "$c/log" "compose up"
dock v5 --shape full --version latest
expect "a non-version is rejected" bad_rc
expect "a non-version says so" has "$c/err" "not a version"
case_start v6; extra_env=(FAKE_API_CODE=403); run_install --dir "$c/pyn" --yes --shape full
expect "rate limit is reported" has "$c/err" "rate limit"
case_start v7; extra_env=(FAKE_NET_DOWN=1); run_install --dir "$c/pyn" --yes --shape full
expect "no network is reported" has "$c/err" "could not reach"
extra_env=()

# Prompts through PYN_INSTALL_TTY
case_start p1
tty_file="$c/answers"
printf '2\n1\n%s\n\n\n\n\n' "$c/pyn" >"$tty_file"
run_install
expect "prompted run (server, docker, defaults) succeeds" ok_rc
expect "shape menu is shown" has "$out" "What do you want to install?"
expect "method menu is shown" has "$out" "How do you want to run it?"
expect "release menu defaults to the pre-release while no stable exists" has "$out" "(v0.1.2-alpha) (default)"
expect "release menu says there is no stable release" has "$out" "none yet: every release so far is a pre-release"
expect "the resolved tag is shown" has "$out" "resolved release: v0.1.2-alpha"
expect "public URL default is this host on 7878 for a server" has "$c/pyn/.env" "PYN_PUBLIC_URL=http://10.0.0.5:7878"
expect "shape answer 2 selected the server file" has "$c/pyn/docker-compose.yml" "docker-compose.server.yml"
expect "server publish is opened for a non-local address" has "$c/pyn/.env" "PYN_SERVER_PUBLISH=0.0.0.0:7878"

case_start p2
tty_file="$c/answers"
printf 'full\ndocker\n%s\n\n3\n0.1.1\nhttps://pyn.example.com\ny\n' "$c/pyn" >"$tty_file"
run_install
expect "typed names, bare version and URL are taken" has "$c/pyn/.env" "PYN_PUBLIC_URL=https://pyn.example.com"
expect "typed bare version selects its tag" has "$c/pyn/.env" "PYN_VERSION=v0.1.1"
expect "an https address keeps the port on loopback" lacks "$c/pyn/.env" "PYN_WEB_PUBLISH=0.0.0.0"

case_start p3
tty_file="$c/answers"
printf 'full\ndocker\n%s\n\n3\nv9.9.9\n' "$c/pyn" >"$tty_file"
run_install
expect "an unknown version typed at the prompt fails" bad_rc

case_start p4
tty_file="$c/answers"
printf 'full\n'  >"$tty_file"
run_install --dir "$c/pyn"
expect "input ending mid-prompt fails instead of hanging" bad_rc

# Docker files
dock d1 --shape full
expect "Docker install succeeds" ok_rc
expect ".env is mode 600" test "$(mode_of "$c/pyn/.env")" = 600
pw=$(sed -n 's/^POSTGRES_PASSWORD=//p' "$c/pyn/.env"); sk=$(sed -n 's/^PYN_SECRET_KEY=//p' "$c/pyn/.env")
expect "POSTGRES_PASSWORD generated (48 hex)" test "${#pw}" -eq 48
expect "PYN_SECRET_KEY generated (64 hex)" test "${#sk}" -eq 64
expect "no empty setting is left" bash -c '! grep -qE "^[A-Z_]+=$" "$0"' "$c/pyn/.env"
expect "web publish uncommented for a LAN address" has "$c/pyn/.env" "PYN_WEB_PUBLISH=0.0.0.0:8080"
expect "pull then up with --wait, in the install dir" has "$c/log" "docker[$c/pyn] compose up --detach --wait"
expect "setup token printed" has "$c/out" "TOKEN-ABC-123"
expect "secrets are not printed" bash -c '! grep -qF -e "$1" -e "$2" "$3" "$4"' _ "$pw" "$sk" "$c/out" "$c/err"

dock d2 --shape server
expect "server shape uses the server Compose file" has "$c/pyn/docker-compose.yml" "docker-compose.server.yml"
expect "env example missing at the tag falls back to main" has "$c/log" "raw.githubusercontent.com/avalon-initiative/pyn/main/deploy/.env.server.example"
expect "fallback is announced" has "$c/out" "using the one from main"

dock d3 --shape web --api-url https://pyn.example.com/
expect "web shape succeeds" ok_rc
expect "web shape sets PYN_API_URL without the trailing slash" has "$c/pyn/.env" "PYN_API_URL=https://pyn.example.com"
expect "web shape makes no secrets" lacks "$c/pyn/.env" "PYN_SECRET_KEY"
expect "web shape names PYN_ALLOWED_ORIGINS" has "$c/out" "PYN_ALLOWED_ORIGINS"
expect "web shape prints no setup token" lacks "$c/out" "TOKEN-ABC-123"

dock d4 --shape web
expect "--yes without a required answer fails" bad_rc
expect "and names the flag" has "$c/err" "--api-url"
dock d5 --shape web --api-url "https://x.example.com/path"
expect "an API address with a path is rejected" bad_rc

# Re-run
dock r1 --shape full --version v0.1.1-alpha
echo "PYN_REGISTRATION=closed" >>"$c/pyn/.env"
pw1=$(sed -n 's/^POSTGRES_PASSWORD=//p' "$c/pyn/.env"); sk1=$(sed -n 's/^PYN_SECRET_KEY=//p' "$c/pyn/.env")
: >"$c/log"
run_install --dir "$c/pyn" --yes --shape full --version v0.1.2-alpha
expect "re-run succeeds" ok_rc
expect "re-run moves only the version" has "$c/pyn/.env" "PYN_VERSION=v0.1.2"
expect "re-run keeps the password" has "$c/pyn/.env" "POSTGRES_PASSWORD=$pw1"
expect "re-run keeps the secret key" has "$c/pyn/.env" "PYN_SECRET_KEY=$sk1"
expect "re-run keeps custom settings" has "$c/pyn/.env" "PYN_REGISTRATION=closed"
expect "re-run says what changed" has "$c/out" "PYN_VERSION v0.1.1 -> v0.1.2"
expect "re-run does not print the setup token again" lacks "$c/out" "TOKEN-ABC-123"
expect "re-run does not refetch the env example" lacks "$c/log" ".env.example"
expect "re-run keeps mode 600" test "$(mode_of "$c/pyn/.env")" = 600

# Native
nat() { # nat NAME flags...: native server into $c/root
  local n=$1; shift
  case_start "$n"
  extra_env=(FAKE_PG_ACTIVE=1)
  run_install --yes --shape server --method native "$@"
}
nat n1
expect "native install succeeds" ok_rc
unit="$c/root/etc/systemd/system/pyn.service"
for line in User=pyn NoNewPrivileges=true ProtectSystem=strict StateDirectory=pyn EnvironmentFile=/etc/pyn/pyn.env \
  ExecStart=/usr/local/bin/pyn-server CapabilityBoundingSet= PrivateTmp=true ProtectHome=true; do
  expect "unit has $line" has "$unit" "$line"
done
envp="$c/root/etc/pyn/pyn.env"
expect "pyn.env is mode 600" test "$(mode_of "$envp")" = 600
for k in PYN_ADDR PYN_DATABASE_URL PYN_SECRET_KEY PYN_PUBLIC_URL PYN_DATA_DIR; do
  expect "pyn.env sets $k" bash -c 'grep -qE "^$1=.+" "$2"' _ "$k" "$envp"
done
expect "pyn.env database URL has a generated password" bash -c 'grep -qE "^PYN_DATABASE_URL=postgres://pyn:[0-9a-f]{48}@127.0.0.1:5432/pyn$" "$1"' _ "$envp"
expect "binary installed and executable" test -x "$c/root/usr/local/bin/pyn-server"
expect "system user created" has "$c/log" "useradd --system"
expect "role and database created through su postgres" has "$c/log.sql" "create role pyn login password"
expect "the password is not on a command line" bash -c '! grep -E "^(su|psql)" "$0" | grep -qE "[0-9a-f]{48}"' "$c/log"
expect "service enabled and restarted" has "$c/log" "systemctl restart pyn"
expect "health checked" has "$c/log" "127.0.0.1:7878/healthz"
expect "setup token read from the journal" has "$c/out" "TOKEN-ABC-123"
expect "native checksum verified" has "$c/err" "checksum verified"
expect "default native URL uses :7878" has "$envp" "PYN_PUBLIC_URL=http://10.0.0.5:7878"
dbpw=$(sed -n 's/^PYN_DATABASE_URL=postgres:\/\/pyn:\([0-9a-f]*\)@.*/\1/p' "$envp")
expect "native secrets are not printed" bash -c '! grep -qF -e "$1" "$2" "$3"' _ "$dbpw" "$c/out" "$c/err"

nat n2 --database-url postgres://u:p@db.example.com:5432/pyn
expect "external database URL is used" has "$c/root/etc/pyn/pyn.env" "PYN_DATABASE_URL=postgres://u:p@db.example.com:5432/pyn"
expect "external database creates no role" bash -c '[ ! -f "$0" ]' "$c/log.sql"
nat n3 --database-url mysql://x
expect "a non-postgres URL is rejected" bad_rc
case_start n4 psql
extra_env=(); run_install --yes --shape server --method native
expect "missing PostgreSQL is installed with --yes" has "$c/log" "apt-get install -y -qq postgresql"
expect "and then the database is created" has "$c/log.sql" "create database pyn owner pyn"
case_start n5; extra_env=(FAKE_PG_ACTIVE=1 FAKE_PG_ROLE_EXISTS=1)
run_install --yes --shape server --method native
expect "an existing role is never dropped or reused silently" bad_rc
expect "and the error points at --database-url" has "$c/err" "--database-url"

# Native re-run
nat u1
cp "$c/root/etc/pyn/pyn.env" "$c/env.before"
: >"$c/log"
extra_env=(FAKE_PG_ACTIVE=1 FAKE_PYN_EXISTS=1)
run_install --yes --shape server --method native --version v0.1.1-alpha
expect "native re-run succeeds" ok_rc
expect "native re-run keeps pyn.env byte for byte" cmp -s "$c/env.before" "$c/root/etc/pyn/pyn.env"
expect "native re-run records the new version" has "$c/root/etc/pyn/version" "v0.1.1-alpha"
expect "native re-run says what changed" has "$c/out" "v0.1.2-alpha -> v0.1.1-alpha"
expect "native re-run restarts the service" has "$c/log" "systemctl restart pyn"
expect "native re-run creates no user or role" bash -c '! grep -qE "useradd|^su" "$0"' "$c/log"
expect "native re-run does not print the setup token" lacks "$c/out" "TOKEN-ABC-123"

# Checksums and platforms
case_start s1; extra_env=(FAKE_PG_ACTIVE=1)
run_install --yes --shape server --method native --version v0.1.0-alpha --database-url postgres://u:p@h/d
expect "checksum mismatch aborts" bad_rc
expect "mismatch is named" has "$c/err" "checksum mismatch"
expect "mismatch installs no binary" bash -c '[ ! -e "$0" ]' "$c/root/usr/local/bin/pyn-server"
expect "mismatch touches no service" lacks "$c/log" "systemctl daemon-reload"
expect "mismatch creates no user" lacks "$c/log" "useradd"

plat() { # plat NAME UNAME_S UNAME_M
  case_start "$1"; extra_env=(FAKE_PG_ACTIVE=1 "FAKE_UNAME_S=$2" "FAKE_UNAME_M=$3")
  run_install --yes --shape server --method native --database-url postgres://u:p@h/d
}
plat a1 Linux x86_64
expect "x86_64 maps to linux-x86_64" has "$c/log" "pyn-server-v0.1.2-alpha-linux-x86_64.tar.gz"
plat a2 Linux aarch64
expect "aarch64 maps to linux-arm64" has "$c/log" "pyn-server-v0.1.2-alpha-linux-arm64.tar.gz"
plat a3 Linux riscv64
expect "an unsupported architecture fails" bad_rc
expect "and says so" has "$c/err" "no release binaries"
plat a4 Darwin arm64
expect "the native server is refused on macOS" has "$c/err" "not supported on macOS"

# Web with native
case_start w1; extra_env=()
run_install --yes --shape web --method native
expect "web with native fails under --yes" bad_rc
expect "and explains Docker-only" has "$out" "Docker-only"
case_start w2; tty_file="$c/answers"
printf 'full\nnative\nserver\n\nlocal\n\n\n\n' >"$tty_file"
extra_env=(FAKE_PG_ACTIVE=1); run_install
expect "full with native offers server-only, interactively" ok_rc
expect "full with native explains Docker-only" has "$out" "Docker-only"
expect "and installs the native server" test -x "$c/root/usr/local/bin/pyn-server"
case_start w3; tty_file="$c/answers"
printf 'web\nnative\n\n%s\n\n\nhttps://pyn.example.com\n\n\n' "$c/pyn" >"$tty_file"
run_install
expect "web with native can switch to Docker interactively" has "$c/pyn/.env" "PYN_API_URL=https://pyn.example.com"

# CLI
case_start c1; extra_env=()
run_install --yes --shape cli --api-url http://pyn.example.com:7878
expect "CLI install as root succeeds" ok_rc
expect "CLI goes to /usr/local/bin for root" test -x "$c/root/usr/local/bin/pyn"
expect "CLI checksum verified" has "$c/err" "checksum verified"
expect "CLI prints the config step" has "$c/out" "pyn config set --global server http://pyn.example.com:7878"
expect "CLI prints the login step" has "$c/out" "pyn login"
case_start c2; extra_env=(FAKE_UID=1000 FAKE_UNAME_S=Darwin FAKE_UNAME_M=arm64)
run_install --yes --shape cli
expect "CLI on macOS arm64 succeeds" ok_rc
expect "CLI uses the macos-arm64 archive" has "$c/log" "pyn-cli-v0.1.2-alpha-macos-arm64.tar.gz"
expect "CLI as non-root goes to ~/.local/bin" test -x "$c/home/.local/bin/pyn"
case_start c3; extra_env=()
run_install --yes --shape cli --version v0.1.0-alpha
expect "CLI checksum mismatch aborts" bad_rc

# Host detection
mac() { # mac NAME ARCH [omit...]: a macOS host
  local n=$1 a=$2; shift 2
  case_start "$n" "$@"
  extra_env=(FAKE_UNAME_S=Darwin "FAKE_UNAME_M=$a")
}
mac m1 arm64
tty_file="$c/answers"; printf 'cli\n\n\n' >"$tty_file"
run_install
expect "macOS: CLI install through the menu succeeds" ok_rc
expect "macOS: says native hosting is not supported" has "$out" "Hosting a server natively is not supported"
expect "macOS: offers Docker shapes when docker compose works" has "$out" "Server and web app (Docker)"
expect "macOS: offers the CLI on Apple silicon" has "$out" "The pyn command only"
expect "macOS: no native/Docker method menu" lacks "$out" "How do you want to run it?"
expect "macOS: uses the macos-arm64 archive" has "$c/log" "pyn-cli-v0.1.2-alpha-macos-arm64.tar.gz"
expect "macOS: never touches apt or systemd" bash -c '! grep -qE "apt-get|systemctl|get.docker.com" "$0"' "$c/log"
mac m2 arm64 docker
tty_file="$c/answers"; printf '\n\n\n' >"$tty_file"
run_install
expect "macOS without Docker: CLI only is offered and installed" ok_rc
expect "macOS without Docker: tells to install Docker Desktop first" has "$out" "install Docker Desktop first"
expect "macOS without Docker: the menu does not offer a server" lacks "$out" "Server and web app"
mac m3 x86_64
tty_file="$c/answers"; printf '1\n%s\n\n\n\n\n' "$c/pyn" >"$tty_file"
run_install
expect "Intel Mac: Docker shapes still work" ok_rc
expect "Intel Mac: says there is no CLI build" has "$out" "no pyn command build for Intel Macs"
expect "Intel Mac: the menu does not offer the CLI" lacks "$out" "The pyn command only"
mac m4 x86_64
run_install --yes --shape cli
expect "Intel Mac: --shape cli is refused clearly" bad_rc
expect "Intel Mac: the reason is stated" has "$err" "Intel Macs"
mac m5 x86_64 docker
run_install --yes
expect "Intel Mac without Docker has nothing to install" bad_rc
expect "Intel Mac without Docker says so" has "$err" "nothing can be installed"
mac m6 arm64
run_install --yes --shape server --method native
expect "macOS: --method native is refused" bad_rc
expect "macOS: native refusal says why" has "$err" "not supported on macOS"
mac m7 arm64 docker
run_install --yes --shape server
expect "macOS: a server without Docker is refused" bad_rc
expect "macOS: asks for Docker Desktop" has "$err" "Docker Desktop"
mac m8 arm64
run_install --yes --shape full --dir "$c/pyn"
expect "macOS: a Docker deployment works" ok_rc
expect "macOS: Docker deployment runs compose up" has "$c/log" "compose up --detach --wait"
expect "macOS: no installer ran" lacks "$c/log" "get.docker.com"
case_start o1; extra_env=(FAKE_UNAME_S=FreeBSD)
run_install --yes
expect "FreeBSD is named and refused" has "$err" "FreeBSD is not supported"
case_start o2; extra_env=(FAKE_UNAME_S=MINGW64_NT-10.0)
run_install --yes
expect "a Windows shell is named and refused" has "$err" "Windows shells"
case_start o3; extra_env=(PYN_INSTALL_OS_RELEASE="$fix/os-fedora")
run_install --yes --shape full --dir "$c/pyn"
expect "other Linux works when the tools exist" ok_rc
expect "other Linux says nothing is installed for you" has "$out" "host: Linux (fedora)"
case_start o4 systemctl; tty_file="$c/answers"
printf 'full\n%s\n\n\n\n\n' "$c/pyn" >"$tty_file"
run_install
expect "Linux without systemd skips the method menu" ok_rc
expect "and says why" has "$out" "native needs systemd"
case_start o5 systemctl; extra_env=()
run_install --yes --shape server --method native
expect "an explicit native choice without systemd still stops" bad_rc

# Release channels
chan() { case_start "$1"; extra_env=(); }
chan h1; extra_env=(FAKE_STABLE=1); tty_file="$c/answers"
printf 'full\ndocker\n%s\n\n\n\n\n' "$c/pyn" >"$tty_file"
run_install
expect "stable exists: it is the menu default" has "$out" "Latest stable release (v1.0.0) (default)"
expect "stable exists: its tag is resolved" has "$out" "resolved release: v1.0.0"
expect "stable exists: image tag v1.0.0" has "$c/pyn/.env" "PYN_VERSION=v1.0.0"
chan h2; extra_env=(FAKE_STABLE=1); tty_file="$c/answers"
printf 'full\ndocker\n%s\n\n2\n\n\n' "$c/pyn" >"$tty_file"
run_install
expect "choice 2 is the latest pre-release" has "$c/pyn/.env" "PYN_VERSION=v0.1.2"
chan h3; tty_file="$c/answers"
printf 'full\ndocker\n%s\n\n1\n2\n\n\n' "$c/pyn" >"$tty_file"
run_install
expect "no stable: choosing stable asks again" has "$out" "There is no stable release yet"
expect "no stable: then the pre-release is used" has "$c/pyn/.env" "PYN_VERSION=v0.1.2"
chan h4; extra_env=(FAKE_STABLE=1)
run_install --yes --dir "$c/pyn"
expect "--yes picks stable when it exists" has "$c/pyn/.env" "PYN_VERSION=v1.0.0"
chan h5
run_install --yes --dir "$c/pyn"
expect "--yes falls back to the pre-release and says so" has "$out" "no stable release exists yet"
chan h6
run_install --yes --channel stable --dir "$c/pyn"
expect "--channel stable fails when no stable exists" bad_rc
expect "and explains" has "$err" "every release so far is a pre-release"
chan h7; extra_env=(FAKE_STABLE=1)
run_install --yes --channel pre --dir "$c/pyn"
expect "--channel pre takes the newest release overall" has "$c/pyn/.env" "PYN_VERSION=v0.1.2"
chan h8; extra_env=(FAKE_STABLE=1)
run_install --yes --channel stable --dir "$c/pyn"
expect "--channel stable takes the latest stable" has "$c/pyn/.env" "PYN_VERSION=v1.0.0"
chan h9
run_install --yes --channel bogus
expect "an invalid channel fails" bad_rc
chan h10; extra_env=(FAKE_STABLE=1)
run_install --yes --channel stable --version 0.1.1 --dir "$c/pyn"
expect "--version wins over --channel" has "$c/pyn/.env" "PYN_VERSION=v0.1.1"

# Existing folder: use / refresh / fresh
seed() { # seed NAME: a Docker install of v0.1.1 with a local Compose edit and a custom setting
  case_start "$1"; extra_env=()
  run_install --dir "$c/pyn" --yes --shape full --version v0.1.1-alpha
  echo "# local edit" >>"$c/pyn/docker-compose.yml"
  echo "PYN_REGISTRATION=closed" >>"$c/pyn/.env"
  cp "$c/pyn/.env" "$c/env.before"
  : >"$c/log"
}
baks() { compgen -G "$1.bak.*" >/dev/null; }
seed e1
run_install --dir "$c/pyn" --yes --shape full --reuse use
expect "use: succeeds" ok_rc
expect "use: leaves .env byte for byte" cmp -s "$c/env.before" "$c/pyn/.env"
expect "use: keeps the local Compose edit" has "$c/pyn/docker-compose.yml" "# local edit"
expect "use: makes no backups" bash -c '! compgen -G "$0/*.bak.*"' "$c/pyn"
expect "use: still starts the stack" has "$c/log" "compose up --detach --wait"
seed e2
run_install --dir "$c/pyn" --yes --shape full --reuse refresh --version v0.1.2-alpha
expect "refresh: replaces the Compose file" lacks "$c/pyn/docker-compose.yml" "# local edit"
expect "refresh: backs the old Compose file up" baks "$c/pyn/docker-compose.yml"
expect "refresh: the backup holds the local edit" bash -c 'grep -qF "# local edit" "$0"/docker-compose.yml.bak.*' "$c/pyn"
expect "refresh: keeps custom env settings" has "$c/pyn/.env" "PYN_REGISTRATION=closed"
expect "refresh: moves the version" has "$c/pyn/.env" "PYN_VERSION=v0.1.2"
expect "refresh: shows the Compose diff" has "$c/out" "differs from the one in v0.1.2-alpha"
expect "refresh: the diff is a unified diff" has "$c/out" "+++"
seed e3
pw0=$(sed -n 's/^POSTGRES_PASSWORD=//p' "$c/pyn/.env")
run_install --dir "$c/pyn" --yes --shape full --reuse fresh
expect "fresh: succeeds" ok_rc
expect "fresh: backs up .env" baks "$c/pyn/.env"
expect "fresh: the backup is the old env" bash -c 'cmp -s "$0" "$1"/.env.bak.*' "$c/env.before" "$c/pyn"
expect "fresh: the backup is owner-only" bash -c '[ "$(stat -c %a "$1"/.env.bak.*)" = 600 ]' _ "$c/pyn"
expect "fresh: backs up the Compose file" baks "$c/pyn/docker-compose.yml"
expect "fresh: writes a new env without the custom setting" lacks "$c/pyn/.env" "PYN_REGISTRATION"
expect "fresh: generates new secrets" bash -c '! grep -qF "POSTGRES_PASSWORD=$0" "$1"' "$pw0" "$c/pyn/.env"
expect "fresh: warns about the database volume" has "$c/out" "keeps the old password"
seed e4
tty_file="$c/answers"; printf 'full\ndocker\n%s\n\n\n1\n\n' "$c/pyn" >"$tty_file"
run_install
expect "prompted: asks what to do with the existing folder" has "$out" "An install already exists in $c/pyn"
expect "prompted: offers all three answers" has "$out" "3) Start fresh"
expect "prompted: use leaves the env untouched" cmp -s "$c/env.before" "$c/pyn/.env"
expect "prompted: the address is taken from the existing env" lacks "$out" "Address people will use"
seed e5
tty_file="$c/answers"; printf 'full\ndocker\n%s\n\n\n2\n\nn\n' "$c/pyn" >"$tty_file"
run_install
expect "prompted: refresh asks before moving PYN_VERSION" has "$out" "Move PYN_VERSION in $c/pyn/.env from v0.1.1 to v0.1.2"
expect "prompted: declining keeps the old version" has "$c/pyn/.env" "PYN_VERSION=v0.1.1"
expect "prompted: declining is stated" has "$out" "PYN_VERSION stays v0.1.1"
seed e6
tty_file="$c/answers"; printf 'full\ndocker\n%s\n\n\n2\n\n\n' "$c/pyn" >"$tty_file"
run_install
expect "prompted: accepting moves PYN_VERSION" has "$c/pyn/.env" "PYN_VERSION=v0.1.2"
expect "prompted: the env is still the user's" has "$c/pyn/.env" "PYN_REGISTRATION=closed"
expect "prompted: the secrets are unchanged" has "$c/pyn/.env" "POSTGRES_PASSWORD=$(sed -n 's/^POSTGRES_PASSWORD=//p' "$c/env.before")"

# An env file elsewhere
cat >"$work/ext.env" <<'EOF'
PYN_VERSION=v0.1.0
POSTGRES_PASSWORD=keep-this-password
PYN_SECRET_KEY=
PYN_PUBLIC_URL=http://envhost:8080
MY_CUSTOM=1
EOF
case_start x1; extra_env=()
mkdir -p "$c/pyn"; printf 'OLD_ENV=1\n' >"$c/pyn/.env"
cp "$work/ext.env" "$c/ext.env"
run_install --dir "$c/pyn" --yes --shape full --env-file "$c/ext.env"
expect "Docker env file: succeeds" ok_rc
expect "Docker env file: existing .env is backed up first" bash -c 'grep -qF OLD_ENV=1 "$0"/.env.bak.*' "$c/pyn"
expect "Docker env file: copied into place with mode 600" test "$(mode_of "$c/pyn/.env")" = 600
expect "Docker env file: existing password kept" has "$c/pyn/.env" "POSTGRES_PASSWORD=keep-this-password"
expect "Docker env file: missing secret generated" bash -c 'grep -qE "^PYN_SECRET_KEY=[0-9a-f]{64}$" "$0"' "$c/pyn/.env"
expect "Docker env file: custom setting kept" has "$c/pyn/.env" "MY_CUSTOM=1"
expect "Docker env file: address taken from it, not prompted" has "$c/pyn/.env" "PYN_PUBLIC_URL=http://envhost:8080"
expect "Docker env file: version moved to the release" has "$c/pyn/.env" "PYN_VERSION=v0.1.2"
expect "Docker env file: the source is untouched" cmp -s "$work/ext.env" "$c/ext.env"
chan x2
run_install --dir "$c/pyn" --yes --shape full --env-file "$c/nope.env"
expect "a missing --env-file fails" bad_rc
expect "and names it" has "$err" "nope.env does not exist"
case_start x3; extra_env=()
cp "$work/ext.env" "$c/ext.env"; chmod 644 "$c/ext.env"
run_install --dir "$c/pyn" --yes --shape full --env-file "$c/ext.env"
expect "a world-readable env file is warned about" has "$out" "readable by everyone"
case_start x4; extra_env=(); tty_file="$c/answers"; cp "$work/ext.env" "$c/ext.env"
printf 'full\ndocker\n%s\n%s\n\n\n' "$c/pyn" "$c/ext.env" >"$tty_file"
run_install
expect "prompted: an env file elsewhere can be named" has "$c/pyn/.env" "MY_CUSTOM=1"

# Required keys
case_start k1; extra_env=()
rm -f "$c/bin/openssl"; printf '#!/bin/sh\n' >"$c/bin/openssl"; chmod +x "$c/bin/openssl"
run_install --dir "$c/pyn" --yes --shape full
expect "required keys still missing stop before starting" bad_rc
expect "and are named" has "$err" "PYN_SECRET_KEY"
expect "nothing was started" lacks "$c/log" "compose up"

# Native with an env file elsewhere
case_start y1; extra_env=(FAKE_PG_ACTIVE=1)
mkdir -p "$c/root/srv"
printf 'PYN_DATABASE_URL=postgres://u:p@db.example.com:5432/pyn\nPYN_SECRET_KEY=orig-secret\nPYN_PUBLIC_URL=http://nat.example.com:7878\nMY_CUSTOM=1\n' >"$c/root/srv/pyn.env"
chmod 644 "$c/root/srv/pyn.env"
run_install --yes --shape server --method native --env-file /srv/pyn.env
expect "native env file: succeeds" ok_rc
expect "native env file: the unit references it" has "$c/root/etc/systemd/system/pyn.service" "EnvironmentFile=/srv/pyn.env"
expect "native env file: nothing is written to /etc/pyn/pyn.env" bash -c '[ ! -e "$0" ]' "$c/root/etc/pyn/pyn.env"
expect "native env file: the secret is kept" has "$c/root/srv/pyn.env" "PYN_SECRET_KEY=orig-secret"
expect "native env file: custom settings kept" has "$c/root/srv/pyn.env" "MY_CUSTOM=1"
expect "native env file: missing settings filled" has "$c/root/srv/pyn.env" "PYN_DATA_DIR=/var/lib/pyn"
expect "native env file: the database URL is kept" has "$c/root/srv/pyn.env" "PYN_DATABASE_URL=postgres://u:p@db.example.com:5432/pyn"
expect "native env file: no local database is created" bash -c '[ ! -f "$0" ]' "$c/log.sql"
expect "native env file: world-readable is warned about" has "$out" "readable by everyone"
expect "native env file: an owned file is tightened to 600" test "$(mode_of "$c/root/srv/pyn.env")" = 600
case_start y2; extra_env=(FAKE_PG_ACTIVE=1)
run_install --yes --shape server --method native --env-file /srv/new.env
expect "native: a path that does not exist yet is created there" test "$(mode_of "$c/root/srv/new.env")" = 600
expect "native: and referenced by the unit" has "$c/root/etc/systemd/system/pyn.service" "EnvironmentFile=/srv/new.env"
run_install --yes --shape server --method native --env-file /srv/other.env --database-url postgres://u:p@h/d
expect "native: a changed env path rewrites the unit" has "$c/root/etc/systemd/system/pyn.service" "EnvironmentFile=/srv/other.env"
expect "native: the old unit is backed up" baks "$c/root/etc/systemd/system/pyn.service"
case_start y3; extra_env=(FAKE_PG_ACTIVE=1)
run_install --yes --shape server --method native
cp "$c/root/etc/pyn/pyn.env" "$c/env.before"; : >"$c/log"
extra_env=(FAKE_PG_ACTIVE=1 FAKE_PYN_EXISTS=1)
run_install --yes --shape server --method native --reuse use
expect "native use: leaves the env alone" cmp -s "$c/env.before" "$c/root/etc/pyn/pyn.env"
expect "native use: downloads no binary" lacks "$c/log" "releases/download"
expect "native use: restarts the service" has "$c/log" "systemctl restart pyn"
run_install --yes --shape server --method native --reuse fresh
expect "native fresh: backs up the env file" baks "$c/root/etc/pyn/pyn.env"
expect "native fresh: the backup is the old env" bash -c 'cmp -s "$0" "$1"/pyn.env.bak.*' "$c/env.before" "$c/root/etc/pyn"
expect "native fresh: writes a new secret" bash -c '! cmp -s "$0" "$1"' "$c/env.before" "$c/root/etc/pyn/pyn.env"
case_start y4; extra_env=(FAKE_PG_ACTIVE=1)
run_install --yes --shape server --method native --version v0.1.1-alpha
: >"$c/log"; extra_env=(FAKE_PG_ACTIVE=1 FAKE_PYN_EXISTS=1)
tty_file="$c/answers"; printf 'server\nnative\n\n2\n\n\nn\n' >"$tty_file"
run_install
expect "native prompted: asks about the existing install" has "$out" "An install already exists (env file /etc/pyn/pyn.env)"
expect "native prompted: declining the replace keeps the old binary" has "$out" "keeping pyn-server v0.1.1-alpha"
expect "native prompted: the recorded version is unchanged" has "$c/root/etc/pyn/version" "v0.1.1-alpha"


# Flags
case_start f1
run_install --help
expect "--help prints usage" has "$out" "Usage: install.sh"
run_install --bogus
expect "an unknown flag fails" bad_rc
case_start f2
run_install --yes --shape nonsense
expect "an invalid shape fails" bad_rc
run_install --yes --method nonsense --shape full
expect "an invalid method fails" bad_rc

bash -n "$src/scripts/install.sh" && pass "install.sh parses" || fail "install.sh parses"
if [ "$fails" -ne 0 ]; then echo "$fails test(s) failed" >&2; exit 1; fi
echo "install tests passed"
