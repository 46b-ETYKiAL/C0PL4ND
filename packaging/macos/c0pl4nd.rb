# Homebrew Cask skeleton for C0PL4ND.
#
# Distribute via a tap repository (e.g. 46b-ETYKiAL/homebrew-tap):
#   brew install --cask 46b-ETYKiAL/tap/c0pl4nd
#
# Replace the sha256 placeholders with the real DMG checksums for each arch
# (printed in the release SHA256SUMS file). Bump `version` per release.
#
# The asset names below are the ones `macos-installer` in .github/workflows/
# release.yml actually publishes: `c0pl4nd-<tag>-<arch>.dmg`, where <arch> is the
# matrix's `arch` (`aarch64` / `x86_64`) and NOT the full Rust target triple.
# They previously carried an `-apple-darwin` suffix that no job has ever emitted,
# so both URLs 404'd — verified against the live release, where the triple form
# returns 404 while a published asset returns 200. Keep these in step with that
# job: a correct namespace pointing at an asset name that does not exist is the
# same broken install in a new place.
cask "c0pl4nd" do
  version "0.1.0"

  on_arm do
    sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    url "https://github.com/46b-ETYKiAL/C0PL4ND/releases/download/v#{version}/c0pl4nd-v#{version}-aarch64.dmg"
  end

  on_intel do
    sha256 "1111111111111111111111111111111111111111111111111111111111111111"
    url "https://github.com/46b-ETYKiAL/C0PL4ND/releases/download/v#{version}/c0pl4nd-v#{version}-x86_64.dmg"
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
