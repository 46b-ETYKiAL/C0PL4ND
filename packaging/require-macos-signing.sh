#!/bin/sh
# require-macos-signing.sh — observe the REAL signing state of the built .app,
# announce it, and refuse to ship an unsigned/unnotarized one on a stable tag
# without an explicit, recorded owner decision.
#
# Usage (from release.yml's macos-installer job, after the bundle is built):
#   sh app/packaging/require-macos-signing.sh C0PL4ND.app
#
# Inputs (environment)
#   GITHUB_REF_TYPE / GITHUB_REF_NAME  classified by release-ref-class.sh
#   UNSIGNED_MACOS_APP_ACK             forwarded from the same-named repo
#                                      variable; the ONLY way a stable tag may
#                                      ship an unsigned/unnotarized bundle
#
# Exit status
#   1  stable tag, not notarized, no acknowledgement -> ::error::, fail the job
#   1  the bundle is missing, or the probe cannot run -> ::error::, fail closed
#   0  otherwise, having ANNOUNCED the observed state
#
# WHY THIS EXISTS
# ---------------
# The macos-installer job contained no `codesign`, no `notarytool`, no `stapler`
# and no `spctl` — not a disabled step, not a gated step, nothing. The .dmg it
# produces is unsigned and unnotarized, so macOS attaches
# `com.apple.quarantine` to it on download and Gatekeeper refuses the app on
# first launch ("cannot be opened because the developer cannot be verified", and
# on Apple Silicon frequently the harsher "is damaged and can't be opened"). The
# release published green and said nothing about any of it.
#
# The .dmg job was added on 2026-08-05, AFTER the most recent release (v0.4.25,
# 2026-07-20), so no .dmg has shipped yet. The next stable tag is the first one —
# which is exactly when this needs to be true rather than after the fact.
#
# WHY THIS PROBES RATHER THAN ASSUMES
# -----------------------------------
# A comment saying "the artifact is unsigned" is a claim, not evidence, and it
# rots the moment anything changes: a future `codesign` step, a runner-image
# change, or a linker that ad-hoc-signs by default all silently invalidate it.
# So this reads the state off the artifact itself with `codesign` and `spctl` and
# reports what it actually finds. The documented state is therefore ASSERTED on
# every run, in both directions:
#
#   unsigned    codesign finds no signature at all.
#   adhoc       an ad-hoc signature (Signature=adhoc), no identity. This is what
#               the Rust/Apple linker emits for arm64 and is what lets the binary
#               execute at all on Apple Silicon — it is NOT Gatekeeper-acceptable.
#   identity    a real signing identity (TeamIdentifier / Developer ID Authority).
#   notarized   spctl accepts it, source=Notarized Developer ID.
#
# ENFORCEMENT POSTURE (deliberate)
# --------------------------------
# An Apple Developer ID cannot be provisioned from CI — it needs a paid Apple
# Developer Program membership and an out-of-band identity. So this gate does not
# pretend the app can be notarized; it makes the un-notarized state impossible to
# ship silently:
#
#   * STABLE TAG, not notarized, no acknowledgement -> ::error:: + exit 1.
#     The release FAILS. This is the default and it is live today.
#   * STABLE TAG, not notarized, acknowledgement recorded -> ::warning:: + exit 0,
#     restating the observed state and the exact user-visible consequence. The
#     owner sets the repo variable UNSIGNED_MACOS_APP_ACK to the sentinel below:
#     one deliberate, auditable decision, re-announced on every run.
#   * PRERELEASE / NON-TAG -> ::notice:: + exit 0.
#
# `identity` WITHOUT notarization on a stable tag is ALSO a failure, and is not
# acknowledgement-eligible: a codesigned-but-unnotarized app is still refused by
# Gatekeeper, so it buys nothing while looking like it did. If an identity
# appears, finish the job — add `notarytool submit --wait` and `stapler staple`.
#
# Pinned in both directions by `packaging/falsify_release_gates.py`, which stubs
# `codesign`/`spctl` to replay each state.
set -eu

ACK_SENTINEL='i-accept-gatekeeper-quarantine'

APP="${1:-}"
[ -n "${APP}" ] || { echo "::error::require-macos-signing.sh needs the .app bundle path as \$1"; exit 1; }
[ -e "${APP}" ] || { echo "::error::require-macos-signing.sh: no such bundle '${APP}' — cannot verify the signing state of an artifact that is not there. Failing closed."; exit 1; }

command -v codesign >/dev/null 2>&1 || { echo "::error::codesign is not on PATH — the macOS signing state cannot be observed, so it cannot be asserted. A gate that cannot run is not a gate. Failing closed."; exit 1; }

# --- observe -----------------------------------------------------------------
cs_out="$(codesign -dv --verbose=4 "${APP}" 2>&1 || true)"

if printf '%s' "${cs_out}" | grep -qi 'not signed at all'; then
	state='unsigned'
elif printf '%s' "${cs_out}" | grep -qi 'Signature=adhoc'; then
	state='adhoc'
elif printf '%s' "${cs_out}" | grep -qiE 'TeamIdentifier=[A-Z0-9]|Authority=Developer ID'; then
	state='identity'
else
	# An unrecognised codesign dialect is NOT "probably fine". Refusing here is
	# what stops this gate degrading into the silence it was written to remove.
	echo "::error::could not classify the signing state of ${APP} from codesign output. Failing closed rather than guessing. codesign said:"
	printf '%s\n' "${cs_out}"
	exit 1
fi

# Gatekeeper's own verdict, when it can be asked. `spctl` is the authority on
# whether a user can actually open this; codesign only describes the signature.
gk='unknown'
if command -v spctl >/dev/null 2>&1; then
	sp_out="$(spctl --assess --type execute --verbose=4 "${APP}" 2>&1 || true)"
	if printf '%s' "${sp_out}" | grep -qi 'source=Notarized Developer ID'; then
		gk='notarized'
	elif printf '%s' "${sp_out}" | grep -qi 'accepted'; then
		gk='accepted-not-notarized'
	else
		gk='rejected'
	fi
	printf 'spctl: %s\n' "${sp_out}"
fi

echo "macOS signing state of ${APP}: codesign=${state} gatekeeper=${gk}"
printf '%s\n' "${cs_out}"

# --- gate --------------------------------------------------------------------
if [ "${gk}" = 'notarized' ]; then
	echo "${APP} is signed and notarized — Gatekeeper will open it without a prompt."
	exit 0
fi

CLASS="$(sh "$(dirname "$0")/release-ref-class.sh")"
REF_NAME="${GITHUB_REF_NAME:-?}"

consequence="macOS attaches com.apple.quarantine to the downloaded .dmg and Gatekeeper REFUSES to open the app — the user sees 'cannot be opened because the developer cannot be verified', or on Apple Silicon often 'is damaged and can't be opened'. The documented workaround is right-click -> Open, or 'xattr -dr com.apple.quarantine /Applications/C0PL4ND.app'."

if [ "${state}" = 'identity' ]; then
	# Not acknowledgement-eligible: see the header. Signed-but-unnotarized is
	# still refused by Gatekeeper while looking like it was handled.
	if [ "${CLASS}" != "stable" ]; then
		echo "::notice::${APP} carries a signing identity but is NOT notarized (${CLASS} ${REF_NAME}). ${consequence}"
		exit 0
	fi
	echo "::error::${APP} carries a signing identity but is NOT notarized, on STABLE tag ${REF_NAME}. Since Catalina, codesigning WITHOUT notarization does not satisfy Gatekeeper: ${consequence} Add 'xcrun notarytool submit --wait' and 'xcrun stapler staple' to the macos-installer job. Failing the release."
	exit 1
fi

if [ "${CLASS}" != "stable" ]; then
	echo "::notice::${APP} is ${state} (not notarized) on a ${CLASS} ref (${REF_NAME}). ${consequence} Tolerated off a stable tag."
	exit 0
fi

if [ "${UNSIGNED_MACOS_APP_ACK:-}" = "${ACK_SENTINEL}" ]; then
	echo "::warning::Shipping a ${state}, UNNOTARIZED macOS app on STABLE tag ${REF_NAME}. ${consequence} This is proceeding ONLY because the repo variable UNSIGNED_MACOS_APP_ACK records an explicit owner decision to accept that. Ship those instructions with the release notes, and clear that variable once an Apple Developer ID is provisioned."
	exit 0
fi

echo "::error::${APP} is ${state} and NOT notarized on a STABLE tag (${REF_NAME}). The macos-installer job contains no codesign, notarytool or stapler step at all, so the published .dmg is an artifact macOS will refuse to open — while the release reports success. ${consequence} Resolve one of: (1) provision an Apple Developer ID and add codesign + 'notarytool submit --wait' + 'stapler staple' to this job; (2) record an explicit owner decision to ship un-notarized by setting the repo variable UNSIGNED_MACOS_APP_ACK to '${ACK_SENTINEL}', and publish the quarantine-removal instructions with the release notes; or (3) cut a prerelease tag. Failing the release."
exit 1
