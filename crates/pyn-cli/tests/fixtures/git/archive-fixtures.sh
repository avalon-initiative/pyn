# Shared archive layout for the fixture regeneration scripts and the Rust loader.
fixture_tmp="$(mktemp -d)"
trap 'rm -rf "$fixture_tmp"' EXIT
fixtures="$fixture_tmp/fixtures"
mkdir -p "$fixtures"
while read -r archive members; do
  tar --zstd -xpf "$here/$archive" -C "$fixtures"
done < "$here/archives.txt"

pack_fixtures() {
  local archive members
  local -a paths
  while read -r archive members; do
    read -r -a paths <<< "$members"
    tar --zstd -cf "$fixture_tmp/$archive" -C "$fixtures" "${paths[@]}"
  done < "$here/archives.txt"
  cp "$fixtures/worktree/README.md" "$here/README-worktree.md"
  cp "$fixtures/linked/README.md" "$here/README-linked.md"
  while read -r archive members; do
    mv "$fixture_tmp/$archive" "$here/$archive"
  done < "$here/archives.txt"
}
