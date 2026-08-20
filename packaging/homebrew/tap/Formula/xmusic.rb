# Homebrew formula for xmusic.
#
# This file is the source of truth; the copy that Homebrew actually reads lives
# in a tap repository (alienstro/homebrew-tap) as Formula/xmusic.rb. See
# packaging/homebrew/README.md for how to publish and update it.

class Xmusic < Formula
  desc "Terminal client for YouTube Music"
  homepage "https://github.com/alienstro/xMusic"
  url "https://github.com/alienstro/xMusic/archive/refs/tags/v0.1.0.tar.gz"
  # Replace once v0.1.0 is tagged and pushed:
  #   brew fetch --build-from-source ./xmusic.rb
  # or compute it directly:
  #   curl -sL https://github.com/alienstro/xMusic/archive/refs/tags/v0.1.0.tar.gz | shasum -a 256
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

      Search and playback work without signing in. To sign into your account,
      press L inside xmusic, log in, then press H.

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
