#!/usr/bin/env python3
"""Fail if a release artifact could ever ship without its license texts.

C0PL4ND `include_bytes!`-embeds ~22 third-party typefaces directly into the
shipped executable. Twenty-one are SIL Open Font License 1.1; Syncopate is
Apache-2.0. OFL-1.1 section 2 requires the license text to accompany EVERY copy
of the Font Software, including when it is bundled inside a larger program.

Before this gate existed, no release artifact carried a single font license:
the packaging steps copied `LICENSE-MIT`/`LICENSE-APACHE`/`README.md` on a
best-effort `2>/dev/null || true` and nothing else. That is a license breach on
distribution, and nothing in CI would ever have noticed it.

This checker asserts over the packaging MANIFEST (workflow + build scripts) and
the repo tree -- it does NOT build artifacts, so it is fast and runnable on any
platform. Four independent checks:

  1. Every bundled font directory carries a license file.
  2. `THIRD-PARTY-LICENSES.md` documents every bundled font.
  3. Every artifact-producing step in `.github/workflows/release.yml` invokes
     the license collector.
  4. Every local packaging build script invokes the license collector.

Check 3 is the one that stops silent regression: if someone rewrites a
packaging step and drops the collector call, this fails.

Usage:
    python packaging/verify_license_delivery.py [--repo-root PATH]

Exit 0 = every artifact path delivers licenses. Exit 1 = a breach is possible.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

try:
    import yaml
except ImportError:  # pragma: no cover - dependency is declared in the workflow
    print("verify-license-delivery: FATAL: PyYAML is required "
          "(pip install pyyaml)", file=sys.stderr)
    raise SystemExit(1)

# --- repository-specific configuration -------------------------------------
# C0PL4ND's fonts live under the app crate, NOT the repo-root `assets/` dir.
# That distinction is exactly why the Windows-installer payload step, which
# copies repo-root `assets/`, historically shipped zero font licenses.
FONT_ROOT = "crates/app/assets/fonts"
COLLECTOR = "packaging/collect-licenses.sh"
RELEASE_WORKFLOW = ".github/workflows/release.yml"
THIRD_PARTY_DOC = "THIRD-PARTY-LICENSES.md"

FONT_SUFFIXES = (".ttf", ".otf", ".ttc", ".woff2")
LICENSE_NAMES = ("OFL.txt", "LICENSE.txt", "LICENSE", "UFL.txt")

# Local packaging scripts that assemble a distributable tree.
BUILD_SCRIPTS = (
    "packaging/linux/build-deb.sh",
    "packaging/linux/build-appimage.sh",
    "packaging/macos/build-dmg.sh",
)

# A `run:` block containing any of these is producing a distributable artifact
# and therefore MUST also stage the license texts.
ARTIFACT_MARKERS = (
    "tar -czf",
    "Compress-Archive",
    "dpkg-deb --build",
    "appimagetool",
    "hdiutil create",
    "build_native_installer",
)

# NOTE: there is deliberately NO step-name exemption list here.
#
# An earlier draft skipped steps whose name matched sign/attest/checksum/... on
# the theory that they only re-package already-licensed inputs. In practice no
# such step contains an artifact marker at all, so the list bought nothing --
# and it silently swallowed SCR1B3's PRIMARY tarball step, named
# "Package + checksum", because the word "checksum" appears in its name. The
# single most important artifact was therefore exempt from the gate while the
# gate reported OK. Matching on a step's NAME to decide whether it produces an
# artifact is the bug; matching on what the step RUNS is the fix.


class Failure(Exception):
    """A license-delivery breach."""


def font_dirs(root: Path) -> list[Path]:
    """Directories under FONT_ROOT that actually ship a font file."""
    base = root / FONT_ROOT
    if not base.is_dir():
        raise Failure(f"font root not found: {FONT_ROOT}")
    found = []
    for d in sorted(p for p in base.iterdir() if p.is_dir()):
        if any(f.suffix.lower() in FONT_SUFFIXES for f in d.iterdir() if f.is_file()):
            found.append(d)
    if not found:
        raise Failure(f"no bundled fonts found under {FONT_ROOT}")
    return found


def check_font_licenses(root: Path, dirs: list[Path]) -> list[str]:
    """1. Every bundled font directory carries a license file."""
    bad = [d.name for d in dirs
           if not any((d / n).is_file() for n in LICENSE_NAMES)]
    if bad:
        raise Failure(
            "bundled font(s) with NO license text: " + ", ".join(sorted(bad))
            + "\n  OFL-1.1 s2 requires the license to accompany every copy."
        )
    return [d.name for d in dirs]


def check_doc_coverage(root: Path, names: list[str]) -> None:
    """2. THIRD-PARTY-LICENSES.md documents every bundled font."""
    doc_path = root / THIRD_PARTY_DOC
    if not doc_path.is_file():
        raise Failure(f"missing {THIRD_PARTY_DOC}")
    doc = doc_path.read_text(encoding="utf-8", errors="replace")
    # The document links each font by its on-disk directory path.
    missing = [n for n in names if f"{FONT_ROOT}/{n}/" not in doc]
    if missing:
        raise Failure(
            f"font(s) bundled but not documented in {THIRD_PARTY_DOC}: "
            + ", ".join(sorted(missing))
        )


def _run_blocks(workflow: dict) -> list[tuple[str, str, str]]:
    """Yield (job_id, step_name, run_script) for every step with a `run:`."""
    out = []
    for job_id, job in (workflow.get("jobs") or {}).items():
        for step in (job or {}).get("steps") or []:
            if not isinstance(step, dict):
                continue
            run = step.get("run")
            if isinstance(run, str):
                out.append((job_id, str(step.get("name", "<unnamed>")), run))
    return out


def check_workflow(root: Path) -> list[str]:
    """3. Every artifact-producing workflow STEP is covered by the collector.

    A packaging step is covered when EITHER:

      a) the step itself invokes the collector, OR
      b) an EARLIER step in the same job invokes the collector AND that earlier
         step is not itself an artifact-producing step -- i.e. it is a payload
         assembly step staging the tree the later step packages.

    Rule (b) exists because `windows-installer` legitimately splits "Assemble
    payload" (stages licenses) from "Build native installer" (packages it).

    The non-artifact restriction in (b) is load-bearing. Scoping this check to
    the whole JOB instead lets a cut slip through undetected: the `build` job
    contains two independent artifact-producing steps (the Unix tar.gz and the
    Windows zip, selected by the OS matrix), so with job scope, deleting the
    collector from one still "passes" on the strength of the other. That is a
    subset-scoped invariant -- green while one of the two shipped artifacts is
    in breach. Per-step is the granularity that actually matches the artifacts.
    """
    wf_path = root / RELEASE_WORKFLOW
    if not wf_path.is_file():
        raise Failure(f"missing {RELEASE_WORKFLOW}")
    workflow = yaml.safe_load(wf_path.read_text(encoding="utf-8"))

    collector_name = Path(COLLECTOR).name

    # Group run-blocks by job, preserving step order.
    jobs: dict[str, list[tuple[str, str]]] = {}
    for job_id, name, run in _run_blocks(workflow):
        jobs.setdefault(job_id, []).append((name, run))

    def is_producing(name: str, run: str) -> bool:
        """A step produces an artifact iff it RUNS an archiver/packager.

        Keyed on the script body only -- never on the step's name (see the note
        beside ARTIFACT_MARKERS).
        """
        return any(m in run for m in ARTIFACT_MARKERS)

    offenders, covered = [], []
    total_producing = 0
    for job_id, steps in jobs.items():
        for i, (name, run) in enumerate(steps):
            if not is_producing(name, run):
                continue
            total_producing += 1

            # A single step can build MORE THAN ONE artifact -- SCR1B3's
            # "Build .deb + AppImage" step builds two. Requiring merely "a
            # collector call appears somewhere in this step" is a subset-scoped
            # invariant: deleting the .deb's collector call still passes on the
            # strength of the AppImage's. So require one staging call per
            # distinct artifact type the step produces.
            need = sum(1 for m in ARTIFACT_MARKERS if m in run)
            have = run.count(collector_name)

            if have >= need:
                covered.append(f"{job_id} / {name} (stages inline x{have})")
                continue

            # (b) an earlier, non-packaging step staged the tree it packages.
            stager = next(
                (n for n, r in steps[:i]
                 if collector_name in r and not is_producing(n, r)),
                None,
            )
            if stager and have + 1 >= need:
                covered.append(f"{job_id} / {name} (staged by '{stager}')")
            else:
                offenders.append(
                    f"{job_id} / {name} "
                    f"[builds {need} artifact type(s), stages {have}]"
                )

    if total_producing == 0:
        raise Failure(
            "no artifact-producing steps detected in "
            f"{RELEASE_WORKFLOW} -- the marker list is stale, so this gate "
            "would silently pass. Refusing to report success."
        )
    if offenders:
        raise Failure(
            "artifact-producing step(s) that never stage license texts:\n"
            + "\n".join(f"    - {o}" for o in sorted(offenders))
            + f"\n  Each must invoke {COLLECTOR} (inline, or in an earlier"
            "\n  payload-assembly step of the same job)."
        )
    return sorted(covered)


def check_build_scripts(root: Path) -> list[str]:
    """4. Every local packaging build script invokes the collector."""
    collector_name = Path(COLLECTOR).name
    offenders, covered = [], []
    for rel in BUILD_SCRIPTS:
        p = root / rel
        if not p.is_file():
            offenders.append(f"{rel} (missing)")
            continue
        if collector_name in p.read_text(encoding="utf-8", errors="replace"):
            covered.append(rel)
        else:
            offenders.append(rel)
    if offenders:
        raise Failure(
            "packaging script(s) that do NOT stage license texts:\n"
            + "\n".join(f"    - {o}" for o in offenders)
            + f"\n  Each must invoke {COLLECTOR}."
        )
    return covered


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo-root", default=None,
                    help="repository root (default: parent of packaging/)")
    args = ap.parse_args()

    root = (Path(args.repo_root).resolve() if args.repo_root
            else Path(__file__).resolve().parent.parent)

    if not (root / COLLECTOR).is_file():
        print(f"verify-license-delivery: FAIL: missing collector {COLLECTOR}",
              file=sys.stderr)
        return 1

    try:
        dirs = font_dirs(root)
        names = check_font_licenses(root, dirs)
        check_doc_coverage(root, names)
        wf = check_workflow(root)
        scripts = check_build_scripts(root)
    except Failure as exc:
        print(f"verify-license-delivery: FAIL: {exc}", file=sys.stderr)
        return 1

    print("verify-license-delivery: OK")
    print(f"  {len(names)} bundled font(s), all licensed and documented")
    print(f"  {len(wf)} artifact-producing workflow step(s) stage licenses:")
    for c in wf:
        print(f"    - {c}")
    print(f"  {len(scripts)} packaging script(s) stage licenses:")
    for c in scripts:
        print(f"    - {c}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
