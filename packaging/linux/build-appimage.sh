#!/bin/sh
# build-appimage.sh - package the release binary as a portable AppImage.
#
# POSIX sh; Linux-only. Run from the crate root (apps/c0pl4nd).
#
# Required tools on the maintainer's machine:
#   * linuxdeploy        (https://github.com/linuxdeploy/linuxdeploy)
#   * a release binary:
#       cargo build --release --bin c0pl4nd
#
# linuxdeploy is auto-downloaded if not on PATH and DOWNLOAD_LINUXDEPLOY=1.
#
# Override defaults via environment variables:
#   APP_NAME, VERSION, BIN, TARGET_DIR, DIST_DIR, ARCH
set -eu

APP_NAME="${APP_NAME:-C0PL4ND}"
VERSION="${VERSION:-0.1.0}"
BIN="${BIN:-c0pl4nd}"
TARGET_DIR="${TARGET_DIR:-target/release}"
DIST_DIR="${DIST_DIR:-dist}"
ARCH="${ARCH:-x86_64}"

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
DESKTOP="${SCRIPT_DIR}/c0pl4nd.desktop"
ICON="${SCRIPT_DIR}/../icons/c0pl4nd.png"

BIN_PATH="${TARGET_DIR}/${BIN}"
if [ ! -f "${BIN_PATH}" ]; then
	echo "error: release binary not found at ${BIN_PATH}" >&2
	echo "       run: cargo build --release --bin ${BIN}" >&2
	exit 1
fi

APPDIR="${DIST_DIR}/${APP_NAME}.AppDir"
echo "Assembling AppDir at ${APPDIR}"
rm -rf "${APPDIR}"
mkdir -p "${APPDIR}/usr/bin"
mkdir -p "${APPDIR}/usr/share/applications"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/256x256/apps"

# License texts (OFL-1.1 s2 — the embedded typefaces' licenses must travel
# with every copy). Mandatory: the collector exits non-zero if any is missing.
sh "${SCRIPT_DIR}/../collect-licenses.sh" "${SCRIPT_DIR}/../.." \
	"${APPDIR}/usr/share/doc/${BIN}"

cp "${BIN_PATH}" "${APPDIR}/usr/bin/${BIN}"
chmod +x "${APPDIR}/usr/bin/${BIN}"
cp "${DESKTOP}" "${APPDIR}/usr/share/applications/c0pl4nd.desktop"

if [ -f "${ICON}" ]; then
	cp "${ICON}" "${APPDIR}/usr/share/icons/hicolor/256x256/apps/c0pl4nd.png"
else
	echo "warning: icon ${ICON} missing; run packaging/gen-icons.sh first" >&2
fi

# Locate or fetch linuxdeploy.
LINUXDEPLOY="$(command -v linuxdeploy || true)"
if [ -z "${LINUXDEPLOY}" ]; then
	if [ "${DOWNLOAD_LINUXDEPLOY:-0}" = "1" ]; then
		TOOL="linuxdeploy-${ARCH}.AppImage"
		URL="https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/${TOOL}"
		echo "Downloading ${URL}"
		mkdir -p "${DIST_DIR}"
		curl -fsSL -o "${DIST_DIR}/${TOOL}" "${URL}"
		chmod +x "${DIST_DIR}/${TOOL}"
		LINUXDEPLOY="${DIST_DIR}/${TOOL}"
	else
		echo "error: linuxdeploy not found on PATH." >&2
		echo "       install it or rerun with DOWNLOAD_LINUXDEPLOY=1" >&2
		exit 1
	fi
fi

OUTPUT="${DIST_DIR}/${BIN}-v${VERSION}-${ARCH}.AppImage"
echo "Building ${OUTPUT}"
OUTPUT="${OUTPUT}" "${LINUXDEPLOY}" \
	--appdir "${APPDIR}" \
	--desktop-file "${APPDIR}/usr/share/applications/c0pl4nd.desktop" \
	--output appimage

echo "Done: ${OUTPUT}"
