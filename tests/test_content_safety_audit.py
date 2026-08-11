#!/usr/bin/env python3
"""Falsification suite for the public-repo content-safety audit.

Every leak class the audit claims to detect is fed to it here and asserted to
be CAUGHT, and every legitimate construct that a naive version of the same rule
would flag is asserted to stay CLEAN. A guard with no negative cases is a guard
nobody can safely tighten.

IMPORTANT - why every sample is assembled from fragments:
    This file is a tracked file, so the audit scans it like any other. A sample
    written as one literal would be a real leak sitting in the repository. Each
    sample is therefore split across a concatenation so that no leak-shaped
    string exists in the file's own bytes, while the value handed to the
    scanner at runtime is exactly the leak we mean to test. Keep this property
    when adding cases: assemble, never inline.

Run:  python tests/test_content_safety_audit.py
      (or: python -m pytest tests/test_content_safety_audit.py)
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("csa", _HERE / "content_safety_audit.py")
assert _spec and _spec.loader
csa = importlib.util.module_from_spec(_spec)
sys.modules["csa"] = csa
_spec.loader.exec_module(csa)

# Fragments. Split points are chosen so this file's own text never matches.
_WIN = "C:/Users" + "/"
_WIN_BS = "C:\\Users" + "\\"
_HOME = "/home" + "/"
_MAC = "/Users" + "/"
_DOT = "."
_BS = chr(92)
# A path written into a source literal that is ITSELF inside another literal
# is escaped twice; this is that form, assembled so it is not a leak here.
_WIN_BS4 = "C:" + (_BS * 4) + "Users" + (_BS * 4)
# Cross-OS mount prefixes. A home path under one of these identifies its owner
# exactly as well as the unmounted form does.
_MNT = "/mnt/c"
_CYG = "/cygdrive/c"
_WSL_UNC = (_BS * 2) + "wsl$" + _BS + "Ubuntu"
# The same UNC written into a source literal that is ITSELF inside another
# literal, so every separator is escaped twice. This is what pins `{1,4}` on
# the MOUNTED patterns: at `{1,2}` they cannot see this form at all, and no
# other pattern covers it (there is no drive letter for the plain Windows rule
# to anchor on), so the account name would be published unnoticed.
_WSL_UNC_BS4 = (_BS * 4) + "wsl$" + (_BS * 4) + "Ubuntu"


# (description, sample) - each MUST produce at least one finding.
MUST_CATCH: list[tuple[str, str]] = [
    # Windows user profile, BOTH separator conventions. The forward-slash form
    # is the one an earlier revision of this audit could not see at all.
    ("windows path, forward slash", f'"{_WIN}a.dev_/Documents/n.md".to_string(),'),
    ("windows path, back slash", f'let p = "{_WIN_BS}a.dev_";'),
    # Personal mailboxes - a class with no pattern at all before.
    ("personal email, consumer domain", "author = someone@proton" + ".me"),
    ("personal email, second domain", "contact: a.person@pm" + ".me"),
    ("personal email, freemail", "reviewer <who@gmail" + ".com>"),
    # The opaque-URI exemption is an explicit scheme ALLOWLIST, not a generic
    # `word:` rule. A generic rule would exempt ordinary prose that happens to
    # carry a colon and turn a real leak into a pass - strictly worse than the
    # false positive it fixes. These pin that it stayed narrow.
    ("colon-prefixed prose is not a uri scheme", "Contact:who@gmail" + ".com"),
    ("author label is not a uri scheme", "Author:a.person@pm" + ".me"),
    # Home paths must not require a trailing slash.
    ("linux home, no trailing slash", "service runs as " + _HOME + "deploy"),
    ("linux home, real account", "cd " + _HOME + "j.smith/build"),
    ("macos home", "open " + _MAC + "jbloggs/dev/x"),
    # Doubly-escaped separators. The `{1,2}` quantifier this replaced could not
    # see this form at all, so a path embedded in a nested string literal was
    # invisible to a rule that claimed to cover Windows user paths.
    ("windows path, doubly-escaped separators", 'cfg = "' + _WIN_BS4 + 'a.dev_"'),
    # Home paths reached through a cross-OS mount. Every mount prefix ends in
    # an alphanumeric, so the `(?<![A-Za-z0-9])` lookbehind on the plain
    # patterns silently made all three of these invisible.
    ("wsl mount to a windows profile", "cd " + _MNT + "/Users/" + "a.dev_/src"),
    ("cygwin mount to a windows profile", _CYG + "/Users/" + "jsmith/build"),
    ("wsl unc to a linux home", "explorer " + _WSL_UNC + "/home/" + "jsmith"),
    # The nested-literal form of both mounted shapes (see `_WSL_UNC_BS4`).
    (
        "wsl unc to a windows profile, doubly-escaped separators",
        'cfg = "' + _WSL_UNC_BS4 + (_BS * 4) + "Users" + (_BS * 4) + 'a.dev_"',
    ),
    (
        "wsl unc to a linux home, doubly-escaped separators",
        'cfg = "' + _WSL_UNC_BS4 + (_BS * 4) + "home" + (_BS * 4) + 'jsmith"',
    ),
    # Internal tooling directories that were not previously registered.
    ("tooling dir, plans", "see " + _DOT + "plans/active/x.md"),
    ("tooling dir, codex", "path: " + _DOT + "codex/config.toml"),
    # A bare fragment of the internal monorepo identifier, with no surrounding
    # path or sibling segment to give it away.
    ("bare monorepo id fragment", "the R0" + "UT3 arbiter module"),
    # The workstation account name as a BARE token, with no path around it -
    # the form no path pattern can see.
    ("os account name, bare token", "profile = " + _DOT + "46b" + "_"),
    # Internal tooling / monorepo / work-item tokens (hash-matched).
    ("tooling dir", "see " + _DOT + "s4f3-data/notes.md"),
    ("tooling dir, second", "path: " + _DOT + "claude/agents"),
    # NOTE the split points: a bare fragment of the identifier is now a
    # suppressed token in its own right, so a 3-way split would leave a
    # detectable token in THIS file's bytes. The runtime value is unchanged.
    ("monorepo id embedded in a longer path", "C:/x/Itasha.Corp_S4F3-" + "R0U" + "T3-4RB" + "1T3R/y"),
    ("work-item token", "<!-- bespoke instrument (plan-" + "611). -->"),
    # Secret shapes.
    ("private key block", "-----BEGIN OPENSSH PRIVATE " + "KEY-----"),
    ("aws access key", "AKIA" + "IOSFODNN7EXAMPLE"),
    ("github token", "ghp_" + "a" * 36),
    ("secret assignment", 'api_key = "' + "abcdefghijklmnopqrstuvwxyz0123" + '"'),
]

# (description, sample) - each MUST produce no finding at all.
MUST_NOT_FIRE: list[tuple[str, str]] = [
    # A name that shares a leading token-run with the internal monorepo id.
    # `token_probes` splits on `. _ -` and emits every contiguous run, so this
    # shape yields `itasha-corp-s4f3` (and `itasha`, `corp`, `itasha-corp`, ...)
    # - the exact prefix of the internal monorepo identifier. These cases prove
    # the suppression digests stay pinned to the WHOLE identifier: the day
    # someone suppresses a prefix instead, this fires.
    #
    # The name is built from THIS repository's own public name, not a sibling's.
    # An earlier revision pinned the sibling repo's former name here, which
    # asserted nothing about this repository: the collision it claims to cover
    # is between the monorepo id and the name this repo actually publishes.
    ("prefix-collision with the monorepo id, url", "https://github.com/46b-ETYKiAL/Itasha.Corp_S4F3-C0PL4ND/releases"),
    ("prefix-collision with the monorepo id, prose", "Itasha.Corp_S4F3-C0PL4ND is the repository"),
    # The public repo's current name, exactly as it appears in README/CI URLs.
    ("public repo url", "https://github.com/46b-ETYKiAL/C0PL4ND/releases"),
    ("public repo name in prose", "C0PL4ND is the repository"),
    # The canonical publishing identity is not PII.
    ("canonical noreply identity", "133311911+46b-ETYKiAL@users.noreply.github.com"),
    # The bare forge address a web-UI commit carries. The identity half of the
    # audit already classifies this as non-PII drift; if the email half
    # disagreed, a file that merely DOCUMENTS it would be reported as a leak.
    ("forge web-ui noreply", "noreply@github.com"),
    # Documentation placeholders in test fixtures identify nobody.
    ("placeholder home, user", 'format_dropped_path("' + _HOME + 'user/file.txt")'),
    ("placeholder home, alice", "cwd=" + _HOME + "alice/proj"),
    ("placeholder home, op", 'insert(PaneId(0), "' + _HOME + 'op/work")'),
    # RFC 2606 / RFC 6761 reserved domains are documentation, not mailboxes.
    ("reserved domain, example.com", "maintainer@example.com"),
    ("reserved domain, .test", 'mailto_url("a@b.test", &title, &body)'),
    ("reserved domain, .example", "Maintainer: Corp <x@corp.example>"),
    # Ordinary English that merely shares a spelling with a suppressed token.
    ("word that shares a tooling name", "the claude model was used here"),
    ("word 'plan' without a number", "the plan is to ship; see plan B"),
    ("ordinary prose", "This terminal renders sixel images safely."),
    # `user@host` in a URL authority is not a mailbox. This shape is the whole
    # point of a URL-confinement test, so flagging it would discourage exactly
    # the security tests we want written.
    ("url userinfo in a confinement test", 'assert!(confined("https://api.github.com@evil.example.com/x").is_err());'),
    # ...and the OPAQUE URI form, which has no `//` at all. The exemption used
    # to require `://`, so every `mailto:` fixture was reported as a personal
    # mailbox. Rewriting such fixtures to a reserved domain only hides it until
    # the next `mailto:` fixture is written.
    # NOTE the concatenation is at RUNTIME (outside the quotes), so the sample
    # actually scanned is a COMPLETE `mailto:user@domain.tld`. Splitting inside
    # the literal would leave the domain TLD-less, the email pattern would not
    # match at all, and the case would pass for the wrong reason - a vacuous
    # test that says nothing about the exemption it claims to cover.
    # The domains are deliberately NON-reserved, so the ONLY thing that can
    # exempt them is the opaque-URI rule under test.
    ("mailto, opaque uri form", "mailto:a@b" + ".com"),
    ("mailto, inside a rust assert", 'is_clickable_url("mailto:who@gmail' + '.com")'),
    ("xmpp, opaque uri form", "xmpp:room@conference" + ".chat"),
    # A trailing file extension means a filename, not a domain.
    ("apple iconset member", 'cp "${D}/app-32.png" "${S}/icon_16x16@2x.png"'),
    # A single-character account name identifies nobody.
    ("single-letter account in a prompt fixture", r't.advance(b"line\r\nC:\Users\x>");'),
    # The placeholder allowlist must reach the MOUNTED patterns too. If it did
    # not, adding mount coverage would have turned every documentation example
    # written against a WSL path into a false positive.
    ("placeholder account under a wsl mount", "cd " + _MNT + "/Users/" + "user/proj"),
    ("placeholder account under a cygwin mount", _CYG + "/Users/" + "runner/work"),
    ("placeholder account under a wsl unc", _WSL_UNC + "/home/" + "alice"),
    # A mount path that is not a home path at all.
    ("mount path, not a home dir", "mount " + _MNT + "/ProgramData/cache"),
    ("mount path, data volume", "/mnt/data" + "/backups/2026"),
    # The relative-path false positives the `(?<![A-Za-z0-9])` lookbehind
    # exists to prevent. The mounted patterns are additive and must not have
    # reintroduced them.
    ("relative docs path containing 'home'", "see docs/home/index.md for setup"),
    ("relative path containing 'Users'", "crates/core/Users/mod.rs"),
    ("word 'home' in prose", "return to the home screen"),
    # Tokens that share digits with the account name but are not it. The
    # account probe carries a LEADING DOT; these do not, so registering it must
    # not have caught them.
    ("version-like token", "bumped to 0.46b in the changelog"),
    ("public handle, no leading dot", "https://github.com/46b-ETYKiAL/C0PL4ND"),
    ("public handle in prose", "46b-ETYKiAL maintains this repository"),
]


def test_catches_every_known_leak_class() -> None:
    for name, sample in MUST_CATCH:
        assert csa.scan_text(sample, "probe"), f"missed leak class: {name}"


def test_does_not_fire_on_legitimate_content() -> None:
    for name, sample in MUST_NOT_FIRE:
        assert not csa.scan_text(sample, "probe"), f"false positive on: {name}"


def test_third_party_attribution_is_exempt_from_the_email_rule_only() -> None:
    """Upstream authors' addresses in licence texts must be reproducible."""
    email = "Copyright (c) 2011, Someone (someone@upstream" + ".se)"
    assert csa.scan_text(email, "THIRD-PARTY-LICENSES.md")
    assert not csa.scan_text(email, "THIRD-PARTY-LICENSES.md", third_party=True)
    # ...but the exemption is email-only: a path still fires in the same file.
    assert csa.scan_text(_WIN + "a.dev_/x", "THIRD-PARTY-LICENSES.md", third_party=True)


def test_third_party_exemption_reaches_the_paths_this_repo_actually_ships() -> None:
    """The exemption is matched against a PATH, so the paths must be real.

    A font's `OFL.txt` and the installer's `License.rtf` legitimately carry
    upstream authors' addresses, and the WiX harvest transform reads them. If
    the pattern did not reach those exact paths the audit would be permanently
    red on files nobody is allowed to edit.
    """
    for rel in (
        "crates/app/assets/fonts/B612Mono/OFL.txt",
        "THIRD-PARTY-LICENSES.md",
        "packaging/windows/License.rtf",
        "LICENSE-MIT",
        "supply-chain/audits.toml",
    ):
        assert csa.THIRD_PARTY_ATTRIBUTION.search(rel), f"not exempt: {rel}"
    # ...and it must NOT swallow ordinary source files.
    for rel in ("crates/core/src/config.rs", "docs/KEYBINDINGS.md", "tests/x.py"):
        assert not csa.THIRD_PARTY_ATTRIBUTION.search(rel), f"wrongly exempt: {rel}"


def test_text_types_this_repo_tracks_are_actually_scanned() -> None:
    """A tracked text file whose extension is unlisted is published unscanned.

    That is the same failure class as a disarmed pattern: the audit reports a
    clean tree because it never looked. Every text extension this repository
    tracks must be enumerated in TEXT_EXT.
    """
    for ext in (".xsl", ".rtf", ".pub", ".1", ".wxs", ".plist", ".desktop"):
        assert ext in csa.TEXT_EXT, f"tracked text type is skipped by the audit: {ext}"


def test_audit_does_not_exempt_itself() -> None:
    """The audit is scanned like any other tracked file."""
    src = (_HERE / "content_safety_audit.py").read_text(encoding="utf-8")
    assert "SKIP_FILES" not in src, "self-exemption must not be reintroduced"
    assert "content_safety_audit.py" in {p.name for p in csa.tracked_files()}


def test_registration_helper_emits_a_digest_that_can_actually_fire() -> None:
    """`--hash` must print PROBE digests, not the digest of the raw token.

    `scan_text` only ever hashes the probe forms `token_probes` derives from a
    token. Printing `token_digest(raw)` therefore hands the operator an entry
    that can never match whenever the two differ - a trailing underscore is
    enough - and a DEAD entry looks exactly like a working one in the table.

    This drives the real `--hash` command line rather than the helper
    functions: the defect lives in the CLI branch, so a test that only
    exercises `token_probes` would stay green while `--hash` printed a digest
    that can never fire.
    """
    import re as _re
    import subprocess as _sp

    raw = "S4F3-Example_Token_"
    proc = _sp.run(
        [sys.executable, str(_HERE / "content_safety_audit.py"), "--hash", raw],
        capture_output=True, text=True, check=True,
    )
    printed = set(_re.findall(r'"([0-9a-f]{32})"', proc.stdout))
    assert printed, f"--hash printed no registrable digest: {proc.stdout!r}"

    # Every digest offered for registration must be one a real scan can
    # produce. `scan_text` is the only consumer, so this is the whole contract.
    reachable = {
        csa.token_digest(p)
        for m in csa._TOKEN_RE.finditer(f"see {raw} here")
        for p in csa.token_probes(m.group(0))
    }
    dead = printed - reachable
    assert not dead, f"--hash offered {len(dead)} digest(s) no scan can ever match"

    # ...and the fixture must actually distinguish the two forms, or the
    # assertion above would hold for a raw-digest implementation too.
    assert csa.token_digest(raw) not in reachable, (
        "fixture no longer distinguishes the raw digest from the probe digests"
    )


def test_this_suite_carries_no_literal_leak() -> None:
    """The corpus must be assembled, never inlined (see the module docstring)."""
    me = Path(__file__).resolve()
    assert not csa.scan_text(
        me.read_text(encoding="utf-8"), me.name
    ), "this suite leaks its own samples"


def _main() -> int:
    failures = 0
    for name, sample in MUST_CATCH:
        if not csa.scan_text(sample, "probe"):
            print(f"FAIL  expected a finding, got none: {name}")
            failures += 1
    for name, sample in MUST_NOT_FIRE:
        found = csa.scan_text(sample, "probe")
        if found:
            print(f"FAIL  unexpected finding for {name}: {found}")
            failures += 1
    for fn in (
        test_third_party_attribution_is_exempt_from_the_email_rule_only,
        test_third_party_exemption_reaches_the_paths_this_repo_actually_ships,
        test_text_types_this_repo_tracks_are_actually_scanned,
        test_audit_does_not_exempt_itself,
        test_registration_helper_emits_a_digest_that_can_actually_fire,
        test_this_suite_carries_no_literal_leak,
    ):
        try:
            fn()
        except AssertionError as e:
            print(f"FAIL  {fn.__name__}: {e}")
            failures += 1
    print(
        f"content-safety falsification: {len(MUST_CATCH)} catch-cases, "
        f"{len(MUST_NOT_FIRE)} clean-cases, {failures} failure(s)"
    )
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(_main())
