#!/bin/sh
# build-deb.sh - build a .deb package from the release binary using dpkg-deb.
#
# POSIX sh; Linux-only. Run from the crate root (apps/c0pl4nd).
#
# This skeleton uses the manual dpkg-deb staging approach (no dh/debuild) so it
# works without a full Debian source tree. The debian/control file is reused
# only as the field source; the binary control file is generated here.
#
# Required tools on the maintainer's machine:
#   * dpkg-deb   (from the dpkg package)
#   * a release binary:
#       cargo build --release --bin c0pl4nd
#
# Override defaults via environment variables:
#   VERSION, BIN, TARGET_DIR, DIST_DIR, ARCH
set -eu

VERSION="${VERSION:-0.1.0}"
BIN="${BIN:-c0pl4nd}"
TARGET_DIR="${TARGET_DIR:-target/release}"
DIST_DIR="${DIST_DIR:-dist}"
ARCH="${ARCH:-amd64}"

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
DESKTOP="${SCRIPT_DIR}/c0pl4nd.desktop"
ICON="${SCRIPT_DIR}/../icons/c0pl4nd.png"
MANPAGE="${SCRIPT_DIR}/../../man/c0pl4nd.1"

BIN_PATH="${TARGET_DIR}/${BIN}"
if [ ! -f "${BIN_PATH}" ]; then
	echo "error: release binary not found at ${BIN_PATH}" >&2
	echo "       run: cargo build --release --bin ${BIN}" >&2
	exit 1
fi

if ! command -v dpkg-deb >/dev/null 2>&1; then
	echo "error: dpkg-deb not found on PATH (install the dpkg package)" >&2
	exit 1
fi

PKG_ROOT="${DIST_DIR}/${BIN}_${VERSION}_${ARCH}"
echo "Staging package tree at ${PKG_ROOT}"
rm -rf "${PKG_ROOT}"
mkdir -p "${PKG_ROOT}/DEBIAN"
mkdir -p "${PKG_ROOT}/usr/bin"
mkdir -p "${PKG_ROOT}/usr/share/applications"
mkdir -p "${PKG_ROOT}/usr/share/icons/hicolor/256x256/apps"
mkdir -p "${PKG_ROOT}/usr/share/doc/${BIN}"
mkdir -p "${PKG_ROOT}/usr/share/man/man1"

# License texts. The binary embeds ~21 OFL-1.1 / Apache-2.0 typefaces via
# include_bytes!; OFL-1.1 s2 requires the license text to accompany every copy,
# so this is mandatory and fails the build when incomplete.
sh "${SCRIPT_DIR}/../collect-licenses.sh" "${SCRIPT_DIR}/../.." \
	"${PKG_ROOT}/usr/share/doc/${BIN}"

install -m 0755 "${BIN_PATH}" "${PKG_ROOT}/usr/bin/${BIN}"
install -m 0644 "${DESKTOP}" "${PKG_ROOT}/usr/share/applications/c0pl4nd.desktop"
if [ -f "${ICON}" ]; then
	install -m 0644 "${ICON}" \
		"${PKG_ROOT}/usr/share/icons/hicolor/256x256/apps/c0pl4nd.png"
else
	echo "warning: icon ${ICON} missing; run packaging/gen-icons.sh first" >&2
fi

# Man page: gzip into the standard man1 location so `man c0pl4nd` works after
# install. Best-effort — a missing gzip leaves the uncompressed page.
if [ -f "${MANPAGE}" ]; then
	if command -v gzip >/dev/null 2>&1; then
		gzip -9 -c -n "${MANPAGE}" > "${PKG_ROOT}/usr/share/man/man1/c0pl4nd.1.gz"
		chmod 0644 "${PKG_ROOT}/usr/share/man/man1/c0pl4nd.1.gz"
	else
		install -m 0644 "${MANPAGE}" "${PKG_ROOT}/usr/share/man/man1/c0pl4nd.1"
	fi
else
	echo "warning: man page ${MANPAGE} missing; skipping" >&2
fi

# Generate the binary control file.
INSTALLED_SIZE="$(du -ks "${PKG_ROOT}/usr" | cut -f1)"
cat > "${PKG_ROOT}/DEBIAN/control" <<EOF
Package: ${BIN}
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Maintainer: Itasha.Corp <support@itasha.example>
Installed-Size: ${INSTALLED_SIZE}
Homepage: https://github.com/46b-ETYKiAL/C0PL4ND
Description: Fast, cross-platform terminal emulator
 C0PL4ND is a cross-platform terminal emulator written in Rust. It provides
 a fast rendering pipeline and runs on Windows, Linux, and macOS.
EOF

OUTPUT="${DIST_DIR}/${BIN}_${VERSION}_${ARCH}.deb"
echo "Building ${OUTPUT}"
dpkg-deb --build --root-owner-group "${PKG_ROOT}" "${OUTPUT}"

echo "Done: ${OUTPUT}"
