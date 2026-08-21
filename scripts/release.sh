#!/usr/bin/env bash
#
# Releases xmusic to the public, in one command.
#
# Everything a release needs happens in one order and stops at the first thing
# that is wrong: bump the version, commit it, push main, tag, push the tag,
# publish a GitHub release, then point the Homebrew tap at the new tarball and
# push that too. After it finishes, `brew upgrade xmusic` and `xmusic update`
# both find the new version, which is the whole point of doing all of it.
#
# Usage: scripts/release.sh patch|minor|major|as-is [--yes] [--dry-run]
#
#   patch|minor|major   bump the version first, via scripts/bump.sh
#   as-is               release the version already in Cargo.toml
#   --yes               skip the confirmation
#   --dry-run           print every command without running any of it
#
# The pushing is the reason this asks first. Three remotes change - main, the
# tag, and the tap - and a tag that points at the wrong commit is not something
# a later commit can take back, because Homebrew has already computed a checksum
# from the tarball it produced.
#
# publish.sh does the formula half and refuses anything unsafe; this drives it
# rather than repeating it, which is why the two-phase dance below (tag, push,
# then run it again) reads the way it does.

set -euo pipefail

readonly ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO="alienstro/xMusic"
readonly TAP="alienstro/tap"
readonly MAIN="main"

DRY_RUN=false
ASSUME_YES=false

usage() {
    sed -n '3,25p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

say() {
    echo "release: $*"
}

die() {
    echo "release: $*" >&2
    exit 1
}

# Echoes before it acts, because the useful record of a release is the list of
# commands that made it.
run() {
    echo "    \$ $*"
    if [ "$DRY_RUN" = false ]; then
        "$@"
    fi
}

workspace_version() {
    awk '
      /^\[workspace\.package\]$/ { in_package = 1; next }
      /^\[/ { in_package = 0 }
      in_package && /^version = / { gsub(/^[^"]*"|".*$/, "", $0); print; exit }
    ' "$ROOT/Cargo.toml"
}

preflight() {
    command -v git >/dev/null || die "git is required"
    command -v gh >/dev/null || die "the GitHub CLI (gh) is required for the release entry"
    command -v brew >/dev/null || die "Homebrew is required to validate the formula"
    command -v curl >/dev/null || die "curl is required to checksum the tarball"

    gh auth status >/dev/null 2>&1 || die "gh is not logged in; run \`gh auth login\`"

    local branch
    branch="$(git -C "$ROOT" rev-parse --abbrev-ref HEAD)"
    [ "$branch" = "$MAIN" ] || die "on branch $branch; release from $MAIN so the tag lands on the published history"

    # Every later step reads the working tree - bump.sh rewrites it, publish.sh
    # refuses a dirty one, and the tarball is built from the tag. Checking once,
    # here, keeps the failure at the start rather than halfway through.
    [ -z "$(git -C "$ROOT" status --porcelain)" ] || die "working tree is not clean; commit or stash first"

    run git -C "$ROOT" fetch origin --tags --quiet
    local ahead behind
    ahead="$(git -C "$ROOT" rev-list --count "origin/$MAIN..HEAD")"
    behind="$(git -C "$ROOT" rev-list --count "HEAD..origin/$MAIN")"
    [ "$behind" = 0 ] || die "origin/$MAIN is $behind commits ahead of you; pull first"
    say "$ahead unpushed commits on $MAIN"

    TAP_DIR="${TAP_DIR:-$(brew --repo "$TAP" 2>/dev/null || true)}"
    [ -d "${TAP_DIR:-}/Formula" ] || die "no tap checkout; run \`brew tap $TAP\` or set TAP_DIR"
    [ -z "$(git -C "$TAP_DIR" status --porcelain)" ] || die "tap checkout $TAP_DIR is dirty; sort that out first"
    # publish.sh reads it from the environment, so export once rather than
    # prefixing the call and hoping the shell scopes it the way we meant.
    export TAP_DIR
    say "tap checkout $TAP_DIR"
}

confirm() {
    local version="$1"
    cat <<SUMMARY

release: about to publish xmusic $version to the public:

    1. commit the version bump, if there is one
    2. push $MAIN to origin
    3. tag v$version and push the tag
    4. create the GitHub release for v$version
    5. rewrite the tap formula for the new tarball and push $TAP

Afterwards, anyone gets it with \`brew upgrade xmusic\` or \`xmusic update\`.

SUMMARY
    if [ "$ASSUME_YES" = true ] || [ "$DRY_RUN" = true ]; then
        return
    fi
    printf 'release: continue? [y/N] '
    local answer
    read -r answer
    case "$answer" in
        y|Y|yes) ;;
        *) die "cancelled; nothing was pushed" ;;
    esac
}

main() {
    local bump="" argument
    for argument in "$@"; do
        case "$argument" in
            -h|--help) usage; exit 0 ;;
            --yes) ASSUME_YES=true ;;
            --dry-run) DRY_RUN=true ;;
            patch|minor|major|as-is) bump="$argument" ;;
            *) usage >&2; exit 2 ;;
        esac
    done
    [ -n "$bump" ] || { usage >&2; exit 2; }

    [ "$DRY_RUN" = true ] && say "dry run: nothing will be committed, pushed or tagged"
    preflight

    local version
    if [ "$bump" = as-is ]; then
        version="$(workspace_version)"
        say "releasing the version already in Cargo.toml: $version"
    else
        run "$ROOT/scripts/bump.sh" "$bump"
        version="$(workspace_version)"
        [ "$DRY_RUN" = true ] && version="<bumped>"
    fi
    [ -n "$version" ] || die "cannot read the version from Cargo.toml"

    local tag="v$version"
    if git -C "$ROOT" ls-remote --exit-code --refs origin "refs/tags/$tag" >/dev/null 2>&1; then
        die "$tag is already published; releasing over it would change what a checksum already describes"
    fi

    confirm "$version"

    # The bump rewrites four files, or none if the version was already right.
    if [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
        run git -C "$ROOT" add -A
        run git -C "$ROOT" commit -m "chore: release $tag"
    fi

    run git -C "$ROOT" push origin "$MAIN"

    # First pass creates and validates the local tag; it never pushes, so the
    # tag is reviewed as a local object before anyone can build from it.
    run "$ROOT/packaging/homebrew/publish.sh" "$version"
    run git -C "$ROOT" push origin "refs/tags/$tag"
    # Second pass now sees the remote tag, downloads that exact tarball, checksums
    # it and writes the formula.
    run "$ROOT/packaging/homebrew/publish.sh" "$version"

    # Notes come from the commits, so the release page says something real
    # without anybody writing it twice.
    run gh release create "$tag" --repo "$REPO" --title "xmusic $version" --generate-notes

    run git -C "$TAP_DIR" add Formula/xmusic.rb
    run git -C "$TAP_DIR" commit -m "xmusic $version"
    run git -C "$TAP_DIR" push

    cat <<DONE

release: xmusic $version is public.

    brew update && brew upgrade xmusic     # for anyone who already has it
    brew install $TAP/xmusic          # for anyone who does not
    xmusic update                          # from inside the client

DONE
}

main "$@"
