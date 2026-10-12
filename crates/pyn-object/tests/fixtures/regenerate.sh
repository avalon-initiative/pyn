#!/usr/bin/env bash
# Regenerates objects.tsv with the git binary as an oracle. Run from this directory: ./regenerate.sh
# Tests never run git; they only read objects.tsv. Columns: name, algorithm, kind, id, raw body in hex.
set -euo pipefail
cd "$(dirname "$0")"
out=objects.tsv
: >"$out"
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export GIT_AUTHOR_NAME=Alice GIT_AUTHOR_EMAIL=alice@example.com
export GIT_COMMITTER_NAME=Bob GIT_COMMITTER_EMAIL=bob@example.com

gen() {
  local algo=$1 repo
  repo=$(mktemp -d)
  trap 'rm -rf "$repo"' RETURN
  git init -q --object-format="$algo" "$repo"
  cd "$repo"

  rec() { # name kind id
    printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$algo" "$2" "$3" "$(git cat-file "$2" "$3" | xxd -p | tr -d '\n')" >>"$OLDPWD/$out"
  }
  blob() { local id; id=$(git hash-object -w --stdin); rec "$1" blob "$id"; echo "$id"; }
  put() { # name kind text -> id, via hash-object so the body is exactly the text
    local id; id=$(printf '%s' "$3" | git hash-object -w -t "$2" ${LITERALLY:-} --stdin); rec "$1" "$2" "$id"; echo "$id"
  }

  b_empty=$(: | blob blob_empty)
  b_hello=$(printf 'hello\n' | blob blob_hello)
  b_script=$(printf '#!/bin/sh\n' | blob blob_script)
  b_link=$(printf 'target' | blob blob_link)
  b_bin=$(python3 -c 'import sys; sys.stdout.buffer.write(bytes(range(256)))' | blob blob_binary)

  t_empty=$(git mktree </dev/null); rec tree_empty tree "$t_empty"

  c_root=$(printf 'root\n' | GIT_AUTHOR_DATE='1700000000 +0000' GIT_COMMITTER_DATE='1700000000 +0000' git commit-tree "$t_empty")
  rec commit_root commit "$c_root"

  mix=$(printf '040000 tree %s\tfoo\x00100644 blob %s\tfoo-bar\x00100755 blob %s\tfoo.c\x00120000 blob %s\tfoo0\x00160000 commit %s\tmod\x00100644 blob %s\tzzz\x00' \
    "$t_empty" "$b_hello" "$b_script" "$b_link" "$c_root" "$b_empty" | git mktree -z --missing)
  rec tree_mixed tree "$mix"

  nonutf=$(printf '100644 blob %s\t\xff\xfe\x00100644 blob %s\tcaf\xc3\xa9\x00100644 blob %s\ta\x00' "$b_hello" "$b_hello" "$b_bin" | git mktree -z)
  rec tree_nonutf8 tree "$nonutf"

  c_neg=$(printf 'second\n\nbody line\n' | GIT_AUTHOR_DATE='1700000100 -0800' GIT_COMMITTER_DATE='1700000200 -0800' git commit-tree "$mix" -p "$c_root")
  rec commit_neg_tz commit "$c_neg"

  c_half=$(printf 'half hour' | GIT_AUTHOR_DATE='1700000300 +0530' GIT_COMMITTER_DATE='1700000400 -0330' git commit-tree "$nonutf" -p "$c_neg")
  rec commit_half_hour commit "$c_half"

  c_merge=$(printf 'Merge\n' | GIT_AUTHOR_DATE='1700000500 +1245' GIT_COMMITTER_DATE='1700000500 +1245' git commit-tree "$mix" -p "$c_root" -p "$c_neg" -p "$c_half")
  rec commit_merge commit "$c_merge"

  put commit_nonutf8_name commit "tree $t_empty
author $(printf '\xff\xfe') Z <alice@example.com> 100000000 +0000
committer Bob <bob@example.com> 100000000 +0000

" >/dev/null

  put commit_gpgsig commit "tree $t_empty
parent $c_root
author Alice <alice@example.com> 1700000600 +0100
committer Bob <bob@example.com> 1700000700 -0700
encoding ISO-8859-1
gpgsig -----BEGIN PGP SIGNATURE-----
 
 iQEzBAABCAAdFiEE
 =abcd
 -----END PGP SIGNATURE-----

signed
" >/dev/null

  put tag_annotated tag "object $c_root
type commit
tag v1.0
tagger Alice <alice@example.com> 1700000800 +0100

release
" >/dev/null
  LITERALLY=--literally put tag_no_tagger tag "object $b_hello
type blob
tag old

legacy tag
" >/dev/null
  put tag_signed tag "object $mix
type tree
tag v2
tagger Bob <bob@example.com> 1700000900 +0000

msg
-----BEGIN PGP SIGNATURE-----

iQEzBAAB
-----END PGP SIGNATURE-----
" >/dev/null
  cd "$OLDPWD"
}

gen sha1
gen sha256
