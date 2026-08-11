#!/bin/sh
# collect-licenses.sh — stage every license text that MUST accompany a
# distributed copy of C0PL4ND.
#
# Usage:
#   packaging/collect-licenses.sh <repo-root> <dest-dir>
#
# Why this exists
# ---------------
# C0PL4ND `include_bytes!`-embeds ~21 third-party typefaces directly into the
# shipped executable (crates/app/src/egui_app/fonts.rs). Twenty are licensed
# under the SIL Open Font License 1.1 and one (Syncopate) under Apache-2.0.
#
#   OFL-1.1 section 2 requires that the license text accompany EVERY copy of the
#   Font Software, including when it is bundled inside a larger software package.
#
# Shipping a binary with the fonts compiled in but WITHOUT the license text is a
# license breach, not a paperwork nit. Every packaging path — .tar.gz, .zip,
# .deb, .AppImage, .dmg, MSI, native installer — must call this script so the
# obligation travels with the artifact.
#
# Layout produced under <dest-dir>:
#   LICENSE-MIT
#   LICENSE-APACHE
#   THIRD-PARTY-LICENSES.md
#   licenses/fonts/<FontName>/<OFL.txt|LICENSE.txt>
#
# FAILS CLOSED: if a bundled font directory has no license file, or a top-level
# license document is missing, this script exits non-zero and the release build
# fails. A silently-incomplete license set is exactly the regression this guards.
set -eu

ROOT="${1:?usage: collect-licenses.sh <repo-root> <dest-dir>}"
DEST="${2:?usage: collect-licenses.sh <repo-root> <dest-dir>}"

# Directory holding the bundled typefaces. Note this is NOT the repo-root
# `assets/` directory — C0PL4ND's fonts live under the app crate. (A packaging
# step that copies only repo-root `assets/` picks up icons and media but NOT a
# single font license.)
FONT_ROOT="${ROOT}/crates/app/assets/fonts"

TOP_LEVEL="LICENSE-MIT LICENSE-APACHE THIRD-PARTY-LICENSES.md"

mkdir -p "${DEST}"

# --- top-level license documents -------------------------------------------
for f in ${TOP_LEVEL}; do
	if [ ! -f "${ROOT}/${f}" ]; then
		echo "collect-licenses: FATAL: missing required license document: ${f}" >&2
		exit 1
	fi
	cp "${ROOT}/${f}" "${DEST}/${f}"
done

# --- per-font license texts -------------------------------------------------
if [ ! -d "${FONT_ROOT}" ]; then
	echo "collect-licenses: FATAL: font directory not found: ${FONT_ROOT}" >&2
	exit 1
fi

mkdir -p "${DEST}/licenses/fonts"

count=0
missing=""
for dir in "${FONT_ROOT}"/*/; do
	[ -d "${dir}" ] || continue
	name="$(basename "${dir}")"

	# Only fonts that are actually shipped need their license shipped.
	has_font=0
	for ext in ttf otf ttc woff2; do
		for candidate in "${dir}"*."${ext}"; do
			[ -f "${candidate}" ] && has_font=1 && break
		done
		[ "${has_font}" -eq 1 ] && break
	done
	[ "${has_font}" -eq 1 ] || continue

	# Accept either upstream naming convention.
	lic=""
	for cand in "${dir}OFL.txt" "${dir}LICENSE.txt" "${dir}LICENSE" "${dir}UFL.txt"; do
		[ -f "${cand}" ] && lic="${cand}" && break
	done

	if [ -z "${lic}" ]; then
		missing="${missing} ${name}"
		continue
	fi

	mkdir -p "${DEST}/licenses/fonts/${name}"
	cp "${lic}" "${DEST}/licenses/fonts/${name}/$(basename "${lic}")"
	count=$((count + 1))
done

if [ -n "${missing}" ]; then
	echo "collect-licenses: FATAL: bundled font(s) with no license text:${missing}" >&2
	echo "collect-licenses: OFL-1.1 s2 requires the license to accompany every copy." >&2
	exit 1
fi

if [ "${count}" -eq 0 ]; then
	echo "collect-licenses: FATAL: no font licenses collected — refusing to" >&2
	echo "collect-licenses: produce an artifact that claims complete licensing." >&2
	exit 1
fi

# A short pointer so a user unpacking the archive knows what they are looking at.
cat > "${DEST}/licenses/README.txt" <<'EOF'
Third-party license texts for components embedded in the C0PL4ND binary.

fonts/  Per-typeface license text. C0PL4ND compiles these typefaces into the
        executable, so the SIL Open Font License 1.1 (section 2) requires this
        text to accompany every copy of the software.

See THIRD-PARTY-LICENSES.md in the parent directory for the full index,
including each font's upstream source and copyright line.
EOF

echo "collect-licenses: staged ${count} font license(s) + ${DEST}/THIRD-PARTY-LICENSES.md"
