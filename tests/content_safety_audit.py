#!/usr/bin/env python3
"""Public-repo content-safety audit.

Fails (exit 1) if any **git-tracked** file, or any commit in the repository's
history, carries content that must not appear in a public repository: an
absolute developer path, an internal-tooling reference, a personal email
address, an internal work-tracking token, or a secret-shaped string.

Three design decisions are load-bearing - read them before editing:

1.  **Tracked files only.** Enumeration is `git ls-files`, not a filesystem
    walk. A walk reports violations inside local, gitignored scratch
    directories that git will never publish, and a gate that is red for things
    which cannot leak trains people to ignore it. It is also far faster.

2.  **This file is scanned like every other file.** There is no self-exemption.
    The internal tokens the audit suppresses are therefore stored as salted
    SHA-256 digests and never as plaintext, so the guard cannot leak the very
    names it exists to suppress. Structural patterns (path shapes, key shapes,
    email shapes) describe generic forms, reveal nothing, and stay readable.

3.  **Git metadata is public.** Commit author/committer identities and commit
    messages are exactly as visible as the tree, so both are audited. The
    identity half has its own dedicated, range-scoped guard in
    `tests/author_identity_guard.py`, which is what a pre-push hook and CI run
    on the commits actually being published.

Register a new suppressed token with:
    python tests/content_safety_audit.py --hash '<token>'
"""

from __future__ import annotations

import argparse
import hashlib
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

_SALT = b"content-safety-audit/v2"


def token_digest(token: str) -> str:
    """Salted digest used to recognise a suppressed token without storing it."""
    return hashlib.sha256(_SALT + token.strip().lower().encode("utf-8")).hexdigest()[:32]


# digest -> violation label. The label must describe the CLASS, never restate
# the token, or the plaintext would be back in this file.
INTERNAL_TOKEN_DIGESTS: dict[str, str] = {
    "c8ae9fddee570432385c33928b50b265": "internal tooling directory reference",
    "faf63005bf32b8fc0b4466622ba0612c": "internal tooling directory reference",
    "7ccc78c856566b5b74057ab772c8f4ed": "internal tooling directory reference",
    "466fc9500022d781f5cc2775d5a54ef8": "internal tooling directory reference",
    "e0cf04edcbf5e4ab5c2fab05b82f2725": "internal monorepo identifier",
    "0a9795434b627753e2f6987dad607f2f": "internal monorepo identifier",
    "33558db327f794545cdcc796bd457ed8": "internal monorepo identifier",
    "dbb10d47bc0663fbc78909b1561ea73f": "internal work-tracking token",
    # The workstation account name, as a BARE token. A home path containing it
    # is already caught structurally; this covers the form that carries no path
    # around it (a config value, a log line, a serialised profile field), which
    # no path pattern can see.
    "9cb7059967aebce6b943a6910bc5139a": "operating-system account name",
}

# ---------------------------------------------------------------------------
# Structural patterns - generic shapes, safe to keep readable
# ---------------------------------------------------------------------------

# Home-directory user components that are recognised documentation
# placeholders. These appear in unit-test fixtures and manual examples and
# identify nobody. This is an allowlist of GENERIC names only: any other user
# component - a real account name or a service account - is still a violation.
PLACEHOLDER_HOME_USERS = {
    "user", "users", "username", "youruser", "your-user", "me", "you",
    "alice", "bob", "carol", "dave", "eve", "op", "u", "example",
    "runner", "someone", "somebody", "test", "testuser",
}

# Addresses that are intentionally published: forge-generated noreply
# identities, and the RFC 2606 / RFC 6761 reserved documentation domains.
# Everything else is treated as a real mailbox, i.e. as PII.
ALLOWED_EMAIL_RE = re.compile(
    r"^(?:[^@\s]+@users\.noreply\.github\.com"
    # The bare forge address a web-UI commit carries. `audit_identities`
    # already classifies this as non-PII drift rather than a leak; omitting it
    # here made the two halves of this file disagree, so a file that merely
    # DOCUMENTS the forge's own noreply address was reported as carrying a
    # personal mailbox.
    r"|noreply@github\.com"
    r"|[^@\s]+@(?:[A-Za-z0-9.\-]+\.)?(?:example|test|invalid|localhost)"
    r"|[^@\s]+@example\.(?:com|org|net))$",
    re.IGNORECASE,
)
EMAIL_RE = re.compile(r"\b[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.([A-Za-z]{2,})\b")

# `user@host` inside a URI is not a mailbox. This shape is load-bearing in
# URL-confinement security tests (`https://trusted@evil/`), which are exactly
# the tests we must not discourage people from writing.
#
# Two alternatives, because a URI's authority is not the only place `user@host`
# legitimately appears:
#
#   1. `scheme://` - the hierarchical form, where `user@` is authority userinfo.
#   2. `scheme:` with NO slashes - the opaque form. `mailto:` is the one that
#      matters here and it is precisely the one the `://` requirement missed, so
#      every `mailto:user@host` fixture was reported as a personal email
#      address. Rewriting such fixtures to @example.com only moves the trap for
#      the next person to write one.
#
# The opaque form is an explicit scheme allowlist, not a generic
# `scheme:`-with-optional-slashes pattern. A generic form would exempt any
# `Word:name@real.example` prose - and quietly turn a real leak into a pass,
# which is a far worse failure than the false positive being fixed.
_URL_USERINFO_RE = re.compile(
    r"(?:[A-Za-z][A-Za-z0-9+.\-]*://|(?:mailto|xmpp|sip|sips|im|tel):)\S*$"
)

# A trailing label that is a file extension is a filename, not a domain -
# e.g. an Apple iconset member such as `icon_16x16@2x.png`.
NON_DOMAIN_TLDS = {
    "png", "jpg", "jpeg", "gif", "svg", "webp", "ico", "icns", "bmp",
    "md", "txt", "rs", "py", "sh", "ps1", "js", "ts", "json", "toml",
    "yml", "yaml", "lock", "html", "css", "exe", "dll", "zip", "gz",
    "log", "csv", "pdf", "xml", "wav", "mp3", "mp4", "ttf", "otf",
}

# Files whose whole purpose is to reproduce THIRD-PARTY authorship: upstream
# licence texts and generated supply-chain attestations. Upstream authors'
# contact addresses are part of the attribution we are legally obliged to
# reproduce verbatim; they are not our PII and cannot be edited out. These
# paths are exempt from the email rule ONLY - every other rule still applies.
THIRD_PARTY_ATTRIBUTION = re.compile(
    r"(?:^|/)(?:THIRD-PARTY-LICENSES\.md|LICENSE[^/]*|COPYING[^/]*|OFL\.txt"
    r"|NOTICE[^/]*)$|^supply-chain/|/vendor/",
    re.IGNORECASE,
)

# Home-path labels whose capture group 1 is the user component, so the
# placeholder allowlist can be applied to them.
_HOME_LABELS = {
    "absolute Windows user path",
    "absolute macOS home path",
    "absolute Linux home path",
    "absolute Windows user path (mounted)",
    "absolute Linux home path (mounted)",
}

# A home path reached through a cross-OS mount. These need their own patterns
# rather than a relaxation of the ones above, and the reason is worth stating
# because it looks like duplication:
#
#   The plain `/home/` and `/Users/` patterns carry a `(?<![A-Za-z0-9])`
#   lookbehind so that an ordinary relative path (`docs/home/index.md`,
#   `crates/x/Users/...`) does not fire. Every mount prefix ends in an
#   alphanumeric -- `/mnt/c`, `/cygdrive/c`, `\\wsl$\Ubuntu` -- so that
#   lookbehind silently made a REAL home path under a mount invisible:
#   `/mnt/c/Users/<account>` identifies its owner exactly as well as
#   `C:\Users\<account>` does, and was matched by nothing.
#
#   Relaxing the lookbehind would have reintroduced the relative-path false
#   positives it exists to prevent. Enumerating the mount prefixes instead
#   leaves the existing patterns untouched and adds no new FP surface: each
#   alternative below is anchored to a specific, unambiguous mount syntax.
_MOUNT_PREFIX = (
    r"(?:/mnt/[A-Za-z]\b"                                   # WSL:      /mnt/c
    r"|/cygdrive/[A-Za-z]\b"                                # Cygwin:   /cygdrive/c
    r"|[\\/]{2,4}wsl(?:\$|\.localhost)[\\/]{1,4}[A-Za-z0-9._\-]+"  # \\wsl$\Distro
    r")"
)

STRUCTURAL_PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    # Windows user profile. BOTH separator conventions: a forward-slash form is
    # exactly as identifying as a backslash one, and is the form that appears in
    # Rust string literals, YAML, and shell snippets.
    # Capture ONLY the account segment, so the placeholder allowlist applies
    # here exactly as it does to the POSIX home patterns.
    # `{1,4}` on the separators, not `{1,2}`: a path written into a source
    # literal is escaped once (`C:\\Users\\x` in Rust/JSON) and a path written
    # into a literal that is itself embedded in another literal is escaped
    # twice (`C:\\\\Users\\\\x`). The doubly-escaped form is the one that
    # appears in generated fixtures and config blobs, and `{1,2}` could not
    # see it at all.
    ("absolute Windows user path", re.compile(r"[A-Za-z]:[\\/]{1,4}Users[\\/]{1,4}([^\s\"'`<>|,)\]\\/]+)")),
    ("absolute macOS home path", re.compile(r"(?<![A-Za-z0-9])/Users/([A-Za-z0-9._\-]+)")),
    # The same two shapes reached through a cross-OS mount (see _MOUNT_PREFIX).
    (
        "absolute Windows user path (mounted)",
        re.compile(_MOUNT_PREFIX + r"[\\/]{1,4}Users[\\/]{1,4}([^\s\"'`<>|,)\]\\/]+)", re.IGNORECASE),
    ),
    (
        "absolute Linux home path (mounted)",
        re.compile(_MOUNT_PREFIX + r"[\\/]{1,4}home[\\/]{1,4}([A-Za-z0-9._\-]+)", re.IGNORECASE),
    ),
    # No trailing slash required: a home path that ends at the account name
    # identifies that account just as well as one with a trailing slash does.
    ("absolute Linux home path", re.compile(r"(?<![A-Za-z0-9])/home/([A-Za-z0-9._\-]+)")),
    ("embedded private key", re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
    ("AWS access key id", re.compile(r"\bAKIA[0-9A-Z]{16}\b")),
    ("GitHub token", re.compile(r"\bgh[pousr]_[A-Za-z0-9]{36,}\b")),
    ("Slack token", re.compile(r"\bxox[abprs]-[A-Za-z0-9\-]{10,}\b")),
    (
        "generic secret assignment",
        re.compile(
            r"(?i)\b(?:secret|api[_-]?key|access[_-]?token|password|passwd)\b\s*[=:]\s*"
            r"['\"][A-Za-z0-9/+_\-]{20,}['\"]"
        ),
    ),
]

# ---------------------------------------------------------------------------
# Tokenising, for the hashed internal tokens
# ---------------------------------------------------------------------------

_TOKEN_RE = re.compile(r"\.?[A-Za-z0-9][A-Za-z0-9_.\-]*")
_MAX_SEGMENTS = 8


def token_probes(raw: str) -> set[str]:
    """Normalised probe forms for one lexical token.

    A leak rarely appears as a bare token: it is embedded in a longer path
    segment or identifier (``<owner>.<group>_<repo>-<suffix>``). The token is
    split on ``. _ -`` and every contiguous run of segments is emitted, so an
    embedded internal name is still recognised. A LEADING dot is kept attached
    to the first segment, which is what distinguishes a hidden tooling
    directory from an ordinary word that merely shares its spelling.
    """
    tok = raw.strip().lower().strip("._-")
    if not tok:
        return set()
    leading_dot = raw.strip().lower().startswith(".")

    segs = [s for s in re.split(r"[._\-]+", tok) if s]
    if not segs or len(segs) > _MAX_SEGMENTS:
        return {tok} if len(segs) <= _MAX_SEGMENTS else set()
    if leading_dot:
        segs[0] = "." + segs[0]

    probes: set[str] = set()
    for i in range(len(segs)):
        for j in range(i + 1, len(segs) + 1):
            run = segs[i:j]
            probes.add("-".join(run))
            # Digit-masked variant, so a numbered work item can be suppressed
            # by its shape rather than by its (common-word) stem alone.
            masked = ["#" if s.isdigit() else s for s in run]
            if masked != run:
                probes.add("-".join(masked))
    return probes


def scan_text(text: str, origin: str, *, third_party: bool = False) -> list[str]:
    """Every violation in ``text``, each labelled with ``origin``."""
    out: list[str] = []

    def lineno(pos: int) -> int:
        return text.count("\n", 0, pos) + 1

    for label, pat in STRUCTURAL_PATTERNS:
        for m in pat.finditer(text):
            if label in _HOME_LABELS:
                who = (m.group(1) or "").strip("<>'\"`").lower()
                # A single-character account name identifies nobody.
                if len(who) <= 1 or who in PLACEHOLDER_HOME_USERS:
                    continue
            out.append(f"{origin}:{lineno(m.start())}: {label}")

    if not third_party:
        for m in EMAIL_RE.finditer(text):
            if ALLOWED_EMAIL_RE.match(m.group(0)):
                continue
            if m.group(1).lower() in NON_DOMAIN_TLDS:
                continue  # a filename, not a mailbox
            line_start = text.rfind("\n", 0, m.start()) + 1
            if _URL_USERINFO_RE.search(text[line_start : m.start()]):
                continue  # URL authority userinfo, not a mailbox
            out.append(f"{origin}:{lineno(m.start())}: personal email address")

    for m in _TOKEN_RE.finditer(text):
        for probe in token_probes(m.group(0)):
            label = INTERNAL_TOKEN_DIGESTS.get(token_digest(probe))
            if label:
                out.append(f"{origin}:{lineno(m.start())}: {label}")
                break
    return out


# ---------------------------------------------------------------------------
# Enumeration
# ---------------------------------------------------------------------------

TEXT_EXT = {
    ".rs", ".toml", ".md", ".yml", ".yaml", ".json", ".txt", ".svg", ".sh",
    ".ps1", ".bat", ".cmd", ".lua", ".wgsl", ".cfg", ".conf", ".ini", ".lock",
    ".py", ".rb", ".rhai", ".wxs", ".xsl", ".desktop", ".plist", ".xml", ".html",
    ".css", ".js", ".ts", ".tsx", ".jsx", ".nsi", ".spec", ".service", ".env",
    ".gitignore", ".gitattributes", ".editorconfig", ".sql", ".proto", ".rc",
    ".c", ".h", ".cpp", ".m", ".mm", ".java", ".kt", ".swift", ".go",
    # Text formats this repository actually tracks and which were previously
    # skipped, so their content was published unscanned:
    #   .xsl  - the WiX license-harvest transform under packaging/windows/
    #   .rtf  - the installer licence page
    #   .pub  - the minisign public verification key
    #   .1    - the roff man page
    ".rtf", ".pub", ".1",
}

# Bulk word lists: still scanned for identity/path leaks, but the secret-shape
# heuristics are noise there, so only lines that could carry one are kept.
# This repository ships no such list; the mechanism is kept so adding one does
# not require rediscovering why it is needed.
DICT_SUFFIXES: tuple[str, ...] = ()
MAX_BYTES = 4 * 1024 * 1024

# The identity every commit SHOULD carry.
CANONICAL_IDENTITY = "133311911+46b-etykial@users.noreply.github.com"

# Identities that are not the canonical one but still expose no mailbox: forge
# noreply forms and bot accounts. These are reported as drift, never as PII.
NON_PII_IDENTITY_RE = re.compile(
    r"^(?:[^@\s]+@users\.noreply\.github\.com|noreply@github\.com)$", re.IGNORECASE
)


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=ROOT, capture_output=True, text=True,
        encoding="utf-8", errors="replace", check=True,
    ).stdout


def tracked_files() -> list[Path]:
    out: list[Path] = []
    for rel in git("ls-files", "-z").split("\0"):
        if not rel:
            continue
        p = ROOT / rel
        if not p.is_file():
            continue
        # An extensionless tracked file (LICENSE, Makefile, a hook script) is
        # text and is scanned; a known-binary extension is skipped.
        if p.suffix and p.suffix.lower() not in TEXT_EXT:
            continue
        try:
            if p.stat().st_size > MAX_BYTES:
                continue
        except OSError:
            continue
        out.append(p)
    return out


def audit_identities() -> tuple[list[str], list[str]]:
    """Audit every commit's author/committer identity.

    Returns ``(violations, drift_notes)``. A real mailbox in commit metadata is
    a violation - commit metadata is exactly as public as the tree, and it
    cannot be redacted without rewriting history. A non-canonical *noreply*
    identity exposes no mailbox, so it is reported as drift, not as PII.
    """
    try:
        raw = git("log", "--all", "--format=%H%x1f%ae%x1f%ce")
    except subprocess.CalledProcessError:
        return (["git-history: cannot read commit history (audit cannot pass)"], [])

    pii: dict[str, tuple[str, int]] = {}
    drift: dict[str, int] = {}
    for line in raw.splitlines():
        parts = line.split("\x1f")
        if len(parts) != 3:
            continue
        sha, ae, ce = parts
        for addr in {ae.lower(), ce.lower()}:
            if addr == CANONICAL_IDENTITY:
                continue
            if NON_PII_IDENTITY_RE.match(addr):
                drift[addr] = drift.get(addr, 0) + 1
                continue
            first, n = pii.get(addr, (sha, 0))
            pii[addr] = (first, n + 1)

    violations = [
        f"git-history: personal mailbox in commit identity - {n} commit(s), "
        f"first {first[:12]} (requires history rewrite)"
        for _addr, (first, n) in sorted(pii.items(), key=lambda kv: -kv[1][1])
    ]
    notes = [
        f"git-history: non-canonical (but non-PII) identity on {n} commit(s)"
        for _addr, n in sorted(drift.items(), key=lambda kv: -kv[1])
    ]
    return violations, notes


def audit_commit_messages(limit: int) -> list[str]:
    try:
        raw = git("log", "--all", f"-n{limit}", "--format=%H%x1f%B%x1e")
    except subprocess.CalledProcessError:
        return ["git-history: cannot read commit messages (audit cannot pass)"]
    out: list[str] = []
    for rec in raw.split("\x1e"):
        rec = rec.strip()
        if not rec or "\x1f" not in rec:
            continue
        sha, body = rec.split("\x1f", 1)
        for v in scan_text(body, f"commit {sha[:12]}"):
            out.append(re.sub(r"^(commit [0-9a-f]+):\d+:", r"\1:", v))
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description="Public-repo content-safety audit.")
    ap.add_argument("--hash", metavar="TOKEN", help="print a token's digest and exit")
    ap.add_argument(
        "--history",
        action="store_true",
        help=(
            "also audit commit identities and messages. Off by default because "
            "history findings can only be cleared by rewriting history, which is "
            "an owner decision - not something an ordinary PR can fix."
        ),
    )
    ap.add_argument("--history-limit", type=int, default=2000)
    args = ap.parse_args()

    if args.hash:
        # Print the digest of every PROBE form, not of the raw token.
        #
        # `scan_text` never hashes a token as typed: it hashes the normalised
        # probe forms `token_probes` derives from it (case-folded, stripped of
        # surrounding `._-`, split into contiguous segment runs). Registering
        # `token_digest(raw)` therefore produces an entry that can never match
        # whenever the raw form differs from its probe form -- a trailing
        # underscore or a mixed-case spelling is enough. That entry is not a
        # loose suppression; it is a DEAD one, and it looks identical in the
        # table to a working entry.
        #
        # Emitting the probe digests makes the registration path honest: copy
        # the line for the form you actually mean to suppress.
        probes = sorted(token_probes(args.hash))
        if not probes:
            print("no probe form derives from that token; nothing to register")
            return 1
        for p in probes:
            print(f'    "{token_digest(p)}": "<label>",  # probe form: {p}')
        return 0

    violations: list[str] = []
    files = tracked_files()
    for path in files:
        rel = path.relative_to(ROOT).as_posix()
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError as e:
            violations.append(f"{rel}:0: unreadable tracked file ({e})")
            continue
        if DICT_SUFFIXES and rel.endswith(DICT_SUFFIXES):
            text = "\n".join(ln for ln in text.splitlines() if "/" in ln or "@" in ln)
        violations.extend(
            scan_text(text, rel, third_party=bool(THIRD_PARTY_ATTRIBUTION.search(rel)))
        )

    notes: list[str] = []
    if args.history:
        ident_v, ident_n = audit_identities()
        violations.extend(ident_v)
        notes.extend(ident_n)
        violations.extend(audit_commit_messages(args.history_limit))

    suffix = f" + git history (last {args.history_limit} commits)" if args.history else ""
    print(f"content-safety: scanned {len(files)} tracked file(s){suffix}")
    for n in dict.fromkeys(notes):
        print(f"  note: {n}")

    if violations:
        seen: set[str] = set()
        uniq = [v for v in violations if not (v in seen or seen.add(v))]
        print(f"\nFAIL - {len(uniq)} content-safety violation(s):\n")
        for v in uniq:
            print(f"  {v}")
        print("\nThis content is NOT safe for a public repository.")
        return 1
    print("PASS - no internal references, identity leaks, or secrets found.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
