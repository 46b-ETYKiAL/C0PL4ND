#!/usr/bin/env python3
"""Prove the release workflow's signing/licence/checksum gates can FAIL.

Every gate this file covers was, at some point, a gate that could not fail:

  * The minisign step classified a prerelease as "the tag name contains a hyphen
    anywhere", so `v2026-08-10` / `v1.0-final` / `v0.5-hotfix` skipped signing
    AND the whole Tier-0 self-verify block and published green.
  * Both SignPath steps are gated on `vars.SIGNPATH_ORG_ID != ''`, which is
    unset, so the Windows installer has never been signed and nothing said so.
  * The macOS job has no codesign/notarytool/stapler/spctl at all.
  * `grep -q $'\\r' release/SHA256SUMS` ran on the file `tr -d '\\r'` had just
    produced -- no input could make it fire.
  * The installer payload's font-licence count was `echo`ed and never asserted,
    while the sibling MSI job asserted `-gt 0` on the identical tree.

A fix for that class of defect is worthless unless the fixed gate is itself
observed failing. So this harness does two things, and NEITHER of them trusts a
copy of the logic:

  1. Executes `packaging/*.sh` gate scripts directly, across a matrix of refs and
     credential states, asserting exit status AND the annotation level.
  2. Parses `.github/workflows/release.yml` with a real YAML parser, EXTRACTS the
     `run:` block of the named steps, and executes THAT TEXT against fixtures --
     so the inline shell in the workflow is falsified as-shipped, not as-quoted.
     It also asserts each gate is actually WIRED (invoked at a path that
     resolves, before the `exit 0` that would skip it).

Usage:
    python packaging/falsify_release_gates.py [--repo-root PATH] [-v]

Exit 0 = every gate was shown red on bad input and green on good input.
Exit 1 = a gate could not be made to fail, or failed when it should not have.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

try:
    import yaml
except ImportError:  # pragma: no cover - dependency is declared in the workflow
    print("falsify-release-gates: FATAL: PyYAML is required (pip install pyyaml)",
          file=sys.stderr)
    raise SystemExit(1)

RELEASE_WORKFLOW = ".github/workflows/release.yml"

FAILURES: list[str] = []
CHECKS = 0
VERBOSE = False


def log(msg: str) -> None:
    if VERBOSE:
        print(f"    {msg}")


def check(ok: bool, label: str, detail: str = "") -> None:
    global CHECKS
    CHECKS += 1
    if ok:
        print(f"  PASS  {label}")
    else:
        print(f"  FAIL  {label}")
        if detail:
            print("        " + detail.replace("\n", "\n        "))
        FAILURES.append(label)


def posix_shell() -> str:
    """Locate a POSIX shell, or refuse to run.

    Skipping because no shell resolved would turn this harness into exactly the
    thing it exists to prevent: a gate that reports success without running.
    """
    for cand in ("bash", "sh", "/usr/bin/bash", "/bin/bash", "/usr/bin/sh"):
        exe = shutil.which(cand) or (cand if Path(cand).exists() else None)
        if exe:
            try:
                r = subprocess.run([exe, "-c", "exit 0"], capture_output=True, timeout=30)
                if r.returncode == 0:
                    return exe
            except (OSError, subprocess.SubprocessError):
                continue
    raise SystemExit(
        "falsify-release-gates: FATAL: no POSIX shell found. The release gates "
        "cannot be executed, so they are UNVERIFIED on this host. Install a "
        "shell rather than skipping -- an unfalsifiable gate is the defect this "
        "harness exists to prevent."
    )


SHELL = ""


def run_sh(script: str, *, args: list[str] | None = None, env: dict[str, str] | None = None,
           cwd: str | None = None, as_file: bool = False) -> tuple[int, str]:
    """Run `script` (a path when as_file, else inline text) and return (rc, log)."""
    full_env = {k: v for k, v in os.environ.items()
                if k not in ("GITHUB_REF_TYPE", "GITHUB_REF_NAME", "SIGNPATH_ORG_ID",
                             "UNSIGNED_WINDOWS_INSTALLER_ACK", "UNSIGNED_MACOS_APP_ACK",
                             "MINISIGN_SECRET_KEY", "GITHUB_OUTPUT")}
    full_env.update(env or {})
    cmd = [SHELL, script, *(args or [])] if as_file else [SHELL, "-c", script, "sh", *(args or [])]
    r = subprocess.run(cmd, capture_output=True, text=True, env=full_env, cwd=cwd, timeout=300)
    return r.returncode, (r.stdout or "") + (r.stderr or "")


# ---------------------------------------------------------------------------
# Workflow introspection
# ---------------------------------------------------------------------------

def load_workflow(root: Path) -> dict:
    with (root / RELEASE_WORKFLOW).open(encoding="utf-8") as fh:
        return yaml.safe_load(fh)


def steps_of(wf: dict, job: str) -> list[dict]:
    return wf.get("jobs", {}).get(job, {}).get("steps", []) or []


def find_step(wf: dict, job: str, name_fragment: str) -> dict | None:
    for st in steps_of(wf, job):
        if name_fragment.lower() in str(st.get("name", "")).lower():
            return st
    return None


# ---------------------------------------------------------------------------
# 1. release-ref-class.sh -- the shared SemVer rule
# ---------------------------------------------------------------------------

def falsify_ref_classifier(root: Path) -> None:
    print("\n[1] release-ref-class.sh -- the SemVer rule (single home)")
    script = str(root / "packaging" / "release-ref-class.sh")

    # (ref_type, ref_name, expected class, why)
    cases = [
        # THE DEFECT. Each of these contains a hyphen, so the old
        # `case … in *-*)` / `contains(ref_name,'-')` rule called it a
        # prerelease -- and each is in fact a stable tag.
        ("tag", "v2026-08-10", "stable", "date tag, not a version triple at all"),
        ("tag", "v1.0-final", "stable", "no PATCH field, so not SemVer"),
        ("tag", "v0.5-hotfix", "stable", "no PATCH field, so not SemVer"),
        ("tag", "v1.2-beta", "stable", "no PATCH field"),
        ("tag", "v2026-01-01-nightly", "stable", "date tag with a suffix"),
        # Ordinary stable tags -- unchanged behaviour.
        ("tag", "v0.4.26", "stable", "ordinary stable tag"),
        ("tag", "v1.0.0", "stable", "ordinary stable tag"),
        ("tag", "0.4.26", "stable", "stable without the v prefix"),
        # Real SemVer prereleases -- MUST still be tolerated. A gate that
        # failed everything would be "shown red" while bricking every rc.
        ("tag", "v0.4.26-rc.1", "prerelease", "well-formed SemVer prerelease"),
        ("tag", "v0.4.26-pre", "prerelease", "well-formed SemVer prerelease"),
        ("tag", "v0.5.0-hotfix", "prerelease",
         "EXPLICITLY correct: 0.5.0-hotfix is a well-formed SemVer prerelease"),
        ("tag", "v1.0.0-alpha.1", "prerelease", "well-formed"),
        ("tag", "v1.0.0-rc.1+build.5", "prerelease", "prerelease + build metadata"),
        ("tag", "1.0.0-rc.1", "prerelease", "no v prefix"),
        # Non-tag refs -- deliberately unchanged.
        ("branch", "master", "non-tag", "workflow_dispatch on a branch"),
        ("", "", "non-tag", "no ref at all"),
    ]
    for ref_type, ref_name, expected, why in cases:
        rc, out = run_sh(script, as_file=True,
                         env={"GITHUB_REF_TYPE": ref_type, "GITHUB_REF_NAME": ref_name})
        got = out.strip()
        check(rc == 0 and got == expected,
              f"{ref_type or '(none)'}/{ref_name or '(none)'} -> {expected}  ({why})",
              f"rc={rc} got={got!r}")


# ---------------------------------------------------------------------------
# 2. require-signing-key.sh -- minisign, both directions
# ---------------------------------------------------------------------------

def falsify_signing_key_gate(root: Path) -> None:
    print("\n[2] require-signing-key.sh -- unsigned STABLE tag must FAIL")
    script = str(root / "packaging" / "require-signing-key.sh")

    must_fail = ["v0.4.26", "v1.0.0", "v2026-08-10", "v1.0-final", "v0.5-hotfix"]
    for tag in must_fail:
        rc, out = run_sh(script, as_file=True,
                         env={"GITHUB_REF_TYPE": "tag", "GITHUB_REF_NAME": tag})
        check(rc == 1 and "::error::" in out,
              f"RED: stable tag {tag} with no key fails (rc=1 + ::error::)",
              f"rc={rc}\n{out}")

    must_pass = [("tag", "v0.4.26-rc.1"), ("tag", "v0.4.26-pre"),
                 ("tag", "v0.5.0-hotfix"), ("branch", "master"), ("", "")]
    for ref_type, ref_name in must_pass:
        rc, out = run_sh(script, as_file=True,
                         env={"GITHUB_REF_TYPE": ref_type, "GITHUB_REF_NAME": ref_name})
        check(rc == 0 and "::warning::" in out,
              f"GREEN: {ref_type or '(none)'}/{ref_name or '(none)'} tolerated, but WARNED",
              f"rc={rc}\n{out}")


# ---------------------------------------------------------------------------
# 3. require-installer-signing.sh -- Windows, both directions
# ---------------------------------------------------------------------------

def falsify_installer_gate(root: Path) -> None:
    print("\n[3] require-installer-signing.sh -- unsigned installer on a STABLE tag must FAIL")
    script = str(root / "packaging" / "require-installer-signing.sh")
    ACK = "i-accept-unsigned-smartscreen-warnings"

    # RED -- the live default. This is the state the repository is in TODAY:
    # `gh variable list` is empty, so SIGNPATH_ORG_ID and the ack are both unset.
    for tag in ["v0.4.26", "v2026-08-10", "v1.0-final"]:
        rc, out = run_sh(script, as_file=True,
                         env={"GITHUB_REF_TYPE": "tag", "GITHUB_REF_NAME": tag})
        check(rc == 1 and "::error::" in out,
              f"RED: stable {tag}, no SIGNPATH_ORG_ID, no ack -> fails",
              f"rc={rc}\n{out}")

    # GREEN -- credential provisioned.
    rc, out = run_sh(script, as_file=True,
                     env={"GITHUB_REF_TYPE": "tag", "GITHUB_REF_NAME": "v0.4.26",
                          "SIGNPATH_ORG_ID": "org-1234"})
    check(rc == 0, "GREEN: stable tag WITH SIGNPATH_ORG_ID -> passes", f"rc={rc}\n{out}")

    # GREEN -- explicit owner acknowledgement, still warned every run.
    rc, out = run_sh(script, as_file=True,
                     env={"GITHUB_REF_TYPE": "tag", "GITHUB_REF_NAME": "v0.4.26",
                          "UNSIGNED_WINDOWS_INSTALLER_ACK": ACK})
    check(rc == 0 and "::warning::" in out,
          "GREEN: stable tag with recorded ack -> passes, but still ::warning::",
          f"rc={rc}\n{out}")

    # RED -- a WRONG ack value must not open the gate. An ack that matched
    # loosely would be a bypass, not a decision.
    for wrong in ["true", "1", "yes", "i-accept", ACK.upper()]:
        rc, out = run_sh(script, as_file=True,
                         env={"GITHUB_REF_TYPE": "tag", "GITHUB_REF_NAME": "v0.4.26",
                              "UNSIGNED_WINDOWS_INSTALLER_ACK": wrong})
        check(rc == 1 and "::error::" in out,
              f"RED: ack value {wrong!r} is NOT the sentinel -> still fails",
              f"rc={rc}\n{out}")

    # GREEN -- prerelease / non-tag tolerated, but announced.
    for ref_type, ref_name in [("tag", "v0.4.26-rc.1"), ("branch", "master")]:
        rc, out = run_sh(script, as_file=True,
                         env={"GITHUB_REF_TYPE": ref_type, "GITHUB_REF_NAME": ref_name})
        check(rc == 0 and "::notice::" in out,
              f"GREEN: {ref_type}/{ref_name} tolerated, but announced",
              f"rc={rc}\n{out}")


# ---------------------------------------------------------------------------
# 4. require-macos-signing.sh -- probes codesign/spctl; stub them to replay states
# ---------------------------------------------------------------------------

CODESIGN_STUB = {
    "unsigned": "printf '%s\\n' \"{app}: code object is not signed at all\" >&2; exit 1",
    "adhoc": ("printf '%s\\n' 'Executable=/x/C0PL4ND.app/Contents/MacOS/c0pl4nd' >&2; "
              "printf '%s\\n' 'Signature=adhoc' >&2; exit 0"),
    "identity": ("printf '%s\\n' 'Authority=Developer ID Application: Itasha Corp (AB12CD34EF)' >&2; "
                 "printf '%s\\n' 'TeamIdentifier=AB12CD34EF' >&2; exit 0"),
    "garbage": "printf '%s\\n' 'something unrecognisable' >&2; exit 0",
}
SPCTL_STUB = {
    "rejected": "printf '%s\\n' 'x: rejected' >&2; exit 3",
    "accepted": "printf '%s\\n' 'x: accepted\\nsource=Developer ID' >&2; exit 0",
    "notarized": "printf '%s\\n' 'x: accepted\\nsource=Notarized Developer ID' >&2; exit 0",
}


def _macos_env(root: Path, tmp: Path, codesign: str, spctl: str | None) -> dict[str, str]:
    """Build a PATH whose codesign/spctl are stubs replaying one signing state."""
    binz = tmp / "stubbin"
    binz.mkdir(exist_ok=True)
    (binz / "codesign").write_text("#!/bin/sh\n" + CODESIGN_STUB[codesign] + "\n",
                                   encoding="utf-8", newline="\n")
    os.chmod(binz / "codesign", 0o755)
    if spctl is not None:
        (binz / "spctl").write_text("#!/bin/sh\n" + SPCTL_STUB[spctl] + "\n",
                                    encoding="utf-8", newline="\n")
        os.chmod(binz / "spctl", 0o755)
    # Keep the shell's own dir on PATH so grep/printf resolve.
    keep = os.pathsep.join(p for p in os.environ.get("PATH", "").split(os.pathsep)
                           if p and "stubbin" not in p)
    return {"PATH": str(binz).replace("\\", "/") + os.pathsep + keep}


def falsify_macos_gate(root: Path) -> None:
    print("\n[4] require-macos-signing.sh -- unnotarized .app on a STABLE tag must FAIL")
    script = str(root / "packaging" / "require-macos-signing.sh")
    ACK = "i-accept-gatekeeper-quarantine"

    with tempfile.TemporaryDirectory() as td:
        tmp = Path(td)
        app = tmp / "C0PL4ND.app"
        app.mkdir()
        appstr = str(app).replace("\\", "/")

        def go(codesign, spctl, ref_type="tag", ref_name="v0.4.26", ack=None, bundle=appstr):
            env = _macos_env(root, tmp, codesign, spctl)
            env |= {"GITHUB_REF_TYPE": ref_type, "GITHUB_REF_NAME": ref_name}
            if ack:
                env["UNSIGNED_MACOS_APP_ACK"] = ack
            return run_sh(script, as_file=True, args=[bundle], env=env)

        # RED -- the live default: unsigned/adhoc, Gatekeeper rejects, stable tag.
        for cs in ("unsigned", "adhoc"):
            rc, out = go(cs, "rejected")
            check(rc == 1 and "::error::" in out and f"codesign={cs}" in out,
                  f"RED: {cs} + gatekeeper-rejected on a stable tag -> fails, state reported",
                  f"rc={rc}\n{out}")

        # RED -- signed with an identity but NOT notarized. Deliberately NOT
        # ack-eligible: Gatekeeper still refuses it, so it buys nothing.
        rc, out = go("identity", "accepted")
        check(rc == 1 and "::error::" in out and "not notarized" in out.lower(),
              "RED: identity WITHOUT notarization on a stable tag -> fails",
              f"rc={rc}\n{out}")
        rc, out = go("identity", "accepted", ack=ACK)
        check(rc == 1, "RED: the ack does NOT excuse signed-but-unnotarized",
              f"rc={rc}\n{out}")

        # RED -- unclassifiable codesign output fails closed rather than guessing.
        rc, out = go("garbage", "rejected")
        check(rc == 1 and "::error::" in out,
              "RED: unrecognised codesign output -> fails closed, never 'probably fine'",
              f"rc={rc}\n{out}")

        # RED -- a missing bundle fails closed.
        rc, out = go("unsigned", "rejected", bundle=str(tmp / "nope.app").replace("\\", "/"))
        check(rc == 1 and "::error::" in out,
              "RED: missing .app bundle -> fails closed",
              f"rc={rc}\n{out}")

        # RED -- codesign absent entirely: a gate that cannot run is not a gate.
        binz = tmp / "emptybin"
        binz.mkdir(exist_ok=True)
        shellpath = str(Path(SHELL).parent).replace("\\", "/")
        rc, out = run_sh(script, as_file=True, args=[appstr],
                         env={"PATH": str(binz).replace("\\", "/") + os.pathsep + shellpath,
                              "GITHUB_REF_TYPE": "tag", "GITHUB_REF_NAME": "v0.4.26"})
        check(rc == 1 and "::error::" in out,
              "RED: codesign not on PATH -> fails closed, never a silent skip",
              f"rc={rc}\n{out}")

        # GREEN -- properly notarized.
        rc, out = go("identity", "notarized")
        check(rc == 0 and "::error::" not in out,
              "GREEN: signed + notarized on a stable tag -> passes clean",
              f"rc={rc}\n{out}")

        # GREEN -- explicit owner acknowledgement, still warned every run.
        rc, out = go("unsigned", "rejected", ack=ACK)
        check(rc == 0 and "::warning::" in out,
              "GREEN: stable tag with recorded ack -> passes, but still ::warning::",
              f"rc={rc}\n{out}")
        rc, out = go("unsigned", "rejected", ack="yes")
        check(rc == 1, "RED: a wrong ack value is not the sentinel -> still fails",
              f"rc={rc}\n{out}")

        # GREEN -- prerelease tolerated, but announced.
        rc, out = go("unsigned", "rejected", ref_name="v0.4.26-rc.1")
        check(rc == 0 and "::notice::" in out,
              "GREEN: unsigned prerelease tolerated, but announced",
              f"rc={rc}\n{out}")


# ---------------------------------------------------------------------------
# 5. The SHA256SUMS aggregation step -- extracted from release.yml and executed
# ---------------------------------------------------------------------------

GOOD_SHA = "a" * 64


def falsify_checksum_step(root: Path, wf: dict) -> None:
    print("\n[5] 'Flatten artifacts + aggregate checksums' -- extracted run: block, executed")
    step = find_step(wf, "release", "aggregate checksums")
    if step is None or "run" not in step:
        check(False, "the aggregate-checksums step exists and has a run: block")
        return
    body = step["run"]

    # The old post-strip CR assertion was a tautology: prove the replacement is
    # not. `tr -d '\r'` removes CRs but NOT a UTF-8 BOM, so the BOM case is the
    # one that shows the new shape check can actually fire.
    def stage(sidecars: dict[str, bytes]) -> str:
        td = tempfile.mkdtemp()
        art = Path(td) / "artifacts"
        art.mkdir()
        for name, data in sidecars.items():
            (art / name).write_bytes(data)
        return td

    def run_step(sidecars: dict[str, bytes]) -> tuple[int, str]:
        cwd = stage(sidecars)
        # The step's `find artifacts … -exec cp` needs real artifact files too,
        # otherwise nothing but the sidecars is staged -- which is fine here:
        # the sidecars ARE what this step aggregates.
        return run_sh(body, cwd=cwd)

    ok = {"a.zip.sha256": f"{GOOD_SHA}  a.zip\n".encode(),
          "b.tar.gz.sha256": f"{GOOD_SHA} *b.tar.gz\n".encode()}

    rc, out = run_step(ok)
    check(rc == 0, "GREEN: well-formed LF sidecars aggregate cleanly", f"rc={rc}\n{out}")

    crlf = {"a.zip.sha256": f"{GOOD_SHA}  a.zip\r\n".encode()}
    rc, out = run_step(crlf)
    check(rc == 0 and "::notice::" in out and "CRLF" in out,
          "GREEN+REPORTED: CRLF in a SOURCE sidecar is normalised and ANNOUNCED "
          "(the old check asserted this AFTER the strip -- a tautology)",
          f"rc={rc}\n{out}")

    # THE FALSIFICATION the tautology could never do: a byte the strip does not
    # remove must now fail the step.
    bom = {"a.zip.sha256": b"\xef\xbb\xbf" + f"{GOOD_SHA}  a.zip\n".encode()}
    rc, out = run_step(bom)
    check(rc == 1 and "::error::" in out,
          "RED: a UTF-8 BOM survives `tr -d '\\r'` and now FAILS the step "
          "(no input could ever make the old CR assertion fire)",
          f"rc={rc}\n{out}")

    pathy = {"a.zip.sha256": f"{GOOD_SHA}  release/a.zip\n".encode()}
    rc, out = run_step(pathy)
    check(rc == 1 and "::error::" in out,
          "RED: a path-prefixed filename field fails (breaks exact-name lookup)",
          f"rc={rc}\n{out}")

    trunc = {"a.zip.sha256": b"deadbeef  a.zip\n"}
    rc, out = run_step(trunc)
    check(rc == 1 and "::error::" in out,
          "RED: a truncated digest fails", f"rc={rc}\n{out}")

    # The LAST row must be checked too. This CANNOT be exercised through the
    # whole step -- `sort -u` always terminates its output, so SHA256SUMS always
    # ends in a newline no matter what the sidecars look like. Feeding the step
    # an unterminated sidecar therefore passes whether or not the guard is
    # present: a test that would report success for the wrong reason.
    # (Measured: reverting `|| [ -n "$line" ]` left that whole-step form GREEN.)
    #
    # So falsify the LOOP ITSELF, extracted from the shipped run: block and run
    # against an unterminated file. Now removing the guard turns this red.
    m = re.search(r"^\s*bad=0$.*?refusing to publish it as the release's only "
                  r"integrity record\"; exit 1; \}\s*$", body, re.S | re.M)
    if not m:
        check(False, "the SHA256SUMS validation loop is extractable from the run: block")
    else:
        frag = "set -euo pipefail\n" + m.group(0)
        for label, data, want in [
            ("well-formed final line, no trailing newline",
             f"{GOOD_SHA}  a.zip".encode(), 0),
            ("MALFORMED final line, no trailing newline",
             b"deadbeef  a.zip", 1),
        ]:
            td = tempfile.mkdtemp()
            rel = Path(td) / "release"
            rel.mkdir()
            (rel / "SHA256SUMS").write_bytes(data)
            rc, out = run_sh(frag, cwd=td)
            check(rc == want and (want == 0 or "::error::" in out),
                  f"{'GREEN' if want == 0 else 'RED'}: {label} -> "
                  f"{'passes' if want == 0 else 'FAILS'} (the final row IS read)",
                  f"rc={rc}\n{out}")

    rc, out = run_step({})
    check(rc == 1 and "::error::" in out,
          "RED: no sidecars at all still fails (pre-existing guard, unchanged)",
          f"rc={rc}\n{out}")


# ---------------------------------------------------------------------------
# 6. The installer payload licence assert -- extracted and executed
# ---------------------------------------------------------------------------

def falsify_payload_licence_assert(root: Path, wf: dict) -> None:
    print("\n[6] 'Assemble payload' -- font-licence count is ASSERTED, not printed")
    step = find_step(wf, "windows-installer", "Assemble payload")
    if step is None or "run" not in step:
        check(False, "the assemble-payload step exists and has a run: block")
        return
    body = step["run"]

    # Execute only the licence-count tail: the rest of the step needs a real
    # Windows binary zip and the collector. Extract it by its own marker rather
    # than re-typing it, so what runs here is the shipped text.
    # Anchor on the DIRECTORY guard, not on `n=$(find …)`. Starting at the
    # count would silently drop the `[ -d … ]` line that precedes it, and the
    # fragment would then reproduce the very bug that guard exists to prevent
    # (find's own error, no ::error::) -- a harness testing a different program
    # than the one that ships.
    m = re.search(r"^\s*\[ -d payload/licenses/fonts \].*?-gt 0 \].*?exit 1; \}\s*$",
                  body, re.S | re.M)
    if not m:
        check(False, "the licence-count assertion is present in the shipped run: block",
              "expected `[ -d payload/licenses/fonts ]` then "
              "`n=$(find …)` then `[ \"$n\" -gt 0 ]`")
        return
    frag = "set -euo pipefail\n" + m.group(0)

    for label, files, want_rc in [
        ("two licence files staged", ["OFL.txt", "LICENSE.txt"], 0),
        ("directory present but EMPTY", [], 1),
        ("directory missing entirely", None, 1),
    ]:
        td = tempfile.mkdtemp()
        if files is not None:
            d = Path(td) / "payload" / "licenses" / "fonts"
            d.mkdir(parents=True)
            for f in files:
                (d / f).write_text("x", encoding="utf-8")
        rc, out = run_sh(frag, cwd=td)
        check(rc == want_rc and (want_rc == 0 or "::error::" in out),
              f"{'GREEN' if want_rc == 0 else 'RED'}: {label} -> "
              f"{'passes' if want_rc == 0 else 'FAILS'}",
              f"rc={rc}\n{out}")

    # Symmetry with the sibling MSI job, which already asserted -gt 0.
    msi = find_step(wf, "windows-msi", "payload")
    msi_asserts = bool(msi and re.search(r'\[\s*"\$n"\s*-gt\s*0\s*\]', msi.get("run", "")))
    check(msi_asserts, "the sibling windows-msi job still asserts -gt 0 (symmetry held)")


# ---------------------------------------------------------------------------
# 7. Wiring -- a gate nothing calls is not a gate
# ---------------------------------------------------------------------------

def falsify_wiring(root: Path, wf: dict) -> None:
    print("\n[7] wiring -- each gate is invoked at a path that RESOLVES")

    # (job, step fragment, script path as written in the YAML, path on disk)
    wanted = [
        ("release", "Sign release artifacts",
         "src/packaging/require-signing-key.sh", "packaging/require-signing-key.sh"),
        ("release", "Classify the tag",
         "src/packaging/release-ref-class.sh", "packaging/release-ref-class.sh"),
        ("windows-installer", "Assert the installer signing state",
         "app/packaging/require-installer-signing.sh", "packaging/require-installer-signing.sh"),
        ("macos-installer", "Assert the macOS signing state",
         "app/packaging/require-macos-signing.sh", "packaging/require-macos-signing.sh"),
    ]
    for job, frag, invoked, ondisk in wanted:
        step = find_step(wf, job, frag)
        check(step is not None, f"{job}: step '{frag}' exists")
        if step is None:
            continue
        body = str(step.get("run", ""))
        check(invoked in body, f"{job}/'{frag}' invokes {invoked}", body[:400])
        # The path half is not pedantry. Each of these jobs checks out into a
        # SUBDIR and sets no working-directory, so a bare `packaging/…` would
        # miss; `sh` on a missing file exits 127 under -e, which would hard-fail
        # the prerelease path these gates are meant to wave through.
        check((root / ondisk).is_file(), f"{ondisk} exists on disk")
        prefix = invoked.split("/", 1)[0]
        co = next((s for s in steps_of(wf, job)
                   if "checkout" in str(s.get("uses", "")).lower()
                   and str((s.get("with") or {}).get("path", "")) == prefix), None)
        check(co is not None,
              f"{job}: a checkout step maps '{prefix}/' to the repo root "
              f"(so {invoked} resolves at runtime)")

    # The minisign guard must run BEFORE the exit 0 that skips signing +
    # self-verify -- calling it after would be decorative.
    sign = find_step(wf, "release", "Sign release artifacts")
    if sign:
        body = str(sign.get("run", ""))
        i_guard = body.find("require-signing-key.sh")
        i_exit = body.find("exit 0", i_guard if i_guard >= 0 else 0)
        check(i_guard >= 0 and i_exit > i_guard,
              "the minisign guard runs BEFORE the exit 0 that skips signing + self-verify")

    # The Windows gate must NOT be `if:`-gated on SIGNPATH_ORG_ID: a gate that
    # disappears under the exact condition it detects is the original bug.
    wgate = find_step(wf, "windows-installer", "Assert the installer signing state")
    if wgate:
        check("SIGNPATH_ORG_ID" not in str(wgate.get("if", "")),
              "the Windows signing gate is NOT if:-gated on SIGNPATH_ORG_ID "
              "(it must run precisely when the credential is absent)")

    # Every gate script must be committed with LF. `require-installer-signing.sh`
    # runs on windows-latest via `shell: bash`; a CRLF checkout there appends \r
    # to the last token of every line, so the ack-sentinel comparison can never
    # match and the acknowledged path silently reverts to a hard failure. This
    # reads the INDEX blob, not the working copy: with core.autocrlf=true the
    # working copy is legitimately CRLF on Windows while the committed bytes are
    # LF, and it is the committed bytes a runner checks out.
    for rel in ("packaging/release-ref-class.sh", "packaging/require-signing-key.sh",
                "packaging/require-installer-signing.sh", "packaging/require-macos-signing.sh"):
        try:
            blob = subprocess.run(["git", "cat-file", "blob", f":{rel}"],
                                  capture_output=True, cwd=root, timeout=60).stdout
            check(blob.count(b"\r\n") == 0 and len(blob) > 0,
                  f"{rel} is committed with LF line endings",
                  f"{blob.count(b'\r\n')} CRLF in the index blob")
        except (OSError, subprocess.SubprocessError) as exc:
            check(False, f"{rel} line-ending check could run", str(exc))

    # The publish step must no longer use the hyphen-anywhere rule.
    pub = find_step(wf, "release", "Create GitHub Release")
    if pub:
        pre = str((pub.get("with") or {}).get("prerelease", ""))
        check("contains(" not in pre and "refclass" in pre,
              "the publish step's prerelease: reads the shared classifier, "
              "not contains(github.ref_name, '-')", f"got: {pre!r}")

    # The old hyphen-anywhere rule must be gone as LIVE CODE. A raw-text scan
    # cannot be used here: the replacement's own comments quote the defect they
    # replaced ("this WAS `case … in *-*)`"), and a scan that cannot tell live
    # code from a comment describing history would force those comments to be
    # deleted to stay green -- destroying the explanation to satisfy the check.
    # So scan the PARSED workflow, with shell comment lines stripped from every
    # run: body.
    def live_shell(body: str) -> str:
        return "\n".join(ln for ln in body.splitlines()
                         if not ln.lstrip().startswith("#"))

    def live_exprs(w: dict) -> list[str]:
        out: list[str] = []
        for job in (w.get("jobs") or {}).values():
            out.append(str(job.get("if", "")))
            for st in (job.get("steps") or []):
                out.append(str(st.get("if", "")))
                for v in (st.get("with") or {}).values():
                    out.append(str(v))
                for v in (st.get("env") or {}).values():
                    out.append(str(v))
        return out

    check(not any("contains(github.ref_name" in e for e in live_exprs(wf)),
          "no live `contains(github.ref_name, …)` expression remains in release.yml")

    offenders = [f"{jn}/{st.get('name')}"
                 for jn, job in (wf.get("jobs") or {}).items()
                 for st in (job.get("steps") or [])
                 if "in *-*)" in live_shell(str(st.get("run", "")))]
    check(not offenders,
          "no live `case … in *-*)` prerelease classification remains in release.yml",
          f"offending steps: {offenders}")

    # Guard the guard: the comment-stripping above must not be able to hide a
    # REAL occurrence. Prove it still fires on live code.
    check("in *-*)" in live_shell('case "$x" in *-*)\n  echo hi\nesac'),
          "the comment-stripper still detects the pattern in LIVE shell")
    check("in *-*)" not in live_shell('# this WAS `case "$x" in *-*)`'),
          "the comment-stripper ignores the pattern inside a comment")


def main() -> int:
    global VERBOSE, SHELL
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo-root", default=None)
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()
    VERBOSE = args.verbose

    root = Path(args.repo_root).resolve() if args.repo_root else Path(__file__).resolve().parents[1]
    SHELL = posix_shell()
    print(f"falsify-release-gates: repo-root={root}")
    print(f"falsify-release-gates: shell={SHELL}")

    wf = load_workflow(root)

    falsify_ref_classifier(root)
    falsify_signing_key_gate(root)
    falsify_installer_gate(root)
    falsify_macos_gate(root)
    falsify_checksum_step(root, wf)
    falsify_payload_licence_assert(root, wf)
    falsify_wiring(root, wf)

    print(f"\n{'=' * 70}")
    if FAILURES:
        print(f"FAIL: {len(FAILURES)}/{CHECKS} checks failed")
        for f in FAILURES:
            print(f"  - {f}")
        return 1
    print(f"PASS: {CHECKS}/{CHECKS} checks — every gate was shown RED on bad "
          f"input and GREEN on good input")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
