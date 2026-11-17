#!/usr/bin/env bash
# Prints the release body: the `## TLDR` section of the release pull request of $GITHUB_SHA (when non-empty), then GitHub's
# generated notes. Without a TLDR the body is the generated notes alone. Needs GITHUB_REPOSITORY, GITHUB_SHA and gh.
set -euo pipefail

tag="${1:?usage: $0 <tag>}"
repo="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}"
sha="${GITHUB_SHA:?GITHUB_SHA is not set}"

# A failed lookup only loses the TLDR.
pr_body=$(gh api "repos/$repo/commits/$sha/pulls" --jq '.[0].body // ""' 2>/dev/null || true)
tldr=$(awk '
  { sub(/\r$/, "") }
  /^##[ \t]+TLDR[ \t]*$/ { on = 1; next }
  on && /^##[ \t]/ { exit }
  on { if (text == "" && $0 ~ /^[ \t]*$/) next; text = text $0 "\n"; if ($0 !~ /^[ \t]*$/) keep = length(text) }
  END { printf "%s", substr(text, 1, keep) }
' <<<"$pr_body")

notes=$(gh api "repos/$repo/releases/generate-notes" -f tag_name="$tag" --jq .body)
if ! head -n 1 <<<"$notes" | grep -qiE "^##[[:space:]]+what's changed"; then
  notes=$'## What\'s changed\n\n'"$notes"
fi
if [ -n "$tldr" ]; then printf '%s\n\n' "$tldr"; fi
printf '%s\n' "$notes"
