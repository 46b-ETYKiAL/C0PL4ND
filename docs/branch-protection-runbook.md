# Branch-protection / required-checks runbook (audit #5)

`master` currently has **no required status checks** — CI is informational, so a
red check (e.g. the long-standing `Supply-Chain Vet` debt, fixed by the
cargo-vet drain PR) can sit on `master` and still be `master`. This runbook turns
the green checks into **required** ones.

## Ordering: fix-then-require (mandatory)

You cannot sanely *require* a check that is currently **red on `master`** — with
`enforce_admins` on it chicken-and-eggs the very fix. So:

1. **Merge the supply-chain drain PR first** so `master` HEAD goes green on
   `Supply-Chain Vet` / `cargo-vet`.
2. Confirm `master` HEAD is green on the checks you intend to require
   (`gh run list --branch master --limit 1`).
3. *Then* enable required checks (below), starting with `enforce_admins:false`.

## The required-check set

Require the lean, fast, deterministic gates — keep slow/flaky suites
informational. Recommended contexts (exact check names):

- `Build & Test (ubuntu-latest)`, `Build & Test (windows-latest)`, `Build & Test (macos-latest)`
- `Format & Clippy`
- `Core line coverage (>= 88%)`   ← the byte-stable Linux gate name (do NOT rename it)
- `Miri soundness (pure core engine)`
- `W1TN3SS reporting-integration coverage`
- `cargo-vet` (only after the drain PR has merged green)
- `Dependency review`, `Supply-chain Audit`, `No-Network Gate`, `Zero-egress invariant`

> The per-OS coverage jobs added in this PR (`Core line coverage (windows)` /
> `(macos)`) can be added to the required set once their first runs confirm the
> chosen floors hold.

## Enable (PUT replaces the whole protection object — send all four keys)

```bash
gh api -X PUT repos/46b-ETYKiAL/Itasha.Corp_C0PL4ND/branches/master/protection \
  --input - <<'JSON'
{
  "required_status_checks": {
    "strict": false,
    "checks": [
      { "context": "Build & Test (ubuntu-latest)" },
      { "context": "Build & Test (windows-latest)" },
      { "context": "Build & Test (macos-latest)" },
      { "context": "Format & Clippy" },
      { "context": "Core line coverage (>= 88%)" },
      { "context": "Miri soundness (pure core engine)" },
      { "context": "W1TN3SS reporting-integration coverage" },
      { "context": "Dependency review" },
      { "context": "Supply-chain Audit" },
      { "context": "No-Network Gate" },
      { "context": "Zero-egress invariant" },
      { "context": "public-repo content-safety audit" },
      { "context": "gitleaks" },
      { "context": "CI Gate" }
    ]
  },
  "enforce_admins": false,
  "required_pull_request_reviews": null,
  "restrictions": null
}
JSON
```

- `CI Gate` is the aggregating job in `ci.yml`: it inspects every other job's
  result explicitly and fails if any is not `success`, so a skipped or
  cancelled job cannot satisfy it. Requiring it is what makes the individual
  contexts above impossible to bypass by skipping.
- `public-repo content-safety audit` runs `tests/content_safety_audit.py`
  (internal references, developer paths, personal mailboxes, work-tracking
  tokens) after first running its own falsification suite, so a guard that has
  stopped detecting something fails loudly instead of reporting a clean tree.
  `gitleaks` covers credential shapes over full history, using `.gitleaks.toml`.
- `strict:false` is correct when using a **merge queue** (the queue handles
  up-to-dateness); set `true` only without a queue.
- Start `enforce_admins:false` so you can still admin-merge a hotfix; flip to
  `true` once the set is stable.

## Merge-queue "skipped ≡ success" footgun

If you adopt a merge queue, every required workflow MUST trigger on **both**
`pull_request` and `merge_group` with an *identical check name*, and MUST NOT be
path-filtered at the `on:` level — a skipped job counts as success and lets a PR
merge without the check ever running. Decide skip *inside* the job with
`dorny/paths-filter` + `if: always()`. This repo's `merge-queue-check-gating.md`
rule documents the pattern.

## Drift probe

`gh api repos/46b-ETYKiAL/Itasha.Corp_C0PL4ND/branches/master/protection/required_status_checks --jq '.checks[].context'`
should list exactly the set above; a `404 "Required status checks not enabled"`
means protection is off.
