# Homebrew packaging

`xmusic.rb` here is the source of truth. Homebrew reads its own copy from a tap
repository, so publishing means copying this file there and keeping the two in
step.

Once published, installing is:

```bash
brew install alienstro/tap/xmusic
```

## Why a tap

Homebrew only accepts software into `homebrew/core` if it is already reasonably
well known, so a personal tap is the normal route. A tap is just a GitHub repo
named `homebrew-<name>`: `alienstro/homebrew-tap` becomes `alienstro/tap`.

## First-time setup

1. **Push and commit the code.** The release helper refuses a dirty working tree,
   so the tag cannot accidentally omit staged or untracked changes. Include
   `Cargo.lock`, because Homebrew builds with `--locked`.

2. **Create the tap checkout.**

   ```bash
   brew tap-new alienstro/tap
   ```

   Then push that tap repo to GitHub as `alienstro/homebrew-tap`.

3. **Prepare the release.** The first run validates the version, clean tree,
   current commit, tap checkout, and formula tooling, then creates a local tag.
   It never pushes:

   ```bash
   TAP_DIR="$(brew --repo alienstro/tap)" packaging/homebrew/publish.sh 0.1.0
   git push origin refs/tags/v0.1.0
   TAP_DIR="$(brew --repo alienstro/tap)" packaging/homebrew/publish.sh 0.1.0
   ```

   The second run verifies the remote tag points to the current commit, downloads
   the GitHub tarball with failure checking, computes its checksum, runs
   `brew style`, and atomically writes the tap formula.

4. **Check it before anyone else does.**

   ```bash
   brew style alienstro/tap
   brew audit --strict --formula alienstro/tap/xmusic
   brew install --formula alienstro/tap/xmusic
   brew test alienstro/tap/xmusic
   ```

## Releasing an update

Bump and commit the workspace version, then run the two-phase `publish.sh` flow
above. The helper refuses an existing tag that does not point at the current
commit and leaves all remote pushes for explicit review.

## Installing from main without a release

The formula carries a `head` spec, so the current `main` branch can be built
directly:

```bash
brew install --HEAD alienstro/tap/xmusic
```

Useful for trying unreleased changes; `brew upgrade` will not track it.

## What the formula relies on

- **Both binaries in one directory.** `xmusic` locates `xmusic-player` beside
  itself, so the formula installs both packages into the same prefix and the
  test asserts both are present. This is the invariant most likely to break in
  packaging, which is why it is tested rather than assumed.
- **Symlink resolution.** Homebrew links binaries from the Cellar into its
  `bin`, so `xmusic` is invoked through a symlink. `std::env::current_exe` may
  return the symlink rather than its target, so `tui/src/daemon.rs` canonicalises
  before looking for its sibling, and falls back to `PATH`. Without that, a
  Homebrew install finds no daemon to start.
- **macOS only.** `depends_on :macos`, because the daemon embeds a WKWebView.
  Linux would need webkit2gtk and has not been tested.
- **Rust at build time only.** `depends_on "rust" => :build`; the installed
  binaries are native and need no runtime.
