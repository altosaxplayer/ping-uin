class PingUin < Formula
  desc "Btop-style TUI for monitoring IPs and hostnames"
  homepage "https://github.com/altosaxplayer/ping-uin"
  url "https://github.com/altosaxplayer/ping-uin/archive/refs/tags/v0.1.9.tar.gz"
  sha256 "d72d485d89598db66505966de491e29ad10161da553776e38486682f83bba3e6"
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
