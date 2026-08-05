# Homebrew Cask skeleton for C0PL4ND.
#
# Distribute via a tap repository (e.g. 46b-ETYKiAL/homebrew-tap):
#   brew install --cask 46b-ETYKiAL/tap/c0pl4nd
#
# Replace the sha256 placeholders with the real DMG checksums for each arch
# (printed in the release SHA256SUMS file). Bump `version` per release.
cask "c0pl4nd" do
  version "0.1.0"

  on_arm do
    sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    url "https://github.com/46b-ETYKiAL/C0PL4ND/releases/download/v#{version}/c0pl4nd-v#{version}-aarch64-apple-darwin.dmg"
  end

  on_intel do
    sha256 "1111111111111111111111111111111111111111111111111111111111111111"
    url "https://github.com/46b-ETYKiAL/C0PL4ND/releases/download/v#{version}/c0pl4nd-v#{version}-x86_64-apple-darwin.dmg"
  end

  name "C0PL4ND"
  desc "Fast, cross-platform terminal emulator"
  homepage "https://github.com/46b-ETYKiAL/C0PL4ND"

  app "C0PL4ND.app"

  # Optional CLI symlink so `c0pl4nd` works from any shell.
  binary "#{appdir}/C0PL4ND.app/Contents/MacOS/c0pl4nd"

  zap trash: [
    "~/Library/Application Support/c0pl4nd",
    "~/Library/Caches/corp.itasha.c0pl4nd",
    "~/Library/Preferences/corp.itasha.c0pl4nd.plist",
    "~/Library/Saved Application State/corp.itasha.c0pl4nd.savedState",
  ]
end
