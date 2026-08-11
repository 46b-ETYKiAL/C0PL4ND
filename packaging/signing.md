# Release Signing (minisign / ed25519)

C0PL4ND release artifacts are signed with [minisign](https://jedisct1.github.io/minisign/)
(ed25519), using the **same convention as the sibling SCR1B3 editor** so every
Itasha app shares one in-app-update signing flow. The in-app updater verifies the
signature against a public key **embedded in the binary** before swapping — so an
update is applied only if it came from the holder of the secret key. SHA-256
checksums provide a second, independent integrity layer.

## One-time key generation (maintainer, offline)

Use a **passwordless** key so the non-interactive CI sign needs no password:

```sh
rsign generate -W -p c0pl4nd.pub -s c0pl4nd.key   # rsign2 (cargo install rsign2)
# or, equivalently: minisign -G -W -p c0pl4nd.pub -s c0pl4nd.key
```

- `c0pl4nd.key` — the **secret key**. NEVER commit it. Store it in the CI secret
  `MINISIGN_SECRET_KEY` (and an offline backup / password manager).
- `c0pl4nd.pub` — the public key. Copy its base64 line into
  `crates/app/src/update_engine/verify.rs` → `EMBEDDED_PUBLIC_KEY`.

The current release keypair's public half is committed in `verify.rs`. The
matching secret half is held offline by the maintainer.

**Where it is held is deliberately not recorded in this repository.** A public
document that names the file and the directory of a *passwordless* signing key
hands an attacker who gains any read access to the maintainer's machine the exact
path to the artifact that authorises every in-app update — the one secret that
turns the updater from a defence into a delivery channel. Keep the location in a
password manager, not in a tracked file.

## Signing in CI (`.github/workflows/release.yml`)

The signing flow is **automated**, gated on the secret being set (mirrors SCR1B3
verbatim):

| Secret | Kind | Purpose |
|---|---|---|
| `MINISIGN_SECRET_KEY` | secret | The **passwordless** ed25519 secret key (`rsign generate -W` / `minisign -G -W`). Present → the release job installs rsign2 and signs every asset (`*.minisig`). Absent → the job logs a `::warning::` and ships checksummed-but-**unsigned** artifacts (the in-app updater then rejects them — fail-closed). The CI signs with rsign2; the app verifies with the `minisign-verify` crate (interoperable). |

For each release asset the job runs (signing with the **bare basename from
inside the asset directory** — see the critical note below):

```sh
printf '%s\n' "$MINISIGN_SECRET_KEY" > sk.key
# Sign from INSIDE release/ with the bare filename, NOT a path-prefixed arg.
( cd release && rsign sign -W -s "$PWD/../sk.key" -x "<asset>.minisig" "<asset>" )
sha256sum "release/<asset>" > "release/<asset>.sha256"   # produced in the build job
```

> **CRITICAL — sign the bare basename.** minisign records the *exact path
> argument* it was given as the trusted comment (`file:<arg>`). The in-app
> updater binds that token to the **bare downloaded asset name**, so you must
> `cd` into `release/` and pass just `<asset>` — signing `release/<asset>`
> writes `file:release/<asset>` into the trusted comment, and the fail-closed
> updater rejects the artifact ("trusted-comment file mismatch"), breaking
> auto-update for every deployed client. The release workflow enforces this via
> `( cd "$(dirname "$f")" && rsign sign … "$(basename "$f")" )`.

`<asset>`, `<asset>.minisig`, and `<asset>.sha256` are all uploaded to the GitHub
Release. The updater downloads all three, verifies checksum + signature
(`update_engine::verify::verify_artifact`), and only then applies
(`update_engine::apply`).

## Activation (one-time, repo owner)

1. Add the contents of the offline secret-key file as the `MINISIGN_SECRET_KEY`
   repository **secret**.
2. Cut a tagged release (`v*`). The release job signs every asset; the shipped
   binary's in-app updater verifies and installs them.

Until the secret is set, releases are unsigned and the updater is inert **by
design** — never insecure.

## Windows code signing (separate concern)

Authenticode-signing the `.exe`/installer (so SmartScreen/AV trust the
self-replace swap) is independent of the minisign update-integrity chain
(Authenticode = OS/AV trust; minisign = our update authenticity). Handled by the
F0RG3-W1R3 installer pipeline.

## Threat model

- Secret-key compromise → attacker can sign malicious updates. Mitigation: key
  stored only in the CI secret + offline backup; rotate by shipping a new embedded
  public key in a normally-signed release before retiring the old key.
- The updater refuses unsigned, wrong-signed, or checksum-mismatched artifacts
  and keeps the prior binary for rollback.
