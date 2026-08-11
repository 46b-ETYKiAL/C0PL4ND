#!/bin/sh
# Point git at the repo's tracked hooks directory.
#
# `core.hooksPath` is used rather than copying files into `.git/hooks`, so the
# hooks stay version-controlled and a later update reaches everyone who has run
# this once. Re-running is safe.

set -eu

REPO_ROOT=$(git rev-parse --show-toplevel)
cd "$REPO_ROOT"

git config core.hooksPath .githooks
chmod +x .githooks/* 2>/dev/null || true

echo "Installed: core.hooksPath -> .githooks"
echo
echo "Active hooks:"
for h in .githooks/*; do
    [ -f "$h" ] || continue
    echo "  $(basename "$h")"
done
echo
echo "The pre-push hook blocks a push that would publish personal information:"
echo "  - both guards are falsified first, so a guard that has stopped"
echo "    detecting anything cannot report a clean pass"
echo "  - tracked files are audited for paths, mailboxes, and secrets"
echo "  - every commit in the pushed range must carry an allowlisted identity"
echo
echo "Set your publishing identity if you have not already:"
echo "  git config user.email '<id>+<handle>@users.noreply.github.com'"
