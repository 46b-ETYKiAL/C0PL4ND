#!/bin/sh
# require-signing-key.sh — decide whether an ABSENT minisign key may ship.
#
# Usage (from release.yml's signing step, when MINISIGN_SECRET_KEY is empty):
#   sh src/packaging/require-signing-key.sh
#
# Exit status
# -----------
#   1  the ref is a STABLE tag  -> a stable release MUST be signed; fail loudly
#   0  prerelease tag / non-tag -> unsigned tolerated, with a ::warning::
#
# WHY THIS EXISTS
# ---------------
# The signing step opens with `if [ -z "${MINISIGN_SECRET_KEY:-}" ]`, and the
# branch it takes decides not just whether the assets are SIGNED but whether the
# step's own fail-closed self-verify gate — coverage, identity, count-parity,
# digest cross-check, tamper test — runs at all. An `exit 0` there skips the
# signing AND the entire Tier-0 verify block in one move, and the release still
# goes green while PUBLISHING artifacts the fail-closed in-app updater is built
# to REJECT. A stable release no deployed client can auto-update to is a
# release-readiness landmine that looks like a success.
#
# The classification is ref-sensitive and deliberately narrow — see
# `release-ref-class.sh`, which owns the rule and documents why "contains a
# hyphen anywhere" was wrong.
#
#   * STABLE TAG      unsigned is a HARD FAILURE. Provision the key (see
#                     packaging/signing.md) or cut a prerelease tag instead.
#   * PRERELEASE TAG  rc/pre builds are opt-in downloads, not auto-update
#                     targets. Warn and continue.
#   * NON-TAG REF     behaviour deliberately UNCHANGED: warn and continue.
#                     Whether a dispatch run should be allowed to produce
#                     unsigned artifacts at all is an open owner decision, and
#                     this script does not pre-empt it.
#
# This is a script rather than inline YAML for one reason: a gate that cannot be
# EXECUTED cannot be shown to fail, and an unfalsifiable gate is exactly the
# defect being fixed here. `packaging/falsify_release_gates.py` runs this file
# for real, in both directions, on every push.
#
# Ported from the sibling SCR1B3 editor's `packaging/require-signing-key.sh`, so
# both Itasha apps share ONE signing-gate convention.
set -eu

REF_NAME="${GITHUB_REF_NAME:-}"
CLASS="$(sh "$(dirname "$0")/release-ref-class.sh")"

case "${CLASS}" in
	non-tag)
		echo "::warning::MINISIGN_SECRET_KEY not set on a non-tag ref (${GITHUB_REF_TYPE:-?}/${REF_NAME:-?}) — shipping checksummed but UNSIGNED artifacts (auto-update will reject them). See packaging/signing.md."
		exit 0
		;;
	prerelease)
		echo "::warning::MINISIGN_SECRET_KEY not set — shipping checksummed but UNSIGNED prerelease artifacts (auto-update will reject them; acceptable for an rc/pre tag ${REF_NAME})."
		exit 0
		;;
esac

echo "::error::MINISIGN_SECRET_KEY not set on a STABLE tag (${REF_NAME:-?}). A stable release MUST be signed — the fail-closed in-app updater verifies a minisign signature before installing, so every deployed client would REJECT this release and auto-update would silently stop working. Provision the signing key (packaging/signing.md) or cut a prerelease (-rc/-pre) tag instead. Failing the release."
exit 1
