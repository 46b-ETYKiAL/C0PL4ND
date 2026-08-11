# Packaging C0PL4ND

C0PL4ND is a cross-platform terminal emulator. The binary crate `c0pl4nd`
builds with `cargo build --release`, producing `c0pl4nd` (Unix) or
`c0pl4nd.exe` (Windows). This directory holds everything needed to package and
distribute that binary on Windows, macOS, and Linux.

## Layout

```
packaging/
├── README.md            # this file
├── gen-icons.sh         # SVG -> PNG/.ico/.icns icon generator
├── icons/               # generated PNGs (created by gen-icons.sh)
├── windows/
│   ├── c0pl4nd.wxs      # WiX v3 MSI definition (candle/light)
│   └── winget/Itasha.C0PL4ND.yaml  # winget manifest skeleton
├── macos/
│   ├── Info.plist       # .app bundle metadata
│   ├── c0pl4nd.rb       # Homebrew cask formula skeleton
│   └── build-dmg.sh     # builds .app + .dmg
└── linux/
    ├── c0pl4nd.desktop  # XDG desktop entry
    ├── build-appimage.sh# AppImage via linuxdeploy
    ├── debian/control   # Debian source-package control fields
    ├── build-deb.sh     # builds .deb via dpkg-deb
    └── install.sh       # POSIX one-line installer (curl | sh)
```

## Quick install (end users)

### Linux / macOS — one-liner

```sh
curl -fsSL https://raw.githubusercontent.com/46b-ETYKiAL/C0PL4ND/master/packaging/linux/install.sh | sh
```

This detects your OS/arch, downloads the latest release tarball, verifies its
SHA256, installs to `~/.local/bin`, and tells you whether that directory is on
your `PATH`. Pin a version with `C0PL4ND_VERSION=v0.1.0` or change the install
location with `C0PL4ND_BIN_DIR=/usr/local/bin`.

### macOS — Homebrew

```sh
brew install --cask 46b-ETYKiAL/tap/c0pl4nd
```

### Windows — winget

```powershell
winget install Itasha.C0PL4ND
```

### Manual

Download the archive for your platform from the
[Releases page](https://github.com/46b-ETYKiAL/C0PL4ND/releases), verify the
SHA256 against `SHA256SUMS`, extract, and put the binary on your `PATH`.

## Building installers (maintainers)

All commands run from the workspace root; the app crate lives at `crates/app`.
Build the release binary first (`cargo build --release --bin c0pl4nd`) unless
the tool builds it for you.

### Icons (do this first)

```sh
./packaging/gen-icons.sh
```

Generates `packaging/icons/*.png`, `packaging/windows/c0pl4nd.ico`, and
`packaging/macos/c0pl4nd.icns` from `assets/svg/app-icon.svg`.

### Windows MSI

The release workflow's `windows-msi` job builds it. To reproduce locally you
need WiX Toolset **v3** (`candle`/`light`) on PATH:

```powershell
candle -arch x64 -dBinPath=target\release\c0pl4nd.exe packaging\windows\c0pl4nd.wxs -o c0pl4nd.wixobj
light  -ext WixUIExtension c0pl4nd.wixobj -o c0pl4nd.msi
```

`c0pl4nd.wxs` is authored against the **WiX v3** schema. It previously used
the v4/v6 schema (`<Package>` root, `StandardDirectory`, `Scope=`), which
`candle`/`light` cannot parse at all -- so it had never been compiled by
anything. There is no `cargo-wix` in this repo and no
`packaging/windows/wix/` directory; both were described here but never
existed.

Generate the `UpgradeCode` GUID **once** (`uuidgen`) and keep it stable across
every release so upgrades replace rather than stack. Provide a `License.rtf`
under `packaging/windows/` for the EULA pane.

### macOS DMG

```sh
./packaging/macos/build-dmg.sh
```

Assembles `dist/C0PL4ND.app` from `Info.plist` + the release binary + icon,
then wraps it in `dist/c0pl4nd-v<version>.dmg`. Set `CODESIGN_IDENTITY` to a
Developer ID to sign the bundle.

### Linux AppImage

```sh
DOWNLOAD_LINUXDEPLOY=1 ./packaging/linux/build-appimage.sh
```

### Linux DEB

```sh
./packaging/linux/build-deb.sh
```

## Release flow (CI)

`.github/workflows/release.yml` is triggered by pushing a `v*` tag (or via
manual dispatch). It:

1. **build** — compiles release binaries on native runners for all six targets:
   Linux (x86_64 + aarch64), macOS (x86_64 + aarch64), and Windows
   (x86_64 + aarch64); packages each as `.tar.gz` (Linux/macOS) or `.zip`
   (Windows) and emits a per-archive `.sha256`.
2. **windows-installer** — builds the native Itasha.Corp CRT installer
   (`c0pl4nd-<tag>-x86_64-setup.exe`), Authenticode-signed via SignPath when the
   signing org is provisioned. This is the **only** installer CI produces — the
   MSI/DMG/AppImage/DEB scripts under `packaging/` are maintainer-local and are
   NOT built or attached by `release.yml`.
3. **release** — flattens all artifacts, aggregates checksums into `SHA256SUMS`,
   emits and minisign-signs the Tier-1 update manifest (`latest.json` +
   `latest.json.minisig`), and publishes a GitHub Release with auto-generated
   notes. Every asset is signed with minisign (`.minisig` siblings).

Continuous integration (`.github/workflows/ci.yml`) runs on every push/PR:
matrix build + test across the three OSes plus `cargo fmt --check` and
`cargo clippy -- -D warnings`. A final `ci-gate` job runs with `if: always()`
and explicitly checks each upstream job's result so a skipped job cannot pass
as green on the merge queue.

### Headless / GPU tests

CI sets `C0PL4ND_HEADLESS=1`. Tests that require a display or GPU should read
this flag and skip themselves (e.g. early-return or `#[ignore]` + opt-in run)
rather than failing on headless runners. Locally, unset the flag to run the
full suite.

## Required maintainer tooling

| Target            | Tool(s)                                              |
|-------------------|------------------------------------------------------|
| Build             | Rust stable toolchain (`cargo`, `rustfmt`, `clippy`) |
| Cross-Linux build | `cross` (release workflow installs it)               |
| Icons             | `librsvg` (`rsvg-convert`) or ImageMagick; `iconutil` (macOS) or `png2icns` (libicns) for `.icns` |
| Windows MSI       | WiX Toolset v3 (`candle`/`light`) — no cargo-wix     |
| macOS DMG         | `hdiutil` (built into macOS); optional Developer ID for signing |
| Linux AppImage    | `linuxdeploy`                                         |
| Linux DEB         | `dpkg-deb`                                            |
| Installer script  | `curl` or `wget`, plus `sha256sum` or `shasum`, `tar` |

## License

C0PL4ND is dual-licensed under **MIT OR Apache-2.0**. Choose either.
