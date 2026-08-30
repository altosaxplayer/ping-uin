class PingUin < Formula
  desc "btop-style TUI for monitoring IPs and hostnames"
  homepage "https://github.com/altosaxplayer/ping-uin"
  url "https://github.com/altosaxplayer/ping-uin/archive/refs/tags/v0.1.0.tar.gz"

  depends_on "rust" => :build

  def install
    system "cargo", "build", "--release"
    bin.install "target/release/ping-uin"
  end

  test do
    system "#{bin}/ping-uin", "--help"
  end
end
