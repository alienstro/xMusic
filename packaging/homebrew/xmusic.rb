# Homebrew formula for xmusic.
#
# This file is the source of truth; the copy that Homebrew actually reads lives
# in a tap repository (alienstro/homebrew-tap) as Formula/xmusic.rb. See
# packaging/homebrew/README.md for how to publish and update it.

class Xmusic < Formula
  desc "Terminal client for YouTube Music"
  homepage "https://github.com/alienstro/xMusic"
  url "https://github.com/alienstro/xMusic/archive/refs/tags/v0.2.2.tar.gz"
  # publish.sh fills this in from the tagged tarball; see packaging/homebrew/README.md.
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"
  head "https://github.com/alienstro/xMusic.git", branch: "main"

  depends_on "rust" => :build
  # The daemon embeds a WKWebView. Linux would need webkit2gtk and is untested,
  # so don't claim support for it.
  depends_on :macos

  def install
    # Order matters less than location: both binaries have to land in the same
    # directory, because the client finds the daemon beside itself.
    system "cargo", "install", *std_cargo_args(path: "player")
    system "cargo", "install", *std_cargo_args(path: "tui")
  end

  def caveats
    <<~EOS
      xmusic runs a background daemon (xmusic-player) that holds the audio. It
      keeps playing after you quit the interface, and survives closing the
      terminal. To stop it:

        xmusic --kill-daemon

      Search and playback work without signing in. Google will not accept a
      sign-in from an embedded webview, so to use your own account you sign in
      with your normal browser and xmusic copies that session across: press L
      (or run `xmusic --login`), sign in when your browser opens, then press L
      again. macOS will ask once for keychain permission, which is what lets
      xmusic decrypt the browser's cookies.

        xmusic --uninstall    # stop the daemon and delete its data

      The daemon logs to ~/.xmusic/daemon.log and listens on 127.0.0.1:13723.
    EOS
  end

  test do
    # Both halves must be installed side by side, or the client has no daemon
    # to start. This is the invariant most likely to break in packaging.
    assert_path_exists bin/"xmusic"
    assert_path_exists bin/"xmusic-player"

    assert_match "terminal client for YouTube Music", shell_output("#{bin}/xmusic --help")
    assert_match "--kill-daemon", shell_output("#{bin}/xmusic --help")
  end
end
