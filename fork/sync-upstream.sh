#!/usr/bin/env bash
# Merge an upstream ParadeDB release into a sync/<tag> branch. Never pushes.
# Without a tag, prints the pinned and latest upstream releases.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

pinned=$(jq -r .upstreamTag fork/release.json)
if [[ $# -eq 0 ]]; then
  echo "pinned: $pinned"
  echo "latest: $(gh release view --repo paradedb/paradedb --json tagName --jq .tagName)"
  exit
fi

tag=$1
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "usage: $0 vX.Y.Z" >&2; exit 1; }
git fetch --no-tags https://github.com/paradedb/paradedb.git "refs/tags/$tag:refs/tags/$tag"
git switch -c "sync/$tag" origin/paradedb-ha

commit=$(git rev-parse "$tag^{commit}")
jq --arg tag "$tag" --arg commit "$commit" \
  '.upstreamTag = $tag | .upstreamCommit = $commit | .revision = 1' \
  fork/release.json > fork/release.json.tmp
mv fork/release.json.tmp fork/release.json
git commit -qm "chore: target upstream $tag" fork/release.json

# Stops on conflicts: resolve, `git merge --continue`, then push the branch.
git merge --no-edit "$tag"
echo "Merged $tag into sync/$tag. Push it to run CI: git push -u origin sync/$tag"
