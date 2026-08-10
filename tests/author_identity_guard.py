#!/usr/bin/env python3
"""Reject commits whose author/committer identity is not an allowlisted one.

Commit metadata is exactly as public as the tree, and it is the vector that
actually leaks: a mailbox committed into `%ae`/`%ce` is published the moment
the branch is pushed, and it cannot be redacted afterwards without rewriting
history. A tree-content audit cannot see it at all.

Four design decisions are load-bearing - read them before editing:

1.  **Allowlist by SHAPE, never a denylist of addresses.** Listing the
    mailboxes we want to keep out would write those exact mailboxes into a
    public file - the guard would publish the very PII it exists to suppress.
    The rule is inverted instead: an identity must MATCH a permitted shape
    (a forge `noreply` address, or an explicitly configured domain) and
    everything else is a violation, whatever it happens to be. A new leak
    class therefore needs no new pattern; it is rejected by default.

2.  **Range-scoped, not history-wide.** The default range is the push or PR
    range. Historical commits already carry mailboxes that only a history
    rewrite - an owner decision - can clear, and a gate that is permanently
    red for something no ordinary PR can fix is a gate people learn to ignore.
    Pass `--all` to audit the whole history as a reporting run.

3.  **An unresolvable range fails CLOSED.** A base that cannot be resolved
    (a force-push, a shallow clone, a first push of a new branch) falls back
    to a bounded walk of the head commit's recent ancestry rather than
    silently checking nothing. An empty range is reported as such, and
    `--require-nonempty` turns "I checked zero commits" into a failure - the
    difference between a guard that passed and a guard that never ran.

4.  **Both identities, every commit.** `git commit --amend --author=...`
    changes `%ae` alone; a rebase changes `%ce` alone. Checking one of the two
    leaves the other wide open, so both are checked on every commit.

A contributor's NAME is deliberately unconstrained. Names are public
attribution and are welcome, in commits and in the contributor list. It is
only the mailbox that must stay out - with one exception, which is why the
name fields are read at all: a name field that carries a WORKSTATION ACCOUNT
NAME, a home path, or an internal token is not attribution, it is the same
leak the content-safety audit exists to catch, wearing a different field.
`git` fills `user.name` from the OS account by default, so this is the
accidental case, not the adversarial one. The name is therefore passed through
the content-safety audit's scanner: whatever that scanner already refuses is
refused here too, and every other name is accepted untouched.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

# Identity shapes that expose no mailbox and are therefore permitted.
#
#   1. Forge-generated noreply identities - `<id>+<handle>@users.noreply.
#      github.com` and the bare `noreply@github.com` a web-UI commit carries.
#      These are the addresses the forge issues precisely so a real mailbox
#      never has to be published.
#   2. Bot identities, which are forge accounts and carry no personal mailbox.
ALLOWED_IDENTITY_PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    (
        "github noreply",
        re.compile(r"^(?:[0-9]+\+)?[A-Za-z0-9._%+\-]+@users\.noreply\.github\.com$", re.I),
    ),
    ("github web-ui noreply", re.compile(r"^noreply@github\.com$", re.I)),
    (
        "forge bot",
        re.compile(r"^[A-Za-z0-9._%+\-]+\[bot\]@users\.noreply\.github\.com$", re.I),
    ),
    (
        "dependabot",
        re.compile(r"^(?:support@dependabot\.com|49699333\+dependabot\[bot\]@users\.noreply\.github\.com)$", re.I),
    ),
]

# How many commits to walk when a range base cannot be resolved. Bounded so a
# first push of a long-lived branch cannot turn into a full-history scan, but
# large enough that a normal feature branch is covered end to end.
FALLBACK_DEPTH = 200

# How many shas are passed to one `git show`. One process for the whole range
# is tempting and wrong: every sha adds 41 bytes to the command line, and
# Windows caps a command line at 32767 characters, so a range of a few hundred
# commits aborts the guard with an OS-level "argument list too long" rather
# than producing a verdict. `--all` on this repository (800+ commits) hit that
# exactly. The batch is kept large enough that the process cost stays
# negligible and small enough that the argv stays far under the cap.
_SHOW_CHUNK = 300

_ZERO_SHA = re.compile(r"^0{7,40}$")


def identity_is_allowed(addr: str) -> str | None:
    """Return the name of the shape that permits ``addr``, or ``None``."""
    a = addr.strip().lower()
    for name, pat in ALLOWED_IDENTITY_PATTERNS:
        if pat.match(a):
            return name
    return None


def redact(addr: str) -> str:
    """A violation message must not republish the mailbox it is reporting."""
    a = addr.strip()
    if "@" not in a:
        return f"{a[:2]}***" if a else "<empty>"
    local, _, domain = a.partition("@")
    dparts = domain.split(".")
    dmask = (dparts[0][:1] + "***") if dparts[0] else "***"
    tld = "." + ".".join(dparts[1:]) if len(dparts) > 1 else ""
    return f"{local[:2]}***@{dmask}{tld}"


def name_findings(name: str) -> list[str]:
    """Violation CLASSES carried by a commit name field, or an empty list.

    Delegates to the content-safety audit rather than re-deriving the rules:
    the account name is stored there only as a salted digest, so duplicating
    the check here would mean either duplicating the digest table or writing
    the plaintext into a second public file.

    Fails CLOSED. The audit sits beside this module and the falsification
    suite asserts that adjacency; if it cannot be imported, the name has NOT
    been checked, and reporting that is the only honest outcome.
    """
    if not name.strip():
        return []
    try:
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        import content_safety_audit as csa
    except ImportError as e:  # pragma: no cover - exercised by the suite
        return [f"name could not be checked ({e})"]
    # Strip the "origin:line: " prefix: the class is what may be printed, and
    # the name itself must never be echoed back into a log.
    return [f.split(": ", 1)[-1] for f in csa.scan_text(name, "name")]


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True,
        encoding="utf-8", errors="replace", check=True,
    ).stdout


def _rev_ok(rev: str) -> bool:
    if not rev or _ZERO_SHA.match(rev):
        return False
    try:
        git("rev-parse", "--verify", f"{rev}^{{commit}}")
        return True
    except subprocess.CalledProcessError:
        return False


def resolve_range(base: str, head: str) -> tuple[list[str], str]:
    """Commits to audit, plus a human description of how they were chosen."""
    head = head or "HEAD"
    if not _rev_ok(head):
        head = "HEAD"

    if _rev_ok(base):
        try:
            out = git("rev-list", f"{base}..{head}")
            return ([c for c in out.split() if c], f"{base[:12]}..{head[:12]}")
        except subprocess.CalledProcessError:
            pass

    # Base unresolvable (force-push, shallow clone, or a brand-new branch).
    # Fall back to a bounded ancestry walk rather than checking nothing - but
    # exclude what is already on a remote.
    #
    # `--not --remotes` is what keeps this consistent with the resolvable-base
    # case above. A plain `rev-list -n<N> <head>` walks into commits that were
    # published long ago, and this repository's history carries identities that
    # only a history rewrite - an owner decision - can clear. The first push of
    # any new branch would therefore be blocked by commits the pusher did not
    # write and cannot fix, which is exactly the permanently-red gate that
    # design decision 2 exists to prevent. Scoping to unpublished commits keeps
    # the guard on what this push actually makes public.
    #
    # With no remote refs configured, `--not --remotes` excludes nothing and
    # this degrades to the plain bounded walk on its own.
    try:
        out = git("rev-list", f"-n{FALLBACK_DEPTH}", head, "--not", "--remotes")
        scope = "not yet on any remote"
    except subprocess.CalledProcessError:
        out = git("rev-list", f"-n{FALLBACK_DEPTH}", head)
        scope = "ancestry"
    return (
        [c for c in out.split() if c],
        f"<= {FALLBACK_DEPTH} commit(s) of {head[:12]}, {scope} "
        "(base unresolvable)",
    )


def audit_commits(shas: list[str]) -> list[str]:
    """One violation line per (commit, offending role)."""
    if not shas:
        return []
    out: list[str] = []
    seen = 0
    # Batched: one `git show` per commit would be O(n) processes, and one
    # `git show` for the whole range overflows the OS argv cap (see
    # `_SHOW_CHUNK`). Every chunk is parsed, so no commit is skipped.
    for i in range(0, len(shas), _SHOW_CHUNK):
        chunk = shas[i : i + _SHOW_CHUNK]
        raw = git(
            "show", "--no-patch", "--format=%H%x1f%an%x1f%ae%x1f%cn%x1f%ce%x1e", *chunk
        )
        for rec in raw.split("\x1e"):
            rec = rec.strip()
            if not rec:
                continue
            parts = rec.split("\x1f")
            if len(parts) != 5:
                continue
            sha, an, ae, cn, ce = parts
            seen += 1
            for role, name, addr in (("author", an, ae), ("committer", cn, ce)):
                if not addr.strip():
                    out.append(f"{sha[:12]}: empty {role} identity")
                elif identity_is_allowed(addr) is None:
                    out.append(
                        f"{sha[:12]}: {role} identity is not allowlisted "
                        f"({redact(addr)})"
                    )
                for cls in name_findings(name):
                    out.append(f"{sha[:12]}: {role} NAME carries a {cls}")
    if seen != len(shas):
        # Fail CLOSED. Fewer commits parsed than requested means the batching
        # silently dropped some, and a guard that checked a subset while
        # reporting on the whole range is indistinguishable from one that
        # passed.
        out.append(
            f"internal: parsed {seen} of {len(shas)} commit(s); the range was "
            "not fully checked"
        )
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Reject commits whose identity is not allowlisted."
    )
    ap.add_argument("--base", default="", help="range base (exclusive)")
    ap.add_argument("--head", default="HEAD", help="range head (inclusive)")
    ap.add_argument("--all", action="store_true", help="audit the whole history")
    ap.add_argument(
        "--require-nonempty",
        action="store_true",
        help="fail if the resolved range contains no commits",
    )
    args = ap.parse_args(argv)

    if args.all:
        shas = [c for c in git("rev-list", "--all").split() if c]
        how = "entire history"
    else:
        shas, how = resolve_range(args.base, args.head)

    print(f"author-identity: checking {len(shas)} commit(s) [{how}]")

    if not shas:
        if args.require_nonempty:
            print("\nFAIL - the resolved range is empty; nothing was checked.")
            return 1
        print("PASS - empty range, nothing to check.")
        return 0

    violations = audit_commits(shas)
    if violations:
        print(f"\nFAIL - {len(violations)} disallowed commit identity/identities:\n")
        for v in violations:
            print(f"  {v}")
        print(
            "\nCommit metadata is public and cannot be redacted after a push.\n"
            "Set a forge noreply address and re-author the offending commits:\n"
            "  git config user.email '<id>+<handle>@users.noreply.github.com'\n"
            "  git rebase -r --reset-author-date --exec "
            "'git commit --amend --no-edit --reset-author' <base>"
        )
        return 1

    print("PASS - every commit in range carries an allowlisted identity.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
