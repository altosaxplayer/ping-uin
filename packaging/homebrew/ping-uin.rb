class PingUin < Formula
  desc "Btop-style TUI for monitoring IPs and hostnames"
  homepage "https://github.com/altosaxplayer/ping-uin"
  url "https://github.com/altosaxplayer/ping-uin/archive/refs/tags/v0.1.13.tar.gz"
  sha256 "53b9fdd1b964fc61371d0fcb994a20c60dbd39a10c6b76f6bc7eeca2dec1deb4"
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
