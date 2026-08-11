#!/usr/bin/env python3
"""Control for `falsify_release_gates.py`: prove the falsifier can see a regression.

`falsify_release_gates.py` reports that every release gate goes RED on bad input
and GREEN on good input. That report is only worth the paper it is printed on if
the falsifier would NOTICE when a gate stopped working. A harness that passes
whether or not the fix is present is indistinguishable from no harness, and is
the same class of defect -- "a gate that cannot fail" -- one level up.

So this file re-introduces each original defect, one at a time, into the real
working tree, and requires the falsifier to CATCH it:

    M1  release-ref-class.sh    SemVer test -> "contains a hyphen anywhere"
    M2  require-signing-key.sh  stable+unsigned stops being a hard failure
    M3  require-installer-signing.sh   the gate returns success unconditionally
    M4  require-macos-signing.sh       the gate returns success unconditionally
    M5  release.yml (checksums) shape assertion -> the original CR tautology
    M6  release.yml (payload)   font-licence count stops being asserted
    M7  release.yml (publish)   prerelease: -> contains(github.ref_name, '-')

Three disciplines make this a control rather than a ritual:

  * THE MUTANT MUST BE OBSERVED APPLIED. Every mutation asserts the file's
    bytes changed (sha256 before != after) and that they are restored exactly
    (sha256 after restore == before). A harness that silently fails to write its
    mutant reports a clean sweep of fake kills.

  * A KILL MUST BE SPECIFIC. Requiring only "the falsifier exited non-zero"
    would credit a kill to a mutant that merely broke YAML parsing, or to an
    unrelated pre-existing failure. Each mutant therefore names the exact check
    labels that must flip to FAIL, and the run is credited only if those precise
    labels are among the failures.

  * THE BASELINE MUST BE GREEN FIRST. A falsifier that is red for an unrelated
    reason would "kill" every mutant without detecting anything. The baseline
    run is asserted clean before any mutation is applied, and again at the end,
    so a mutant that failed to restore cannot be mistaken for a passing tree.

Files are read and written as BYTES. `Path.write_text` on Windows translates LF
to CRLF, which would rewrite every line of the file, defeat the byte-identity
assertion, and leave a mutated-looking tree behind after a restore.

Usage:
    python packaging/mutate_release_gates.py [--repo-root PATH] [-v]

Exit 0 = every mutant was applied, killed for the right reason, and reverted.
Exit 1 = a mutant SURVIVED (the falsifier cannot see that defect), or a mutant
         was never applied, or the tree was not restored.
"""

from __future__ import annotations

import argparse
import hashlib
import re
import subprocess
import sys
from pathlib import Path

FALSIFIER = "packaging/falsify_release_gates.py"
WORKFLOW = ".github/workflows/release.yml"

VERBOSE = False


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


class Mutant:
    """One re-introduced defect, plus the checks that must notice it."""

    def __init__(self, mid: str, path: str, old: str, new: str,
                 must_fail: list[str], rationale: str):
        self.mid = mid
        self.path = path
        self.old = old
        self.new = new
        self.must_fail = must_fail
        self.rationale = rationale


MUTANTS = [
    Mutant(
        "M1", "packaging/release-ref-class.sh",
        # The exact original defect: classify by "is there a hyphen", not SemVer.
        """if printf '%s' "${REF_NAME}" | grep -qE '^v?[0-9]+\\.[0-9]+\\.[0-9]+-[0-9A-Za-z.-]+(\\+[0-9A-Za-z.-]+)?$'; then""",
        """if printf '%s' "${REF_NAME}" | grep -qE -- '-'; then""",
        [
            "tag/v2026-08-10 -> stable",
            "tag/v1.0-final -> stable",
            "tag/v0.5-hotfix -> stable",
            "RED: stable tag v2026-08-10 with no key fails",
            "RED: stable v2026-08-10, no SIGNPATH_ORG_ID, no ack -> fails",
        ],
        "hyphen-anywhere hands every hyphenated stable tag the unsigned path",
    ),
    Mutant(
        "M2", "packaging/require-signing-key.sh",
        # Neuter the gate at the top: it can no longer reach its `exit 1`.
        "set -eu\n",
        "set -eu\nexit 0\n",
        [
            "RED: stable tag v0.4.26 with no key fails",
            "RED: stable tag v1.0.0 with no key fails",
        ],
        "an unsigned STABLE release stops being a hard failure",
    ),
    Mutant(
        "M3", "packaging/require-installer-signing.sh",
        "set -eu\n",
        "set -eu\nexit 0\n",
        [
            "RED: stable v0.4.26, no SIGNPATH_ORG_ID, no ack -> fails",
            "RED: ack value 'true' is NOT the sentinel -> still fails",
        ],
        "the permanently-absent SignPath credential stops blocking a stable tag",
    ),
    Mutant(
        "M4", "packaging/require-macos-signing.sh",
        "set -eu\n",
        "set -eu\nexit 0\n",
        [
            "RED: unsigned + gatekeeper-rejected on a stable tag -> fails, state reported",
            "RED: identity WITHOUT notarization on a stable tag -> fails",
            "RED: codesign not on PATH -> fails closed, never a silent skip",
        ],
        "an unnotarized .app stops blocking a stable tag",
    ),
    # The checksum shape check is mutated in three places rather than one,
    # because "the gate is gone" and "one clause of the gate is gone" are
    # different claims and only the narrow mutants can tell the falsifier's
    # clauses apart. A single coarse mutant that kills every check at once
    # would credit the falsifier with a precision it had not been shown to
    # have -- and, by removing the text the falsifier locates the loop by,
    # would in fact be killed for parse failure rather than for the defect.
    Mutant(
        "M5a", WORKFLOW,
        # Relax the line-shape regex to match anything.
        """grep -qE '^[0-9a-f]{64} [ *][^/\\\\]+$'""",
        """grep -qE '^.*$'""",
        [
            "RED: a path-prefixed filename field fails",
            "RED: a truncated digest fails",
            "RED: MALFORMED final line, no trailing newline -> FAILS",
        ],
        "the per-line shape regex stops constraining anything",
    ),
    Mutant(
        "M5b", WORKFLOW,
        # Drop the unterminated-final-line guard. This is the narrowest mutant
        # in the set: it must kill EXACTLY ONE check, the one that exists
        # because a whole-step test could not see this clause at all (`sort -u`
        # always terminates its output, so the step-level probe passes with or
        # without the guard -- which is why the falsifier extracts and runs the
        # loop directly).
        """while IFS= read -r line || [ -n "$line" ]; do""",
        """while IFS= read -r line; do""",
        [
            "RED: MALFORMED final line, no trailing newline -> FAILS",
        ],
        "a malformed LAST row stops being read (the clause no whole-step probe can see)",
    ),
    Mutant(
        "M5c", WORKFLOW,
        # Neuter the aggregate assertion while leaving the surrounding text --
        # which the falsifier uses to locate the loop -- intact.
        """[ "$bad" -eq 0 ] || { echo "::error::SHA256SUMS is malformed""",
        """[ 0 -eq 0 ] || { echo "::error::SHA256SUMS is malformed""",
        [
            "RED: a UTF-8 BOM survives",
            "RED: a path-prefixed filename field fails",
            "RED: a truncated digest fails",
            "RED: MALFORMED final line, no trailing newline -> FAILS",
        ],
        "the shape verdict is discarded, restoring a check no input can fail",
    ),
    # Both payload mutants deliberately leave `[ -d … ]`, `-gt 0 ]` and the
    # trailing `exit 1; }` in place. The falsifier locates this fragment by
    # that exact shape, so a mutant that edits it away is caught by the
    # "assertion is present in the shipped run: block" guard and the three
    # behavioural cases never execute -- proving only that the text changed,
    # never that the checks can see the behaviour change.
    Mutant(
        "M6a", WORKFLOW,
        # Sever the count from what it counts, leaving the assertion's text
        # intact. This is defect #5 exactly: `-gt 0` still written, nothing
        # actually constrained.
        """n=$(find payload/licenses/fonts -type f | wc -l)""",
        """n=1""",
        [
            "RED: directory present but EMPTY -> FAILS",
        ],
        "the licence count stops reflecting the payload (assertion present, inert)",
    ),
    Mutant(
        "M6b", WORKFLOW,
        # Downgrade the missing-directory diagnostic below the level an
        # operator is alerted on.
        """[ -d payload/licenses/fonts ] || { echo "::error::payload/licenses/fonts does not exist""",
        """[ -d payload/licenses/fonts ] || { echo "::notice::payload/licenses/fonts does not exist""",
        [
            "RED: directory missing entirely -> FAILS",
        ],
        "a wholly missing licence directory degrades to an unalerted notice",
    ),
    Mutant(
        "M7", WORKFLOW,
        "prerelease: ${{ steps.refclass.outputs.prerelease }}",
        "prerelease: ${{ contains(github.ref_name, '-') }}",
        [
            "the publish step's prerelease: reads the shared classifier",
            "no live `contains(github.ref_name",
        ],
        "the publish step re-grows a second, disagreeing prerelease predicate",
    ),
]


def run_falsifier(root: Path) -> tuple[int, str]:
    """Run the falsifier, capturing its exit status DIRECTLY (never via a pipe).

    A pipeline reports only its last stage's status, so an interpreter error
    ("no such command") would arrive as success.
    """
    proc = subprocess.run(
        [sys.executable, FALSIFIER],
        cwd=str(root), capture_output=True, text=True, timeout=900,
    )
    return proc.returncode, (proc.stdout or "") + (proc.stderr or "")


def failed_labels(output: str) -> list[str]:
    return [m.group(1).strip() for m in re.finditer(r"^  FAIL  (.+)$", output, re.M)]


def main() -> int:
    global VERBOSE
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo-root", default=".")
    ap.add_argument("-v", "--verbose", action="store_true")
    ap.add_argument("--check-anchors", action="store_true",
                    help="verify every mutant's anchor resolves exactly once, "
                         "then exit without mutating or running anything")
    args = ap.parse_args()
    VERBOSE = args.verbose
    root = Path(args.repo_root).resolve()

    if args.check_anchors:
        bad = 0
        for mut in MUTANTS:
            text = (root / mut.path).read_bytes().decode("utf-8")
            n = text.count(mut.old)
            if n != 1:
                bad += 1
            print(f"{'OK ' if n == 1 else 'BAD'}  {mut.mid:5s} anchor x{n}  {mut.path}")
        print(f"\nanchors resolving non-uniquely: {bad}")
        return 1 if bad else 0

    print(f"mutate-release-gates: repo-root={root}")
    print(f"mutate-release-gates: python={sys.executable}\n")

    # --- baseline -----------------------------------------------------------
    # A falsifier that is already red would "kill" every mutant without ever
    # detecting one. Establish that it is green on the unmutated tree first.
    print("[baseline] the falsifier must be GREEN on the unmutated tree")
    rc, out = run_falsifier(root)
    if rc != 0:
        print("  FAIL  baseline falsifier is RED — mutation results would be meaningless")
        print("        " + "\n        ".join(failed_labels(out)[:20]))
        return 1
    print("  PASS  baseline falsifier is green\n")

    survivors: list[str] = []
    misapplied: list[str] = []

    for mut in MUTANTS:
        target = root / mut.path
        original = target.read_bytes()
        before = sha(original)

        text = original.decode("utf-8")
        occurrences = text.count(mut.old)
        print(f"[{mut.mid}] {mut.path}: {mut.rationale}")

        if occurrences != 1:
            # Not a kill and not a survival — the mutant never described the
            # tree. Reporting either verdict would be a fabrication.
            print(f"  FAIL  anchor matched {occurrences} times, expected exactly 1 "
                  f"— mutant NOT APPLIED, verdict unknown")
            misapplied.append(mut.mid)
            continue

        mutated = text.replace(mut.old, mut.new).encode("utf-8")
        # Bytes, not write_text: a text-mode write turns every LF into CRLF on
        # Windows and would leave the tree rewritten after "restore".
        target.write_bytes(mutated)
        after = sha(target.read_bytes())

        try:
            if after == before:
                print("  FAIL  file bytes UNCHANGED after writing the mutant "
                      "— any 'kill' below would be fake")
                misapplied.append(mut.mid)
                continue

            rc, out = run_falsifier(root)
            fails = failed_labels(out)

            if rc == 0:
                print(f"  FAIL  MUTANT SURVIVED — the falsifier stayed green with "
                      f"the defect re-introduced")
                survivors.append(f"{mut.mid} ({mut.rationale})")
                continue

            missing = [want for want in mut.must_fail
                       if not any(want in got for got in fails)]
            if missing:
                # The falsifier went red, but not for the reason claimed. A kill
                # credited to the wrong check is how a harness comes to look
                # sharper than it is.
                print(f"  FAIL  killed for the WRONG REASON — expected these checks "
                      f"to flip, and they did not:")
                for want in missing:
                    print(f"          - {want}")
                print(f"        (observed {len(fails)} failure(s): "
                      f"{'; '.join(fails[:6])})")
                survivors.append(f"{mut.mid} (wrong-reason kill)")
                continue

            print(f"  PASS  KILLED — {len(fails)} check(s) flipped, including all "
                  f"{len(mut.must_fail)} required")
            if VERBOSE:
                for f in fails:
                    print(f"          FAIL: {f}")
        finally:
            target.write_bytes(original)
            restored = sha(target.read_bytes())
            if restored != before:
                print(f"  FAIL  {mut.path} NOT RESTORED "
                      f"({before[:12]} -> {restored[:12]})")
                misapplied.append(f"{mut.mid}/restore")

    # --- post-check ---------------------------------------------------------
    # Prove the tree really is back to the state the baseline passed on, so a
    # botched restore cannot masquerade as a clean run.
    print("\n[post] the falsifier must be GREEN again after every restore")
    rc, out = run_falsifier(root)
    if rc != 0:
        print("  FAIL  tree not restored — falsifier is RED after the mutation run")
        print("        " + "\n        ".join(failed_labels(out)[:20]))
        return 1
    print("  PASS  tree restored; falsifier green\n")

    print("=" * 70)
    if survivors or misapplied:
        for s in survivors:
            print(f"SURVIVED: {s}")
        for m in misapplied:
            print(f"NOT APPLIED: {m}")
        print(f"FAIL: {len(survivors)} surviving mutant(s), "
              f"{len(misapplied)} not applied")
        return 1
    print(f"PASS: {len(MUTANTS)}/{len(MUTANTS)} mutants applied, killed for the "
          f"stated reason, and reverted")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
