#!/usr/bin/env bash
#
# Puts this working tree behind the `xmusic` command, in one step.
#
# `xmusic update` upgrades to a published release, which is no use while the
# thing you want to run is the checkout in front of you. This does the local
# equivalent: build both binaries, stop the daemon that is still running the old
# ones, and replace them wherever the `xmusic` on your PATH lives.
#
# Usage: scripts/install-local.sh [--to DIR] [--debug]
#
# The destination is worked out rather than assumed, because the answer differs
# per machine: the command usually comes from a Homebrew keg or from
# ~/.cargo/bin, and installing into the other one leaves PATH pointing at the
# copy you did not just build. --to overrides it.
#
# The daemon is stopped before anything is copied, and not for tidiness: macOS
# refuses to write over the file of a running executable, so an unstopped daemon
# makes the install fail halfway.

set -euo pipefail

readonly ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly BINARIES=(xmusic-player xmusic)

usage() {
    cat <<'EOF'
Usage: scripts/install-local.sh [--to DIR] [--debug]

Builds this working tree and installs both binaries over the ones behind your
`xmusic` command, stopping the daemon first.

    --to DIR    Install into DIR instead of wherever `xmusic` resolves to
    --debug     Install the debug build (faster to build, slower to run)
EOF
}

say() {
    echo "install-local: $*"
}

die() {
    echo "install-local: $*" >&2
    exit 1
}

# Symlinks matter here: Homebrew's `xmusic` is a link into the Cellar, and the
# Cellar is where the real file has to be replaced.
resolve() {
    python3 -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "$1"
}

# Where the binaries will go, in order of what the machine can tell us.
destination() {
    local current
    if current="$(command -v xmusic 2>/dev/null)"; then
        dirname "$(resolve "$current")"
        return
    fi
    if [ -d "${CARGO_HOME:-$HOME/.cargo}/bin" ]; then
        echo "${CARGO_HOME:-$HOME/.cargo}/bin"
        return
    fi
    die "no xmusic on PATH and no cargo bin directory; pass --to DIR"
}

main() {
    local profile=release dest=""
    while [ $# -gt 0 ]; do
        case "$1" in
            -h|--help) usage; exit 0 ;;
            --debug) profile=debug; shift ;;
            --to) [ $# -ge 2 ] || die "--to needs a directory"; dest="$2"; shift 2 ;;
            *) usage >&2; exit 2 ;;
        esac
    done

    # Named rather than empty-when-debug: an empty array is an unbound variable
    # under `set -u` on the bash macOS ships.
    local build_flags=(--release)
    if [ "$profile" = debug ]; then
        build_flags=(--profile dev)
    fi

    # Build first. A failed build should leave the working install alone, which
    # it only does if nothing has been stopped or copied yet.
    say "building the workspace ($profile)"
    ( cd "$ROOT" && cargo build "${build_flags[@]}" )

    local built="${CARGO_TARGET_DIR:-$ROOT/target}/$profile"
    for binary in "${BINARIES[@]}"; do
        [ -x "$built/$binary" ] || die "$built/$binary is missing after the build"
    done

    [ -n "$dest" ] || dest="$(destination)"
    [ -d "$dest" ] || die "$dest is not a directory"
    [ -w "$dest" ] || die "$dest is not writable; pass --to DIR, or use sudo deliberately"

    # The new client can stop an older daemon: /quit needs the control token, not
    # a matching version. Not running is the normal case, so it is not a failure.
    say "stopping the daemon"
    "$built/xmusic" --kill-daemon || true

    for binary in "${BINARIES[@]}"; do
        # Via a temporary name in the same directory, so an interrupted copy
        # cannot leave half a binary where a working one used to be.
        install -m 755 "$built/$binary" "$dest/.$binary.incoming"
        mv -f "$dest/.$binary.incoming" "$dest/$binary"
        say "installed $dest/$binary"
    done

    local installed
    installed="$("$dest/xmusic" --version 2>/dev/null || echo unknown)"
    say "$installed is now in $dest"

    case "$dest" in
        */Cellar/*)
            say "Homebrew still records the version it installed; \`brew reinstall xmusic\` puts its own build back"
            ;;
    esac

    local first
    first="$(command -v xmusic 2>/dev/null || true)"
    if [ -z "$first" ]; then
        say "note: $dest is not on your PATH, so \`xmusic\` will not find this build"
    elif [ "$(dirname "$(resolve "$first")")" != "$dest" ]; then
        say "note: \`xmusic\` still resolves to $first, not $dest"
    fi

    say "run \`xmusic\` to start it"
}

main "$@"
