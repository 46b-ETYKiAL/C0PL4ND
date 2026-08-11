#!/bin/sh
# install.sh - one-line installer for C0PL4ND.
#
# Detects OS/arch, downloads the latest release tarball from GitHub, verifies
# it against the minisign public key EMBEDDED IN THIS SCRIPT, installs the
# binary to ~/.local/bin, and prints next steps. POSIX sh, no bashisms.
# shellcheck-clean.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/46b-ETYKiAL/C0PL4ND/master/packaging/linux/install.sh | sh
#
# Environment overrides:
#   C0PL4ND_VERSION   pin a version tag (default: latest)
#   C0PL4ND_BIN_DIR   install directory (default: $HOME/.local/bin)
#
# --- Why a signature and not just a checksum -------------------------------
# A SHA-256 sidecar fetched from the SAME host as the artifact is a corruption
# check, not a security control: whoever can serve you a malicious tarball can
# serve the matching `.sha256` in the same breath. The authenticity control is
# the Ed25519 (minisign) signature checked against PUBKEY below, which is
# embedded in this script and therefore shares the trust root you already
# accepted when you chose to run this script. The checksum is still verified
# first, as a cheap integrity/corruption gate.
#
# This script FAILS CLOSED. If no minisign-compatible verifier is available, it
# aborts with installation instructions rather than installing an unverified
# binary. There is deliberately NO environment variable to skip verification.
set -eu

# Canonical public release repository. Confirmed via
# `gh api repos/46b-ETYKiAL/C0PL4ND --jq .full_name`. The prior value
# `itasha-corp/c0pl4nd` referred to a GitHub namespace that DOES NOT EXIST
# (`gh api users/itasha-corp` -> 404), so anyone could have registered it and
# taken control of the URL this installer told users to pipe into a shell.
REPO="46b-ETYKiAL/C0PL4ND"
BIN="c0pl4nd"
BIN_DIR="${C0PL4ND_BIN_DIR:-${HOME}/.local/bin}"

# Minisign public key for release artifacts. This is the same key compiled into
# the application binary as `EMBEDDED_PUBLIC_KEY`
# (crates/app/src/update_engine/verify.rs) and published at
# packaging/minisign.pub, so the one-line installer and the in-app self-updater
# enforce the identical trust root. The matching secret key is the CI secret
# `MINISIGN_SECRET_KEY` and is never committed.
PUBKEY_ID="A8D869E2B4DD3FD9"
PUBKEY="RWTZP9204mnYqKT/TK6OfYG70QwFoHF5WuuxODg8tgPU+WdLRJYt6iNN"

err() {
	printf 'error: %s\n' "$1" >&2
	exit 1
}

info() {
	printf '%s\n' "$1"
}

need() {
	command -v "$1" >/dev/null 2>&1 || err "required tool not found: $1"
}

# --- detect a usable downloader -------------------------------------------
DOWNLOADER=""
if command -v curl >/dev/null 2>&1; then
	DOWNLOADER="curl"
elif command -v wget >/dev/null 2>&1; then
	DOWNLOADER="wget"
else
	err "neither curl nor wget is available"
fi

download() {
	# download <url> <output-path>
	if [ "${DOWNLOADER}" = "curl" ]; then
		curl -fsSL -o "$2" "$1"
	else
		wget -qO "$2" "$1"
	fi
}

fetch_stdout() {
	# fetch_stdout <url>
	if [ "${DOWNLOADER}" = "curl" ]; then
		curl -fsSL "$1"
	else
		wget -qO- "$1"
	fi
}

# --- locate a signature verifier (FAIL CLOSED) ----------------------------
# `minisign` (jedisct1) and `rsign` (rsign2, the Rust implementation the release
# workflow signs with) take different arguments, so detect which one we have.
VERIFIER=""
if command -v minisign >/dev/null 2>&1; then
	VERIFIER="minisign"
elif command -v rsign >/dev/null 2>&1; then
	VERIFIER="rsign"
else
	printf 'error: no signature verifier found — refusing to install.\n\n' >&2
	printf 'C0PL4ND release artifacts are Ed25519-signed. This installer will not\n' >&2
	printf 'install a binary it cannot verify, so please install one of:\n\n' >&2
	printf '  minisign   Debian/Ubuntu : sudo apt install minisign\n' >&2
	printf '             Fedora        : sudo dnf install minisign\n' >&2
	printf '             Arch          : sudo pacman -S minisign\n' >&2
	printf '             macOS         : brew install minisign\n' >&2
	printf '  rsign2     any platform  : cargo install rsign2\n\n' >&2
	printf 'Then re-run this installer. Alternatively download the release\n' >&2
	printf 'archive and its .minisig from\n' >&2
	printf '  https://github.com/%s/releases\n' "${REPO}" >&2
	printf 'and verify manually with public key %s.\n' "${PUBKEY_ID}" >&2
	exit 1
fi

# --- detect OS ------------------------------------------------------------
os="$(uname -s)"
case "${os}" in
	Linux) os_id="unknown-linux-gnu" ;;
	Darwin) os_id="apple-darwin" ;;
	*) err "unsupported operating system: ${os}" ;;
esac

# --- detect arch ----------------------------------------------------------
arch="$(uname -m)"
case "${arch}" in
	x86_64 | amd64) arch_id="x86_64" ;;
	aarch64 | arm64) arch_id="aarch64" ;;
	*) err "unsupported architecture: ${arch}" ;;
esac

target="${arch_id}-${os_id}"

# --- resolve version ------------------------------------------------------
version="${C0PL4ND_VERSION:-}"
if [ -z "${version}" ]; then
	info "Resolving latest release..."
	api_url="https://api.github.com/repos/${REPO}/releases/latest"
	# Parse the tag_name without requiring jq: grep the JSON field.
	version="$(fetch_stdout "${api_url}" \
		| grep '"tag_name"' \
		| head -n 1 \
		| sed -e 's/.*"tag_name"[[:space:]]*:[[:space:]]*"//' -e 's/".*//')"
	[ -n "${version}" ] || err "could not determine latest version"
fi
info "Installing C0PL4ND ${version} (${target})"

# --- build download URLs --------------------------------------------------
stage="${BIN}-${version}-${target}"
archive="${stage}.tar.gz"
base_url="https://github.com/${REPO}/releases/download/${version}"
archive_url="${base_url}/${archive}"
# Releases publish ONE signed aggregate checksum manifest (SHA256SUMS +
# SHA256SUMS.minisig) instead of a per-artifact `.sha256` / `.minisig` pair.
#
# THIS SCRIPT WAS BROKEN BY THAT CHANGE AND COULD NOT INSTALL ANY RELEASE FROM
# THE FIRST ONE THAT PRUNED THEM. It fetched `${archive}.sha256` and
# `${archive}.minisig`; the release workflow's "Prune redundant signature
# sidecars before publish" step deletes both before upload, so `download` got a
# 404 on the checksum and the `.minisig` fetch hit its explicit
# `err "no .minisig signature published ... refusing to install an unverified
# binary"`. Every `curl | sh` install aborted.
#
# Binding to SHA256SUMS is also strictly stronger than what this script did
# before: the old per-artifact `.sha256` was UNSIGNED, so the checksum gate it
# fed had no authenticity value (see the header note). SHA256SUMS is signed with
# the SAME key as the release, so its digests are AUTHENTICATED.
checksums_url="${base_url}/SHA256SUMS"
checksums_sig_url="${base_url}/SHA256SUMS.minisig"

# --- work in a temp dir ---------------------------------------------------
tmp="$(mktemp -d 2>/dev/null || mktemp -d -t c0pl4nd)"
[ -n "${tmp}" ] || err "could not create temp directory"
# shellcheck disable=SC2064
trap "rm -rf \"${tmp}\"" EXIT INT TERM

info "Downloading ${archive_url}"
# The `|| err` is not decoration. Under `set -eu`, `curl -f` on a 404 exits 22
# and the script dies HERE with no message whatsoever — the user sees the
# "Downloading ..." line above and then nothing, which is precisely how a
# release-asset naming change stayed invisible: it looks like the script hung or
# the terminal ate the output, not like a failure with a cause. The two
# downloads below always had this guard; this one did not.
download "${archive_url}" "${tmp}/${archive}" \
	|| err "could not download the release archive: ${archive_url} (the asset may not exist for this version/platform, or the network refused the request)"
[ -s "${tmp}/${archive}" ] \
	|| err "the downloaded release archive is empty: ${archive_url}"

info "Downloading checksum manifest"
download "${checksums_url}" "${tmp}/SHA256SUMS" \
	|| err "no SHA256SUMS published in release ${version} — refusing to install an unverified binary"
[ -s "${tmp}/SHA256SUMS" ] || err "checksum manifest is empty"

info "Downloading checksum-manifest signature"
download "${checksums_sig_url}" "${tmp}/SHA256SUMS.minisig" \
	|| err "no SHA256SUMS.minisig published — refusing to trust an unsigned checksum manifest"
[ -s "${tmp}/SHA256SUMS.minisig" ] \
	|| err "signature file is empty — refusing to install an unverified binary"

# --- verify the checksum manifest's signature (authenticity) ---------------
# The signature is checked against the key embedded above, NOT against a key
# fetched from the download host. This is the control that actually detects a
# malicious or substituted artifact. It runs BEFORE any digest is read out of
# SHA256SUMS, so the digest we compare against is an AUTHENTICATED value.
keyfile="${tmp}/minisign.pub"
printf 'untrusted comment: minisign public key: %s\n%s\n' "${PUBKEY_ID}" "${PUBKEY}" > "${keyfile}"

if [ "${VERIFIER}" = "minisign" ]; then
	minisign -V -p "${keyfile}" -x "${tmp}/SHA256SUMS.minisig" -m "${tmp}/SHA256SUMS" >/dev/null 2>&1 \
		|| err "SIGNATURE VERIFICATION FAILED for SHA256SUMS — the checksum manifest is not authentic. Aborting."
else
	rsign verify -p "${keyfile}" -x "${tmp}/SHA256SUMS.minisig" "${tmp}/SHA256SUMS" >/dev/null 2>&1 \
		|| err "SIGNATURE VERIFICATION FAILED for SHA256SUMS — the checksum manifest is not authentic. Aborting."
fi
info "Checksum manifest signature verified (${VERIFIER}, key ${PUBKEY_ID})."

# --- verify the archive against the SIGNED digest --------------------------
# Exact match on the filename field (either the `sha256sum` two-space form or
# the `*name` binary-mode form), never a substring grep.
expected="$(awk -v a="${archive}" '$2 == a || $2 == "*" a { print $1; exit }' "${tmp}/SHA256SUMS")"
[ -n "${expected}" ] \
	|| err "SHA256SUMS has no entry for ${archive} — refusing to install an unverified binary"

actual=""
if command -v sha256sum >/dev/null 2>&1; then
	actual="$(sha256sum "${tmp}/${archive}" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
	actual="$(shasum -a 256 "${tmp}/${archive}" | awk '{print $1}')"
else
	err "no sha256 tool found (need sha256sum or shasum)"
fi

if [ "${expected}" != "${actual}" ]; then
	err "checksum mismatch: expected ${expected}, got ${actual}"
fi
info "Archive verified against the signed checksum manifest."

# --- extract --------------------------------------------------------------
need tar
( cd "${tmp}" && tar -xzf "${archive}" )

src_bin="${tmp}/${stage}/${BIN}"
[ -f "${src_bin}" ] || err "binary not found in archive: ${src_bin}"

# --- install --------------------------------------------------------------
mkdir -p "${BIN_DIR}"
install -m 0755 "${src_bin}" "${BIN_DIR}/${BIN}" 2>/dev/null \
	|| { cp "${src_bin}" "${BIN_DIR}/${BIN}" && chmod 0755 "${BIN_DIR}/${BIN}"; }

# --- licenses -------------------------------------------------------------
# The release archive carries the license texts that must accompany every copy
# of the bundled fonts (OFL-1.1 section 2). Keep them next to the binary.
lic_src="${tmp}/${stage}/licenses"
if [ -d "${lic_src}" ]; then
	lic_dir="${BIN_DIR}/../share/c0pl4nd/licenses"
	mkdir -p "${lic_dir}" 2>/dev/null \
		&& cp -R "${lic_src}/." "${lic_dir}/" 2>/dev/null \
		&& info "License texts installed to ${lic_dir}"
fi

info ""
info "C0PL4ND installed to ${BIN_DIR}/${BIN}"

# --- next steps -----------------------------------------------------------
case ":${PATH}:" in
	*:"${BIN_DIR}":*)
		info "Run it with: ${BIN}"
		;;
	*)
		info ""
		info "${BIN_DIR} is not on your PATH. Add it by appending this line to"
		info "your shell profile (~/.profile, ~/.bashrc, or ~/.zshrc):"
		info ""
		info "    export PATH=\"${BIN_DIR}:\$PATH\""
		info ""
		info "Then restart your shell and run: ${BIN}"
		;;
esac
