#!/usr/bin/env bash
#
# Bumps the project version, carrying at ten rather than at nine.
#
# The version lives in three files that have to agree: cargo builds from the
# workspace manifest, tauri stamps its own config into the app bundle, and the
# formula's tarball URL has to point at a tag that exists. Bumping them by hand
# means one of them is eventually forgotten, and the symptom of that is a
# daemon and a client that disagree about whether they are compatible.
#
# Usage: scripts/bump.sh patch|minor|major
#
# Counting runs 1..10 per component and then carries, so 0.1.10 is followed by
# 0.2.0 and 0.9.10 by 1.0.0. That is deliberate and not semver: ten is the
# ceiling, so a component reaching it is the carry, not a value to keep.

set -euo pipefail

readonly CEILING=10
readonly ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly MANIFEST="$ROOT/Cargo.toml"
readonly TAURI_CONFIG="$ROOT/player/tauri.conf.json"
readonly FORMULA="$ROOT/packaging/homebrew/xmusic.rb"

usage() {
    cat <<'EOF'
Usage: scripts/bump.sh patch|minor|major

Increments one component by 1 and carries at 10, then writes the result to
Cargo.toml, player/tauri.conf.json and the formula's tarball URL.

    0.1.9  --patch-->  0.1.10
    0.1.10 --patch-->  0.2.0     (patch hit the ceiling, so it carried)
    0.9.10 --minor-->  1.0.0
EOF
}

die() {
    echo "bump: $*" >&2
    exit 1
}

# The workspace manifest is the single source of truth; the other two files are
# copies kept in step with it.
current_version() {
    local version
    version="$(sed -n 's/^version = "\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)"$/\1/p' "$MANIFEST" | head -1)"
    [ -n "$version" ] || die "no version = \"x.y.z\" line in $MANIFEST"
    printf '%s' "$version"
}

# Carries left as far as it needs to, so a patch bump can move the major.
bump() {
    local component="$1" major="$2" minor="$3" patch="$4"

    case "$component" in
        patch) patch=$((patch + 1)) ;;
        minor) minor=$((minor + 1)); patch=0 ;;
        major) major=$((major + 1)); minor=0; patch=0 ;;
        *) usage >&2; exit 2 ;;
    esac

    if [ "$patch" -gt "$CEILING" ]; then
        patch=0
        minor=$((minor + 1))
    fi
    if [ "$minor" -gt "$CEILING" ]; then
        minor=0
        major=$((major + 1))
    fi

    printf '%d.%d.%d' "$major" "$minor" "$patch"
}

# Rewrites through a temporary file so a failed sed cannot leave a file with the
# old version in it and the new version everywhere else.
replace() {
    local file="$1" pattern="$2"
    local scratch
    scratch="$(mktemp)"
    sed "$pattern" "$file" >"$scratch"
    if cmp -s "$file" "$scratch"; then
        rm -f "$scratch"
        die "nothing changed in $file; its version line may have moved"
    fi
    mv "$scratch" "$file"
}

main() {
    [ $# -eq 1 ] || { usage >&2; exit 2; }
    case "$1" in
        -h|--help) usage; exit 0 ;;
    esac

    local from to
    from="$(current_version)"
    IFS=. read -r major minor patch <<<"$from"
    to="$(bump "$1" "$major" "$minor" "$patch")"

    replace "$MANIFEST" "1,/^version = \"$from\"$/s|^version = \"$from\"$|version = \"$to\"|"
    replace "$TAURI_CONFIG" "s|\"version\": \"$from\"|\"version\": \"$to\"|"
    replace "$FORMULA" "s|tags/v$from\.tar\.gz|tags/v$to.tar.gz|g"

    # Keeps Cargo.lock's own record of the workspace crates in step, so
    # --locked builds do not fail on the version that was just written.
    ( cd "$ROOT" && cargo update --workspace --offline >/dev/null 2>&1 ) || true

    echo "bump: $from -> $to"
    echo "bump: the formula's sha256 is now wrong; recompute it once v$to is tagged:"
    echo "      curl -sL https://github.com/alienstro/xMusic/archive/refs/tags/v$to.tar.gz | shasum -a 256"
}

main "$@"
