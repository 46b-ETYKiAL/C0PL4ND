#!/bin/sh
# require-installer-signing.sh — refuse to ship an UNSIGNED Windows installer
# on a stable tag without an explicit, recorded owner decision.
#
# Usage (from release.yml's windows-installer job, after the installer is built):
#   sh packaging/require-installer-signing.sh
#
# Inputs (environment)
#   GITHUB_REF_TYPE / GITHUB_REF_NAME   classified by release-ref-class.sh
#   SIGNPATH_ORG_ID                     forwarded from `vars.SIGNPATH_ORG_ID`;
#                                       non-empty => the signing steps ran
#   UNSIGNED_WINDOWS_INSTALLER_ACK      forwarded from the same-named repo
#                                       variable; the ONLY way a stable tag may
#                                       ship an unsigned installer
#
# Exit status
#   1  stable tag, no credential, no acknowledgement  -> ::error::, fail the job
#   0  anything else                                  -> ::warning:: / ::notice::
#
# WHY THIS EXISTS
# ---------------
# Both SignPath steps in the windows-installer job are gated on
# `if: vars.SIGNPATH_ORG_ID != ''`. That variable has never been set on this
# repository, so BOTH steps have been permanently skipped since the day they
# were added — including on stable tags. Unlike the minisign step beside them
# there was no stable-tag guard at all, so v0.4.25 published
# `c0pl4nd-v0.4.25-x86_64-setup.exe` UNSIGNED, and the release was green.
#
# A skipped step in GitHub Actions renders as a neutral grey tick. Nothing in the
# log said "this installer is unsigned"; nothing said the signing path was
# unreachable. That silence is the defect. Every Windows user who downloads the
# installer meets a SmartScreen "Windows protected your PC" interstitial and must
# click through "More info" -> "Run anyway" to install, and the release process
# reported no hint that this was so.
#
# ENFORCEMENT POSTURE (deliberate)
# --------------------------------
# The credential cannot be provisioned from CI — SignPath Foundation certificates
# are granted to a project by application. So this gate does not pretend the
# installer can be signed; it makes the UNSIGNED state impossible to ship
# silently:
#
#   * STABLE TAG, no credential, no acknowledgement -> ::error:: + exit 1.
#     The release FAILS. This is the default and it is live today.
#   * STABLE TAG, no credential, acknowledgement recorded -> ::warning:: + exit 0.
#     The owner must set the repo variable UNSIGNED_WINDOWS_INSTALLER_ACK to the
#     exact sentinel below. That is a deliberate, dated, auditable decision made
#     once — not a default, and not something a workflow edit can drift into. The
#     ::warning:: is still emitted on EVERY run, so the unsigned state stays
#     visible for as long as it lasts.
#   * PRERELEASE / NON-TAG -> ::notice:: + exit 0. rc/pre installers are opt-in
#     downloads; a SmartScreen prompt on one is not a release-blocking defect.
#
# The acknowledgement is NOT a loosening of the check: the check still detects
# and announces the unsigned installer on every single run. It converts a silent
# permanent skip into an owner-recorded exception with a permanent log trail.
#
# Pinned in both directions by `packaging/falsify_release_gates.py`.
set -eu

ACK_SENTINEL='i-accept-unsigned-smartscreen-warnings'

CLASS="$(sh "$(dirname "$0")/release-ref-class.sh")"
REF_NAME="${GITHUB_REF_NAME:-?}"

# Credential present => the SignPath steps ran; nothing to announce.
if [ -n "${SIGNPATH_ORG_ID:-}" ]; then
	echo "SIGNPATH_ORG_ID is set — the installer was submitted for Authenticode signing."
	exit 0
fi

if [ "${CLASS}" != "stable" ]; then
	echo "::notice::SIGNPATH_ORG_ID is not set — this ${CLASS} installer (${REF_NAME}) is UNSIGNED and will raise a SmartScreen warning on download. Tolerated off a stable tag."
	exit 0
fi

if [ "${UNSIGNED_WINDOWS_INSTALLER_ACK:-}" = "${ACK_SENTINEL}" ]; then
	echo "::warning::Shipping an UNSIGNED Windows installer on STABLE tag ${REF_NAME}. SIGNPATH_ORG_ID is not set, so no Authenticode signature was applied and every user will meet a SmartScreen 'Windows protected your PC' interstitial. This is proceeding ONLY because the repo variable UNSIGNED_WINDOWS_INSTALLER_ACK records an explicit owner decision to accept that. Clear that variable once SignPath is provisioned."
	exit 0
fi

echo "::error::SIGNPATH_ORG_ID is not set on a STABLE tag (${REF_NAME}), so the Windows installer would publish with NO Authenticode signature. Both SignPath steps in this job are gated on that variable and were SKIPPED — silently, as a neutral grey tick. Every user downloading this installer would meet a SmartScreen 'Windows protected your PC' interstitial with no publisher name, and nothing in the release log would have said so. Resolve one of: (1) provision SignPath Foundation signing and set the SIGNPATH_ORG_ID repo variable plus the SIGNPATH_API_TOKEN secret; (2) record an explicit owner decision to ship unsigned by setting the repo variable UNSIGNED_WINDOWS_INSTALLER_ACK to '${ACK_SENTINEL}'; or (3) cut a prerelease tag. Failing the release."
exit 1
