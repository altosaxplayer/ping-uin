class PingUin < Formula
  desc "Btop-style TUI for monitoring IPs and hostnames"
  homepage "https://github.com/altosaxplayer/ping-uin"
  url "https://github.com/altosaxplayer/ping-uin/archive/refs/tags/v0.1.5.tar.gz"
  sha256 "1d1b1de4b719810f2bf25597883289b900b5a8540bbdf8fe9e4bb1860b7af381"
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
