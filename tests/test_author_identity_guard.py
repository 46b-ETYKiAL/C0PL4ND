#!/usr/bin/env python3
"""Falsification suite for the commit author/committer identity guard.

Every identity class the guard claims to reject is fed to it here and asserted
to be REJECTED, and every identity that must keep working is asserted to be
ACCEPTED. A guard with no negative cases is a guard nobody can safely tighten,
and an allowlist with no rejection cases is indistinguishable from one that
accepts everything.

IMPORTANT - why the rejected samples are assembled from fragments:
    This file is a tracked file and the content-safety audit scans it like any
    other. A real-looking mailbox written as one literal would be an actual
    leak sitting in the repository - the guard would be publishing the very
    PII it exists to suppress. Each sample is therefore split across a
    concatenation so no mailbox-shaped string exists in this file's own bytes,
    while the value handed to the guard at runtime is exactly the identity we
    mean to test. Keep this property when adding cases: assemble, never inline.

    The domains used below are ordinary consumer-mail domains chosen because
    they are NOT reserved: a reserved domain (example.com) would be exempt
    under the content-safety audit's documentation-domain rule, and the case
    would then pass for the wrong reason.

Run:  python tests/test_author_identity_guard.py
      (or: python -m pytest tests/test_author_identity_guard.py)
"""

from __future__ import annotations

import subprocess
import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE))

import author_identity_guard as aig  # noqa: E402

_AT = "@"

# (description, identity) - each MUST be rejected by the allowlist.
MUST_REJECT: list[tuple[str, str]] = [
    # The class that actually leaked: a personal mailbox on a consumer host.
    ("personal mailbox, privacy host", "someone" + _AT + "proton" + ".me"),
    ("personal mailbox, short privacy host", "a.person" + _AT + "pm" + ".me"),
    ("personal mailbox, freemail", "who" + _AT + "gmail" + ".com"),
    ("personal mailbox, freemail 2", "who" + _AT + "outlook" + ".com"),
    # A corporate mailbox is still a mailbox: allowlisted DOMAINS are not the
    # rule, allowlisted SHAPES are, and a real inbox is a real inbox.
    ("company mailbox", "first.last" + _AT + "acme-corp" + ".io"),
    # Near-misses on the noreply shape. Each is one character/segment away from
    # being permitted, which is exactly where a sloppy regex silently opens up.
    ("noreply lookalike, wrong host", "user" + _AT + "users.noreply.github" + ".io"),
    ("noreply lookalike, missing subdomain", "user" + _AT + "noreply.github" + ".com"),
    ("noreply as a subdomain of an attacker host", "user" + _AT + "users.noreply.github.com" + ".evil" + ".net"),
    ("noreply string only in the local part", "users.noreply.github.com" + _AT + "gmail" + ".com"),
    # Structurally broken identities must not slip through as "not matching a
    # violation pattern" - the allowlist is positive, so these fail closed.
    ("empty identity", ""),
    ("whitespace identity", "   "),
    ("no at-sign", "not-an-email"),
    ("local part only", "someone" + _AT),
]

# (description, identity) - each MUST be accepted.
MUST_ACCEPT: list[tuple[str, str]] = [
    ("canonical numeric-prefixed noreply", "133311911+46b-ETYKiAL@users.noreply.github.com"),
    ("bare handle noreply", "46b-ETYKiAL@users.noreply.github.com"),
    ("noreply, different contributor", "12345+someone-else@users.noreply.github.com"),
    ("noreply, mixed case host", "someone@Users.NoReply.GitHub.Com"),
    ("web-ui commit identity", "noreply@github.com"),
    ("forge bot", "github-actions[bot]@users.noreply.github.com"),
    ("dependabot noreply", "49699333+dependabot[bot]@users.noreply.github.com"),
]


# (description, name) - each MUST be refused as a commit NAME field. Assembled
# from fragments for the same reason as the mailboxes above.
MUST_REJECT_NAME: list[tuple[str, str]] = [
    # The default `user.name` git derives from the workstation account. This is
    # the accidental case and by far the most common: 400+ commits already on
    # the public branch carry it.
    ("workstation account name", "." + "46b" + "_"),
    ("account name inside a longer name", "build-agent (" + "." + "46b" + "_" + ")"),
    # A name field is free text, so a path or a mailbox can land in it.
    ("home path in the name field", "/home" + "/j.smith"),
    ("mailbox in the name field", "who@gmail" + ".com"),
    ("internal tooling reference in the name field", "." + "s4f3-data runner"),
]

# (description, name) - each MUST be accepted. Contributor names are welcome.
MUST_ACCEPT_NAME: list[tuple[str, str]] = [
    ("the canonical handle", "46b-ETYKiAL"),
    ("an ordinary personal name", "Ada Lovelace"),
    ("a name with punctuation", "J. Random O'Hacker-Smith"),
    ("a name with digits", "user2049"),
    ("a forge bot", "dependabot[bot]"),
    ("the forge itself", "GitHub"),
    ("a non-ascii name", "Ana Gonzalez"),
    ("empty name is reported by the identity rule, not the name rule", ""),
]


def test_rejects_every_name_that_leaks() -> None:
    for name, sample in MUST_REJECT_NAME:
        assert aig.name_findings(sample), f"a leaking name was accepted: {name}"


def test_accepts_every_ordinary_contributor_name() -> None:
    for name, sample in MUST_ACCEPT_NAME:
        assert not aig.name_findings(sample), f"an ordinary name was refused: {name}"


def test_name_findings_never_echo_the_name() -> None:
    """Reporting a leaking name by quoting it republishes the leak."""
    for _desc, sample in MUST_REJECT_NAME:
        for cls in aig.name_findings(sample):
            assert sample not in cls, f"the name was echoed back: {cls}"


def test_name_check_fails_closed_when_the_audit_is_unreachable() -> None:
    """An uncheckable name must be reported, never silently accepted.

    The name rules live in the content-safety audit (the account name is only
    ever a salted digest, so there is nothing to duplicate here). If that
    module cannot be imported the name has NOT been checked, and returning an
    empty finding list would make "I could not look" indistinguishable from
    "I looked and it was clean" - the gate-you-never-run failure.
    """
    import importlib.abc
    import importlib.machinery

    class _Blackhole(importlib.abc.MetaPathFinder):
        def find_spec(self, fullname, path=None, target=None):  # noqa: ANN001
            if fullname == "content_safety_audit":
                raise ImportError("blackholed for the fail-closed test")
            return None

    blackhole = _Blackhole()
    saved = sys.modules.pop("content_safety_audit", None)
    sys.meta_path.insert(0, blackhole)
    try:
        findings = aig.name_findings("Ada Lovelace")
    finally:
        sys.meta_path.remove(blackhole)
        if saved is not None:
            sys.modules["content_safety_audit"] = saved

    assert findings, "an unreachable audit produced no finding (fails OPEN)"
    assert any("could not be checked" in f for f in findings), (
        f"the failure was not reported as uncheckable: {findings}"
    )
    # ...and the mechanism must be restored, or every later case is vacuous.
    assert not aig.name_findings("Ada Lovelace"), "the blackhole leaked"
    assert aig.name_findings("." + "46b" + "_"), "the audit did not come back"


def test_guard_catches_a_clean_identity_with_a_leaking_name() -> None:
    """An allowlisted address must not launder a leaking name field.

    `git` fills `user.name` from the OS account by default, so a contributor
    who sets only `user.email` to a forge noreply address still publishes
    their workstation account name on every commit. The address half of this
    guard cannot see that at all.
    """
    import os
    import tempfile

    good = "133311911+46b-ETYKiAL@users.noreply.github.com"
    leaky_name = "." + "46b" + "_"
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        env = {**os.environ, "GIT_COMMITTER_NAME": leaky_name,
               "GIT_COMMITTER_EMAIL": good}

        def g(*a: str) -> None:
            subprocess.run(["git", *a], cwd=tmp, check=True,
                           capture_output=True, text=True, env=env)

        g("init", "-q", "-b", "main")
        g("config", "user.name", leaky_name)
        g("config", "user.email", good)
        g("commit", "-q", "--allow-empty", "-m", "base")
        base = subprocess.run(["git", "rev-parse", "HEAD"], cwd=tmp, check=True,
                              capture_output=True, text=True, env=env).stdout.strip()
        g("commit", "-q", "--allow-empty", "-m", "clean address, leaking name")

        # Guard the fixture: if the address were NOT allowlisted the case would
        # pass for the wrong reason - it would prove nothing about names.
        ae, ce = subprocess.run(
            ["git", "show", "--no-patch", "--format=%ae%n%ce", "HEAD"],
            cwd=tmp, check=True, capture_output=True, text=True, env=env,
        ).stdout.split()
        assert aig.identity_is_allowed(ae) is not None, "fixture author address is dirty"
        assert aig.identity_is_allowed(ce) is not None, "fixture committer address is dirty"

        real_root = aig.ROOT
        try:
            aig.ROOT = tmp
            shas, _how = aig.resolve_range(base, "HEAD")
            violations = aig.audit_commits(shas)
            rc = aig.main(["--base", base, "--head", "HEAD"])
        finally:
            aig.ROOT = real_root
    assert rc == 1, "a leaking name passed behind an allowlisted address"
    assert any("NAME carries" in v for v in violations), (
        f"the violation was not attributed to the name field: {violations}"
    )


def test_rejects_every_non_allowlisted_identity() -> None:
    for name, ident in MUST_REJECT:
        assert aig.identity_is_allowed(ident) is None, f"wrongly allowed: {name}"


def test_accepts_every_publishable_identity() -> None:
    for name, ident in MUST_ACCEPT:
        assert aig.identity_is_allowed(ident) is not None, f"wrongly rejected: {name}"


def test_violation_messages_do_not_republish_the_mailbox() -> None:
    """A guard that prints the PII it caught has leaked it into the CI log."""
    for _name, ident in MUST_REJECT:
        red = aig.redact(ident)
        if not ident.strip():
            # A blank identity carries nothing to redact; it is reported as
            # such. Asserting a mask here would be asserting on nothing.
            assert red == "<empty>", f"blank identity mis-rendered: {red}"
            continue
        if "@" in ident and len(ident.strip()) > 4:
            local, _, domain = ident.partition("@")
            assert local not in red or len(local) <= 2, f"local part republished: {red}"
            assert domain.split(".")[0] not in red or len(domain.split(".")[0]) <= 1, (
                f"domain republished: {red}"
            )
        assert "***" in red, f"redaction produced no mask: {red}"


def test_this_suite_carries_no_literal_leak() -> None:
    """The corpus must be assembled, never inlined (see the module docstring).

    Enforced against the content-safety audit itself, so this suite cannot
    become the leak. The audit must be present - its absence is a failure, not
    a silent skip.
    """
    audit = _HERE / "content_safety_audit.py"
    assert audit.is_file(), "content_safety_audit.py must sit beside this suite"
    sys.path.insert(0, str(_HERE))
    import content_safety_audit as csa

    me = Path(__file__).resolve()
    findings = csa.scan_text(me.read_text(encoding="utf-8"), me.name)
    assert not findings, f"this suite leaks its own samples: {findings}"


def test_empty_range_can_be_made_a_failure() -> None:
    """"I checked zero commits" must be distinguishable from "I passed".

    Without `--require-nonempty` a misconfigured range would report PASS while
    checking nothing - a gate that cannot fail because it never ran.
    """
    rc_lenient = aig.main(["--base", "HEAD", "--head", "HEAD"])
    rc_strict = aig.main(["--base", "HEAD", "--head", "HEAD", "--require-nonempty"])
    assert rc_lenient == 0, "an empty range should pass by default"
    assert rc_strict == 1, "--require-nonempty must fail on an empty range"


def test_guard_actually_fails_on_a_real_bad_commit() -> None:
    """End-to-end proof the guard exits non-zero on a genuinely bad commit.

    The allowlist unit-cases above prove the predicate; this proves the whole
    program - range resolution, batching, exit code - goes RED for a commit
    that really carries a non-allowlisted identity. A disposable throwaway repo
    is used so nothing touches the real one.
    """
    import tempfile

    bad = "throwaway" + _AT + "not-allowlisted" + ".test"
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)

        def g(*a: str) -> None:
            subprocess.run(["git", *a], cwd=tmp, check=True,
                           capture_output=True, text=True)

        g("init", "-q", "-b", "main")
        g("config", "user.name", "Throwaway")
        g("config", "user.email", bad)
        g("commit", "-q", "--allow-empty", "-m", "base")
        base = subprocess.run(["git", "rev-parse", "HEAD"], cwd=tmp, check=True,
                              capture_output=True, text=True).stdout.strip()
        g("commit", "-q", "--allow-empty", "-m", "bad identity")

        real_root = aig.ROOT
        try:
            aig.ROOT = tmp
            rc = aig.main(["--base", base, "--head", "HEAD"])
        finally:
            aig.ROOT = real_root
    assert rc == 1, "the guard passed a commit carrying a non-allowlisted identity"


def test_guard_catches_a_committer_only_violation() -> None:
    """A clean author must not launder a dirty committer.

    `git rebase` and `git commit --amend --reset-author` each rewrite ONE of
    the two identities, so the realistic leak is a commit whose author is a
    proper noreply address while the committer is the machine's personal git
    identity. A suite whose only end-to-end case has BOTH fields dirty cannot
    tell the difference: deleting the committer check leaves it green. This
    case is the one that pins the committer half.
    """
    import os
    import tempfile

    bad = "rebaser" + _AT + "not-allowlisted" + ".test"
    good = "133311911+46b-ETYKiAL@users.noreply.github.com"
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        env = {**os.environ, "GIT_COMMITTER_NAME": "Rebaser",
               "GIT_COMMITTER_EMAIL": bad}

        def g(*a: str) -> None:
            subprocess.run(["git", *a], cwd=tmp, check=True,
                           capture_output=True, text=True, env=env)

        g("init", "-q", "-b", "main")
        g("config", "user.name", "Contributor")
        g("config", "user.email", good)
        g("commit", "-q", "--allow-empty", "-m", "base")
        base = subprocess.run(["git", "rev-parse", "HEAD"], cwd=tmp, check=True,
                              capture_output=True, text=True, env=env).stdout.strip()
        g("commit", "-q", "--allow-empty", "-m", "clean author, dirty committer")

        # Guard the fixture itself: if git ignored the committer override the
        # case would be vacuous - it would pass while testing nothing.
        ae, ce = subprocess.run(
            ["git", "show", "--no-patch", "--format=%ae%n%ce", "HEAD"],
            cwd=tmp, check=True, capture_output=True, text=True, env=env,
        ).stdout.split()
        assert aig.identity_is_allowed(ae) is not None, "fixture author is not clean"
        assert aig.identity_is_allowed(ce) is None, "fixture committer is not dirty"

        real_root = aig.ROOT
        try:
            aig.ROOT = tmp
            rc = aig.main(["--base", base, "--head", "HEAD"])
        finally:
            aig.ROOT = real_root
    assert rc == 1, "a dirty committer passed behind a clean author"


def test_unresolvable_base_checks_unpublished_commits_and_only_those() -> None:
    """The fallback must cover what a push publishes - and nothing older.

    Two failure modes bracket this, and a test that only covers one of them
    proves nothing:

      * Too narrow - the fallback checks nothing, so the first push of a new
        branch (remote sha all-zeros) publishes an unchecked mailbox. That is
        the guard never running.
      * Too wide - the fallback walks into already-published ancestry, so the
        gate is red for commits the pusher did not write and cannot fix
        without rewriting history for everyone. That is the gate people learn
        to ignore, and this repository's own history triggers it.

    Both directions are asserted here against one fixture.
    """
    import tempfile

    bad = "unpublished" + _AT + "not-allowlisted" + ".test"
    good = "133311911+46b-ETYKiAL@users.noreply.github.com"
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)

        def g(*a: str) -> str:
            return subprocess.run(["git", *a], cwd=tmp, check=True,
                                  capture_output=True, text=True).stdout.strip()

        g("init", "-q", "-b", "main")
        g("config", "user.name", "Historic")
        g("config", "user.email", bad)
        # An OLD commit with a bad identity, already published.
        g("commit", "-q", "--allow-empty", "-m", "old, already public")
        published = g("rev-parse", "HEAD")
        g("update-ref", "refs/remotes/origin/main", published)

        # A NEW commit with a bad identity, not on any remote.
        g("config", "user.email", good)
        g("commit", "-q", "--allow-empty", "-m", "clean and new")
        g("config", "user.email", bad)
        g("commit", "-q", "--allow-empty", "-m", "dirty and new")
        unpublished_bad = g("rev-parse", "HEAD")

        real_root = aig.ROOT
        try:
            aig.ROOT = tmp
            # An all-zero remote sha is what git feeds a pre-push hook for a
            # branch the remote has never seen.
            shas, how = aig.resolve_range("0" * 40, "HEAD")
            rc = aig.main(["--base", "0" * 40, "--head", "HEAD"])
        finally:
            aig.ROOT = real_root

    assert unpublished_bad in shas, f"the unpublished bad commit was not checked [{how}]"
    assert published not in shas, (
        f"the fallback walked into already-published ancestry [{how}]"
    )
    assert rc == 1, "an unpublished non-allowlisted identity was not rejected"


def test_a_range_larger_than_one_batch_is_fully_checked() -> None:
    """A multi-chunk range must not silently check only the first chunk.

    `git show <sha> ...` for a whole range overflows the OS argv cap - on
    Windows `--all` over this repository's history aborted with an OS error
    instead of a verdict. The fix batches the shas, which introduces the
    opposite risk: a batching bug that parses one chunk and reports on all of
    them. This drives a range that spans several chunks (by shrinking the
    batch, not by making hundreds of commits) and asserts every offending
    commit is reported.
    """
    import tempfile

    bad = "batched" + _AT + "not-allowlisted" + ".test"
    n_commits = 7
    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)

        def g(*a: str) -> None:
            subprocess.run(["git", *a], cwd=tmp, check=True,
                           capture_output=True, text=True)

        g("init", "-q", "-b", "main")
        g("config", "user.name", "Batched")
        g("config", "user.email", bad)
        g("commit", "-q", "--allow-empty", "-m", "base")
        base = subprocess.run(["git", "rev-parse", "HEAD"], cwd=tmp, check=True,
                              capture_output=True, text=True).stdout.strip()
        for i in range(n_commits):
            g("commit", "-q", "--allow-empty", "-m", f"c{i}")

        real_root, real_chunk = aig.ROOT, aig._SHOW_CHUNK
        try:
            aig.ROOT = tmp
            aig._SHOW_CHUNK = 2  # forces 4 batches over 7 commits
            shas, _how = aig.resolve_range(base, "HEAD")
            assert len(shas) == n_commits, f"fixture range is {len(shas)}, not {n_commits}"
            violations = aig.audit_commits(shas)
        finally:
            aig.ROOT, aig._SHOW_CHUNK = real_root, real_chunk

    # Both roles are dirty on every commit, so every commit yields two lines.
    assert len(violations) == 2 * n_commits, (
        f"batching dropped commits: {len(violations)} lines for {n_commits} commits"
    )
    assert not any("not fully checked" in v for v in violations), (
        f"the completeness check fired unexpectedly: {violations}"
    )


def _main() -> int:
    failures = 0
    for name, ident in MUST_REJECT:
        if aig.identity_is_allowed(ident) is not None:
            print(f"FAIL  expected rejection, got acceptance: {name}")
            failures += 1
    for name, ident in MUST_ACCEPT:
        if aig.identity_is_allowed(ident) is None:
            print(f"FAIL  expected acceptance, got rejection: {name}")
            failures += 1
    for name, sample in MUST_REJECT_NAME:
        if not aig.name_findings(sample):
            print(f"FAIL  a leaking name was accepted: {name}")
            failures += 1
    for name, sample in MUST_ACCEPT_NAME:
        if aig.name_findings(sample):
            print(f"FAIL  an ordinary name was refused: {name}")
            failures += 1
    for fn in (
        test_name_findings_never_echo_the_name,
        test_name_check_fails_closed_when_the_audit_is_unreachable,
        test_guard_catches_a_clean_identity_with_a_leaking_name,
        test_violation_messages_do_not_republish_the_mailbox,
        test_this_suite_carries_no_literal_leak,
        test_empty_range_can_be_made_a_failure,
        test_guard_actually_fails_on_a_real_bad_commit,
        test_guard_catches_a_committer_only_violation,
        test_unresolvable_base_checks_unpublished_commits_and_only_those,
        test_a_range_larger_than_one_batch_is_fully_checked,
    ):
        try:
            fn()
        except AssertionError as e:
            print(f"FAIL  {fn.__name__}: {e}")
            failures += 1
    print(
        f"author-identity falsification: {len(MUST_REJECT)} reject-cases, "
        f"{len(MUST_ACCEPT)} accept-cases, "
        f"{len(MUST_REJECT_NAME)} name-reject-cases, "
        f"{len(MUST_ACCEPT_NAME)} name-accept-cases, {failures} failure(s)"
    )
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(_main())
