# Porting between SCR1B3 and C0PL4ND

**Read this before copying anything between the two repos.**

SCR1B3 and C0PL4ND ship independently, on separate cadences, from separate
release workflows. They are not a monorepo and there is deliberately **no shared
release crate** — coupling two independently-versioned products through a shared
library trades a small amount of duplication for a release-blocking dependency
between them, which is a worse bargain than the duplication.

The cost of that decision is real, though, and it is specific: the two repos keep
re-learning the same packaging lessons independently, in whichever order the bugs
happen to surface. This file is the cheap alternative to a shared crate. It is a
**ledger of what has actually diverged and which direction each fix travels** —
not a style guide, and not a list of things that would be nice to unify.

## The rule this file exists to enforce

> **Establish which side is correct *before* you port. The newer repo is not
> automatically the better one, and neither is the one that fixed it last.**

This is not a caution in the abstract. Entry **C** below is a near-miss: at the
time the divergence was first written up, SCR1B3's content-safety audit was the
strict superset — it carried an opaque-URI (`mailto:`) fix that C0PL4ND lacked,
so the obvious reading was "port SCR1B3 → C0PL4ND". By the time anyone acted on
it, C0PL4ND had absorbed the `mailto:` fix *and* added five more classes SCR1B3
still lacks. Porting in the originally-recorded direction would have **deleted
working detections** from C0PL4ND and shipped a regression in a security gate,
while every test stayed green — because a gate that has stopped detecting
something reports a clean tree and looks exactly like success.

So the sequence is always:

1. **Diff both sides.** Not the changelogs, not this file — the actual files.
2. **Decide which behaviour is correct**, on the merits, for each *individual*
   difference. A file is rarely wholly ahead; usually each side holds one half.
3. **Run the receiving repo's falsification harness first, then its audit.**
   Harness first is load-bearing: it proves the guard can still fail. An audit
   that passes tells you nothing until you know the audit can still go red.
4. Only then port, and only the half that is actually behind.

Entry **B** is the same shape without the near-miss: each repo held one half of
a correct pattern, and either "port A → B" or "port B → A" alone would have been
wrong.

## About this file being duplicated

This file is **byte-identical in both repos** (`packaging/PORTING.md`). That is
intentional and it is the one place duplication is the right call: a porting
ledger that lives in only one of two independent repos is invisible from the
other exactly when it is needed.

To keep the two copies from drifting into the very problem they document, the
table below names repos **absolutely** (`SCR1B3`, `C0PL4ND`) rather than
relatively ("this repo" / "the sibling"), so the two copies never need to differ.
**Any edit here must land in both repos.**

## Status legend

| Status | Meaning |
|---|---|
| **OPEN** | Divergence is live. The direction column says which way the fix travels. |
| **IN FLIGHT** | Fix exists on an unmerged branch. Do not port it twice — land the branch. |
| **CLOSED** | Both sides agree. Recorded so the *old* shape is not re-introduced by a future port. |

---

## A. Prerelease / ref classification — one predicate, one home

**Status: IN FLIGHT (SCR1B3) · direction: adopt C0PL4ND's filename**

Both repos need the same answer to "is this tag a SemVer prerelease?" in more
than one place, and those places **must agree**. The failure mode when they do
not is not a cosmetic inconsistency — it is an artifact that is simultaneously
"unsigned because it is a prerelease" and "published as latest because it is
stable", i.e. an unsigned build served to every download link and offered to an
updater built to reject exactly those bytes.

Both repos independently discovered that `contains(ref_name, '-')` /
`case $REF in *-*)` — "contains a hyphen anywhere" — is the wrong predicate. The
workflows trigger on `v*`, so that rule silently classified `v2026-08-10`,
`v1.0-final` and `v0.5-hotfix` as prereleases. Both repos independently landed
the same real SemVer test, and both extracted it to its own file so it has one
home. **The remaining divergence is only the filename:**

| Repo | File | Consumers |
|---|---|---|
| C0PL4ND | `packaging/release-ref-class.sh` | `require-signing-key.sh`, `require-installer-signing.sh`, `require-macos-signing.sh`, `release.yml` publish-classify step, plus `falsify_release_gates.py` and `mutate_release_gates.py` |
| SCR1B3 | `packaging/semver-tag-class.sh` — **on the unmerged branch `fix/gates-that-cannot-fail`** | `require-signing-key.sh`, `release.yml`, and an integration test in `crates/scribe-app/src/integration/mod.rs` |

**Decision: adopt `release-ref-class.sh`.** Not because C0PL4ND is newer — it is
not — but on consumer count and merge state: C0PL4ND's name is merged and pinned
by six consumers including two falsification harnesses, while SCR1B3's is
unmerged with three. Renaming the unmerged side with fewer consumers is strictly
the smaller change.

**The rename is owned by whoever lands `fix/gates-that-cannot-fail`,** because
the file does not exist on SCR1B3's mainline yet and its falsification test lives
on that same branch. Doing it anywhere else means editing an in-flight branch and
being unable to run the test that pins it. When landing that branch:

```
git mv packaging/semver-tag-class.sh packaging/release-ref-class.sh
# then update all three consumers:
#   packaging/require-signing-key.sh   (the `$(dirname "$0")/...` call)
#   .github/workflows/release.yml      (the `src/packaging/...` invocation)
#   crates/scribe-app/src/integration/mod.rs  (the `SCRIPT` const + the ../../ join)
```
and re-run that integration test — a rename that compiles is not a rename that
still fires.

**Related, same root cause:** on SCR1B3's mainline the publish step runs
`gh release edit --draft=false --latest` *unconditionally*, so a prerelease tag
is promoted to "Latest release". C0PL4ND sets `prerelease:` from the shared
classifier. SCR1B3's fix is on the same unmerged branch.

## B. Release-drift check — each repo held one half

**Status: OPEN · direction: C0PL4ND → SCR1B3**

`release-drift-check.yml` exists in both repos and catches "version bumped in
source but never released". SCR1B3 had the job first; C0PL4ND ported it and then
hardened two things SCR1B3 still lacks. Neither repo was wholly ahead.

**B1 — "no releases" and "could not ask about releases" must not be one branch.**

SCR1B3 currently has:

```sh
latest="$(gh release view --json tagName -q .tagName 2>/dev/null | sed 's/^v//' || true)"
if [ -z "$latest" ]; then
  echo "::warning::no published releases yet — nothing to compare against."
  exit 0
fi
```

That collapses *every* failure mode — auth expiry, rate limit, network blip,
egress policy, `gh` missing from the image — into a vacuous pass. Because the
job runs on cron and on every master push, a persistent auth fault makes it
silently inert forever while logging a green premise that is false. It also
disagrees with the tag probe further down the same script, which already fails
closed on an API error.

C0PL4ND separates the **status** from the **output**: it probes reachability with
`gh release list`, hard-errors (with `gh`'s own stderr in the message) on a
non-zero status, and treats only a *successful* query returning nothing as the
genuine no-releases case. Port that shape into SCR1B3.

**B2 — the unreachable `-z` guard.** SCR1B3 reads the workspace version with a
bare `grep … | sed …` under `set -euo pipefail`. A `Cargo.toml` with no version
line makes `grep` exit 1, `pipefail` propagates it, and the script dies **on the
assignment**, before the `if [ -z "$src" ]` guard below it can run — so the
diagnostic is unreachable code and the failure has no message at all. C0PL4ND
carries a `|| true` there specifically to make that guard reachable, with a
comment saying so. Port it, and keep the comment: without it the `|| true` reads
as defensive noise and will be "cleaned up" by the next person.

**B3 — minor.** C0PL4ND's job runs `step-security/harden-runner` in egress-audit
mode; SCR1B3's does not. Port at leisure; unlike B1/B2 this does not affect
whether the gate can fail.

## C. Content-safety audit — **the direction inverted; check before porting**

**Status: OPEN · direction: C0PL4ND → SCR1B3 (this is the reverse of the
originally-recorded direction — see "The rule" above)**

Same tool, different home: `scripts/content_safety_audit.py` in SCR1B3,
`tests/content_safety_audit.py` in C0PL4ND. Both pass on their own trees.

Both repos now carry the opaque-URI fix (`mailto:user@host` is a scheme, not a
personal mailbox) that SCR1B3 had first. **C0PL4ND has since added five classes
SCR1B3 still lacks:**

- **Home paths reached through a cross-OS mount.** The plain `/home/` and
  `/Users/` patterns carry a `(?<![A-Za-z0-9])` lookbehind to avoid firing on
  relative paths, and every mount prefix (`/mnt/c`, `/cygdrive/c`, `\\wsl$\…`)
  ends in an alphanumeric — so that lookbehind made a **real** home path under a
  mount invisible. C0PL4ND enumerates the mount prefixes rather than relaxing the
  lookbehind, which adds no false-positive surface.
- **Escape depth.** `[\\/]{1,4}` rather than `{1,2}`, so a path escaped twice
  (`C:\\\\Users\\\\x`, the form that appears in a literal embedded in another
  literal) is still seen.
- **Backtick stripping** in the account-name capture, and backticks in the
  terminating character class.
- **More scanned suffixes** (`.rtf`, `.pub`, `.1`) that were previously published
  unscanned, and **the forge's own bare noreply address** in the allowed-email
  set. Omitting it made the two halves of the file disagree: `audit_identities`
  already classified that address as non-PII drift rather than a leak, so a file
  that merely *documented* it was reported as carrying a personal mailbox.

  > **This divergence was demonstrated by this very file.** An earlier draft
  > spelled that address out literally. C0PL4ND's audit passed it; SCR1B3's
  > failed it as a `personal email address` — same bytes, two verdicts. The
  > sentence was reworded rather than SCR1B3's allowlist widened, because
  > widening a detection rule to make a document land is how a gate quietly
  > stops detecting. Port the allowlist entry as part of this section's work,
  > with the harness re-run, and this paragraph can go back to naming it.
- **`--hash` prints the *probe* digests, not the raw token's.** The scanner never
  hashes a token as typed — it hashes normalised probe forms. Registering
  `token_digest(raw)` therefore produced entries that could never match: not
  loose suppressions but **dead** ones, indistinguishable in the table from
  working ones.

**C1 — SCR1B3's falsification harness is not wired.** SCR1B3's CI runs only
`scripts/content_safety_audit.py`; `scripts/test_content_safety_audit.py` exists
but nothing executes it. C0PL4ND runs the harness **first**, then the audit, in
both CI and a pre-push hook, with a comment explaining the ordering. An unrun
harness is worth exactly as much as no harness.
*(A fix is IN FLIGHT on SCR1B3's `feat/pii-forward-guard` branch, which also adds
`author_identity_guard.py` + its harness — C0PL4ND has both; SCR1B3's mainline
has neither. Check that branch before porting any of C1.)*

**C2 — filename/location.** `scripts/` vs `tests/`. Deliberately **not**
resolved here: unlike entry A there is no correctness argument either way, both
sides have local co-location reasons, and both files are under active edit on
in-flight branches in both repos. A cosmetic rename that collides with live work
costs more than the inconsistency. Revisit when both branches have landed.

## D. One-line installer — resolved on both sides

**Status: CLOSED — recorded so the old shape is not re-introduced**

Both installers once pointed at branch `main` while both repos' default branch is
`master` (so the documented one-liner fetched nothing), and neither verified a
signature. Both are now fixed: `master`, fail-closed, minisign-verified against a
public key embedded in the script, with the archive checked against a **signed**
digest manifest rather than a bare checksum. C0PL4ND's fix landed first
(`packaging/linux/install.sh`); SCR1B3 followed (`packaging/install.sh`).

If you are porting installer changes, **do not** take the pre-fix shape from any
older reference: an installer that fetches over TLS and checks only a SHA-256 it
fetched from the same place verifies nothing an attacker who controls that place
cannot also forge.

## E. Mutation config — split, and not in the expected direction

**Status: OPEN · direction: both ways (see below)**

Both repos gate on `cargo-mutants` and both have a staleness problem, but they
solved *different halves* and each lacks the other's canary.

**E1 — examine-glob staleness. C0PL4ND has the canary; SCR1B3 does not.**
C0PL4ND's `.cargo/mutants.toml` restricts mutation to `examine_globs`, and a
glob that stops matching selects **zero** mutants while the run reports success.
Its `mutants.yml` asserts, per examine area, that the area selects `> 0` mutants,
and that the total is `< 4000` (a total that large means the config was not read
at all — `cargo-mutants` reads `.cargo/mutants.toml`, and a `mutants.toml` in the
repo root is silently ignored). The glob itself is now multi-level
(`**/update_engine/**/*.rs`), so a subdirectory split no longer drops a
security-critical file out of the gate.

SCR1B3 uses no `examine_globs` (it mutates everything not excluded), so it does
not need the per-area assertion *today* — but it also has **no assertion that its
mutants config is applied at all**, which is the other half of C0PL4ND's canary
and applies regardless. Port that half.

**E2 — pardon-position staleness. SCR1B3 has the canary; C0PL4ND does not.**
SCR1B3 carries `crates/scribe-app/tests/mutation_pardons_are_not_stale.rs`, which
exists because a coordinate is not a stable key: `exclude_re` entries pinned to
`file.rs:LINE:COL` slide onto a different statement after any edit above or to
the left, and then suppress the **wrong** mutant — or nothing — while still
looking like coverage. That is not hypothetical in SCR1B3; the migration pass
that wrote the test found eight anchors matching zero mutants and one silently
pardoning an unrelated function. The test classifies position-pinning
*semantically* (after normalising regex spelling, so escaped-colon and
character-class spellings cannot evade it) on **both** axes, and requires each
positional pardon to declare the source text that must still sit at its
coordinate.

C0PL4ND has seven `exclude_re` pardons with no equivalent guard. Port SCR1B3's
canary in that direction — it is the more valuable half of this entry, because
C0PL4ND's pardons include ones justified as "provably equivalent", which is
exactly the class that silently stops being true.

**E3 — a stale comment left behind by the E1 fix.** C0PL4ND's `mutants.yml`
still says the config "must use `**/update_engine/*.rs`" while `.cargo/mutants.toml`
now correctly says `**/update_engine/**/*.rs`. In-repo, not cross-repo, and
harmless to the gate — but it is the "fixed the defect, did not revisit the prose"
shape, and prose that contradicts the code is what the next porter reads first.

## F. Release-gate falsification — C0PL4ND only

**Status: OPEN · direction: C0PL4ND → SCR1B3**

C0PL4ND has `packaging/falsify_release_gates.py` (88 checks: every signing,
checksum, licence-count and wiring gate shown **RED** on bad input and **GREEN**
on good input, including that each gate is invoked at a path that actually
resolves at runtime) plus `packaging/mutate_release_gates.py`, a **control** that
mutates the gates to prove the falsifier can still see a regression. Both run on
every push and PR, not only at tag time.

SCR1B3 has no equivalent. Its signing gate is exercised by one integration test.
This is the largest single gap in the ledger: SCR1B3's release gates are, for the
most part, unfalsified — and a release gate is precisely the kind that runs rarely
enough for a silent break to go unnoticed until the release it was meant to stop.

---

## Adding an entry

An entry earns its place if it is **grounded in a real difference you diffed**,
and it must say which side is correct **and why** — consumer count, merge state,
a falsification result, a named failure mode. "Repo X is newer" is not a reason;
entry C is the record of where that reasoning leads.

When a divergence closes, move it to **CLOSED** rather than deleting it. The
value of entry D is not that the installers agree — it is that nobody
re-introduces the shape they used to have.
