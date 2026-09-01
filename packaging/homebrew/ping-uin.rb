class PingUin < Formula
  desc "Btop-style TUI for monitoring IPs and hostnames"
  homepage "https://github.com/altosaxplayer/ping-uin"
  url "https://github.com/altosaxplayer/ping-uin/archive/refs/tags/v0.1.6.tar.gz"
  sha256 "54eb552d407246b1311815ddf298f153f5cab2244dbd59218052a68740918f4a"
  license "MIT"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_path_exists bin/"ping-uin"
    assert_predicate bin/"ping-uin", :executable?
  end
end
