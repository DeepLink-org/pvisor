# Generated from the verified release wheels; do not edit checksums by hand.
class Pvisor < Formula
  desc "Scaling autonomous agent execution"
  homepage "https://deeplink-org.github.io/pvisor/"
  version "0.3.1"
  license "Apache-2.0"

  on_macos do
    depends_on arch: :arm64
    url "https://github.com/DeepLink-org/pvisor/releases/download/v0.3.1/pvisor-0.3.1-macos-arm64.tar.gz"
    sha256 "d9722bab3a4bd3d2196a50727ad0d4625d120a6aea07d93c5155789bdc7d3e0e"
  end

  on_linux do
    depends_on arch: :x86_64
    url "https://github.com/DeepLink-org/pvisor/releases/download/v0.3.1/pvisor-0.3.1-linux-x86_64.tar.gz"
    sha256 "0ae9886e8103c8127df972aa0a0a3f4201421114682113cafb464c93431ddb03"
  end

  def install
    # Keep companions and firmware beside the real executable, behind brew symlinks.
    libexec.install Dir["bin/*"]
    bin.install_symlink libexec/"pvisor", libexec/"pvisor-tui", libexec/"pvisor-replay", libexec/"pvisor-daemon"
    prefix.install "LICENSE", "NOTICE"
  end

  test do
    %w[pvisor pvisor-tui pvisor-replay pvisor-daemon].each do |name|
      assert_match version.to_s, shell_output("#{bin}/#{name} --version")
    end
    # Homebrew's test sandbox cannot nest pVisor's Seatbelt; CI runs Jobs outside it.
    system bin/"pvisor", "tui", "--help"
    system bin/"pvisor", "replay", "--help"
    system bin/"pvisor-daemon", "protocol"
  end
end
