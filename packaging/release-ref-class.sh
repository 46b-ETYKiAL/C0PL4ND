#!/bin/sh
# release-ref-class.sh — classify the ref a release job is running for.
#
# Prints exactly ONE word on stdout and exits 0:
#
#   stable      a tag that is NOT a SemVer prerelease  (v0.4.26, v1.0-final,
#               v2026-08-10)  -> the release real users install and auto-update
#               to; every signing gate must fail closed here.
#   prerelease  a tag carrying a real SemVer prerelease segment
#               (v0.4.26-rc.1, v0.4.26-pre, v0.5.0-hotfix) -> opt-in download.
#   non-tag     a workflow_dispatch run on a branch, or no ref at all.
#
# Reads GITHUB_REF_TYPE / GITHUB_REF_NAME, or takes them as $1 / $2.
#
# WHY THIS FILE EXISTS
# --------------------
# release.yml classified a prerelease as "the tag name contains a hyphen
# ANYWHERE" — `case "${GITHUB_REF_NAME:-}" in *-*)` in the signing step, and
# `contains(github.ref_name, '-')` on the publish step. The workflow triggers on
# `v*`, so that handed the unsigned-is-fine path to every stable tag that merely
# happened to contain a hyphen:
#
#     v2026-08-10   a date tag — not a version triple at all
#     v1.0-final    no PATCH field, so `1.0-final` is not SemVer
#     v0.5-hotfix   likewise
#
# Each of those would have PUBLISHED UNSIGNED and green, and been marked a
# GitHub prerelease into the bargain, while every deployed client rejected it.
#
# `v0.5.0-hotfix` DOES remain a prerelease: `0.5.0-hotfix` is a well-formed
# SemVer prerelease of 0.5.0, so reading it as one is correct, not a hole. Cut
# `v0.5.1` (or provision the credential) to ship it as a stable release.
#
# SemVer 2.0.0 §9: a prerelease is MAJOR.MINOR.PATCH, then `-`, then dot-
# separated identifiers of [0-9A-Za-z-], optionally followed by `+<build>`.
# The leading `v` is C0PL4ND's tag convention, not part of SemVer.
#
# Every consumer of this classification calls THIS file, so the rule has exactly
# one home and cannot drift between the minisign gate, the Windows-installer
# gate, the macOS gate and the publish step. `falsify_release_gates.py` pins it
# in both directions.
set -eu

REF_TYPE="${1:-${GITHUB_REF_TYPE:-}}"
REF_NAME="${2:-${GITHUB_REF_NAME:-}}"

if [ "${REF_TYPE}" != "tag" ]; then
	echo "non-tag"
	exit 0
fi

if printf '%s' "${REF_NAME}" | grep -qE '^v?[0-9]+\.[0-9]+\.[0-9]+-[0-9A-Za-z.-]+(\+[0-9A-Za-z.-]+)?$'; then
	echo "prerelease"
	exit 0
fi

echo "stable"
