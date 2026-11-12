#!/usr/bin/env bash
# Exits 0 only when a successful CI run exists for the same git tree as <commit> (default HEAD), found through the
# pull request(s) the commit came from. Prints one line saying why (stdout when it passes, stderr when not). Any error exits non-zero, so a caller's gate runs.
set -Eeuo pipefail

trap 'echo "ci-green-for-tree: cannot decide (git or GitHub API error, line $LINENO)" >&2' ERR
no() { echo "ci-green-for-tree: $*" >&2; exit 1; }

commit="${1:-HEAD}"
sha=$(git rev-parse --verify "$commit^{commit}")
tree=$(git rev-parse --verify "$sha^{tree}")
repo="${GH_REPO:-${GITHUB_REPOSITORY:-}}"
[ -n "$repo" ] || repo='{owner}/{repo}'

pulls=$(gh api "repos/$repo/commits/$sha/pulls" --jq '.[] | "\(.number) \(.head.sha)"')
[ -n "$pulls" ] || no "no pull request is associated with ${sha:0:12}"

why=""
while read -r number head; do
  head_tree=$(gh api "repos/$repo/git/commits/$head" --jq .tree.sha)
  if [ "$head_tree" != "$tree" ]; then
    why+="; PR #$number head ${head:0:12} has a different tree"
    continue
  fi
  # The latest CI run on the head decides; a re-run that failed overrides an earlier pass.
  run=$(gh api "repos/$repo/actions/runs?head_sha=$head&per_page=100" \
    --jq '[.workflow_runs[] | select(.name == "CI")] | sort_by(.created_at) | .[-1] | if . == null then "none -" else "\(.conclusion // .status) \(.html_url)" end')
  read -r state url <<<"$run"
  if [ "$state" = success ]; then
    echo "ci-green-for-tree: tree ${tree:0:12} passed CI on PR #$number ($url)"
    exit 0
  fi
  why+="; PR #$number CI on the same tree is $state"
done <<<"$pulls"
no "no passing CI run for tree ${tree:0:12}${why}"
