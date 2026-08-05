#!/bin/sh
# build-dmg.sh - assemble a .app bundle and wrap it in a distributable .dmg.
#
# POSIX sh; macOS-only (uses hdiutil). Run from the crate root (apps/c0pl4nd).
#
# Required tools on the maintainer's machine:
#   * hdiutil  (ships with macOS)
#   * a release binary built for the host arch:
#       cargo build --release --bin c0pl4nd
#
# Override defaults via environment variables:
#   APP_NAME, VERSION, BIN, TARGET_DIR, DIST_DIR
set -eu

APP_NAME="${APP_NAME:-C0PL4ND}"
VERSION="${VERSION:-0.1.0}"
BIN="${BIN:-c0pl4nd}"
TARGET_DIR="${TARGET_DIR:-target/release}"
DIST_DIR="${DIST_DIR:-dist}"

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
PLIST="${SCRIPT_DIR}/Info.plist"
ICNS="${SCRIPT_DIR}/c0pl4nd.icns"

BIN_PATH="${TARGET_DIR}/${BIN}"
if [ ! -f "${BIN_PATH}" ]; then
	echo "error: release binary not found at ${BIN_PATH}" >&2
	echo "       run: cargo build --release --bin ${BIN}" >&2
	exit 1
fi

APP_BUNDLE="${DIST_DIR}/${APP_NAME}.app"
CONTENTS="${APP_BUNDLE}/Contents"
MACOS_DIR="${CONTENTS}/MacOS"
RES_DIR="${CONTENTS}/Resources"

echo "Assembling ${APP_BUNDLE}"
rm -rf "${APP_BUNDLE}"
mkdir -p "${MACOS_DIR}" "${RES_DIR}"

# License texts inside the bundle (OFL-1.1 s2 — the embedded typefaces'
# licenses must travel with every copy). Mandatory: the collector exits
# non-zero if any is missing, failing the build rather than shipping a breach.
sh "${SCRIPT_DIR}/../collect-licenses.sh" "${SCRIPT_DIR}/../.." \
	"${RES_DIR}/licenses"

cp "${BIN_PATH}" "${MACOS_DIR}/${BIN}"
chmod +x "${MACOS_DIR}/${BIN}"
cp "${PLIST}" "${CONTENTS}/Info.plist"

if [ -f "${ICNS}" ]; then
	cp "${ICNS}" "${RES_DIR}/c0pl4nd.icns"
else
	echo "warning: ${ICNS} missing; bundle will have no icon" >&2
fi

# Optional code signing: set CODESIGN_IDENTITY to a valid Developer ID.
if [ -n "${CODESIGN_IDENTITY:-}" ]; then
	echo "Code signing with identity: ${CODESIGN_IDENTITY}"
	codesign --force --deep --options runtime \
		--sign "${CODESIGN_IDENTITY}" "${APP_BUNDLE}"
fi

DMG_PATH="${DIST_DIR}/${BIN}-v${VERSION}.dmg"
echo "Creating ${DMG_PATH}"
rm -f "${DMG_PATH}"
hdiutil create \
	-volname "${APP_NAME}" \
	-srcfolder "${APP_BUNDLE}" \
	-ov \
	-format UDZO \
	"${DMG_PATH}"

echo "Done: ${DMG_PATH}"
