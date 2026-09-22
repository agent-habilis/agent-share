class AgentShare < Formula
  desc "Share a folder with peers, or mount a peer's folder locally"
  homepage "https://github.com/agent-habilis/agent-share"
  version "0.1.0"
  license "MIT"

  # The release workflow rewrites every version and digest below, matching a
  # `sha256` line only where it directly follows its `url`. Nothing may be put
  # between the two, or that pair keeps its stale digest through the bump and
  # ships a formula that cannot verify.
  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-share/releases/download/v#{version}/agent-share-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    else
      url "https://github.com/agent-habilis/agent-share/releases/download/v#{version}/agent-share-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    if Hardware::CPU.intel?
      url "https://github.com/agent-habilis/agent-share/releases/download/v#{version}/agent-share-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    elsif Hardware::CPU.arm?
      url "https://github.com/agent-habilis/agent-share/releases/download/v#{version}/agent-share-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "agent-share"
    man1.install Dir["man/*.1"]
  end

  test do
    assert_match "agent-share", shell_output("#{bin}/agent-share --version")
  end
end
