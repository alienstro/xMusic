#!/usr/bin/env bash
# Prepares an xmusic release for the Homebrew tap.
#
# The script never pushes. On the first run it validates the repository and
# creates a local tag, then prints the tag-push command. After that tag is pushed,
# run the script again to fetch the GitHub tarball, verify its checksum, and
# atomically update the tap formula.

set -euo pipefail

VERSION="${1:?usage: publish.sh <version>   e.g. publish.sh 0.1.0}"
REPO="alienstro/xMusic"
TAG="v${VERSION}"
TAP_DIR="${TAP_DIR:-$HOME/homebrew-tap}"
TAP_FORMULA="$TAP_DIR/Formula/xmusic.rb"
TARBALL="https://github.com/${REPO}/archive/refs/tags/${TAG}.tar.gz"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$ ]]; then
  echo "invalid version: $VERSION" >&2
  exit 1
fi

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

if [ -n "$(git status --porcelain)" ]; then
  echo "working tree is not clean; commit or stash every change before tagging" >&2
  exit 1
fi

WORKSPACE_VERSION="$(awk '
  /^\[workspace\.package\]$/ { in_package = 1; next }
  /^\[/ { in_package = 0 }
  in_package && /^version = / {
    gsub(/^[^\"]*\"|\".*$/, "", $0)
    print
    exit
  }
' Cargo.toml)"
if [ "$WORKSPACE_VERSION" != "$VERSION" ]; then
  echo "Cargo.toml version is $WORKSPACE_VERSION, not $VERSION" >&2
  exit 1
fi

if ! command -v brew >/dev/null 2>&1; then
  echo "Homebrew is required to validate the formula" >&2
  exit 1
fi
if [ ! -d "$TAP_DIR/Formula" ]; then
  echo "tap checkout not found at $TAP_DIR" >&2
  echo "clone it first or set TAP_DIR to its location" >&2
  exit 1
fi

HEAD_COMMIT="$(git rev-parse HEAD)"
if git show-ref --verify --quiet "refs/tags/$TAG"; then
  TAG_COMMIT="$(git rev-list -n 1 "$TAG")"
  if [ "$TAG_COMMIT" != "$HEAD_COMMIT" ]; then
    echo "$TAG points to $TAG_COMMIT, not current HEAD $HEAD_COMMIT" >&2
    exit 1
  fi
else
  git tag "$TAG"
  echo "created local tag $TAG at $HEAD_COMMIT"
fi

REMOTE_COMMIT="$(git ls-remote origin "refs/tags/$TAG^{}" | awk 'NR == 1 { print $1 }')"
if [ -z "$REMOTE_COMMIT" ]; then
  REMOTE_COMMIT="$(git ls-remote --refs origin "refs/tags/$TAG" | awk 'NR == 1 { print $1 }')"
fi
if [ -z "$REMOTE_COMMIT" ]; then
  cat <<NEXT

$TAG is validated locally but has not been pushed. Review it, then run:

  git push origin refs/tags/$TAG
  packaging/homebrew/publish.sh $VERSION

No remote changes were made.
NEXT
  exit 0
fi
if [ "$REMOTE_COMMIT" != "$HEAD_COMMIT" ]; then
  echo "remote $TAG points to $REMOTE_COMMIT, not current HEAD $HEAD_COMMIT" >&2
  exit 1
fi

echo "==> Waiting for $TARBALL"
for attempt in $(seq 1 30); do
  if curl -fsLI "$TARBALL" >/dev/null 2>&1; then
    break
  fi
  [ "$attempt" -eq 30 ] && { echo "timed out fetching $TARBALL" >&2; exit 1; }
  sleep 2
done

echo "==> Computing checksum"
SHA="$(curl -fsSL "$TARBALL" | shasum -a 256 | awk '{print $1}')"

TEMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TEMP_DIR"' EXIT
# The candidate has to sit in a Formula/ directory: brew style applies its
# stricter non-tap ruleset (Sorbet sigils, frozen string literal, top-level
# class docs) to a formula found anywhere else, and no tap formula carries those.
mkdir -p "$TEMP_DIR/Formula"
TEMP_FORMULA="$TEMP_DIR/Formula/xmusic.rb"
sed \
  -e "s|url \".*\"|url \"${TARBALL}\"|" \
  -e "s|sha256 \".*\"|sha256 \"${SHA}\"|" \
  packaging/homebrew/xmusic.rb > "$TEMP_FORMULA"

echo "==> Checking formula"
brew style "$TEMP_FORMULA"
install -m 0644 "$TEMP_FORMULA" "$TAP_FORMULA"

cat <<NEXT

Formula updated at $TAP_FORMULA with sha256 $SHA.
Review and publish the tap separately:

  cd $TAP_DIR
  git add Formula/xmusic.rb
  git commit -m "xmusic ${VERSION}"
  git push

NEXT
