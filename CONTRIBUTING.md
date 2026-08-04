# Contributing to C0PL4ND

Thanks for your interest in improving C0PL4ND — the operator's shell into the wired. This document covers how to set up a development environment, build and test the project, and submit changes.

By participating in this project you agree to abide by our [Code of Conduct](CODE_OF_CONDUCT.md).

---

## Ways to contribute

- **Report bugs** using the [bug report template](.github/ISSUE_TEMPLATE/bug_report.md).
- **Request features** using the [feature request template](.github/ISSUE_TEMPLATE/feature_request.md).
- **Improve documentation** — fixes to the README, CONFIG, and inline docs are very welcome.
- **Submit code** — bug fixes, performance work, platform support, and features.

For security vulnerabilities, **do not open a public issue** — follow the disclosure process in [SECURITY.md](SECURITY.md).

---

## Development setup

C0PL4ND is written in Rust. You'll need:

1. **Rust (stable)** via [rustup](https://rustup.rs). The pinned toolchain is defined in `rust-toolchain.toml`; rustup will install it automatically on first build.
2. A working GPU/graphics environment for your platform.

### Platform prerequisites

| Platform | Requirements |
| --- | --- |
| **Windows** | Windows 10 1809+ (for ConPTY). Visual Studio Build Tools with the C++ workload (MSVC linker). |
| **Linux** | A C toolchain (`build-essential`/`gcc`), plus development headers for your windowing stack (e.g. `libwayland-dev`, `libxkbcommon-dev`, and X11 dev packages). A Vulkan or OpenGL-capable driver. |
| **macOS** | Xcode Command Line Tools (`xcode-select --install`). |

### Clone and build

```bash
git clone https://github.com/46b-ETYKiAL/Itasha.Corp_C0PL4ND.git
cd Itasha.Corp_C0PL4ND
cargo build
```

A release build (optimized):

```bash
cargo build --release
```

Run the locally-built terminal:

```bash
cargo run
```

If you hit a runtime issue while developing (colour/`TERM` detection, GPU or
transparency fallback, or where config and logs live), see
**[TROUBLESHOOTING.md](TROUBLESHOOTING.md)**. The default keybindings are
documented in **[docs/KEYBINDINGS.md](docs/KEYBINDINGS.md)**.

---

## Build, test, lint, and format

Before opening a pull request, run the full local check suite. CI runs the same checks.

```bash
# Format (must produce no diff)
cargo fmt --all

# Lint — clippy must pass with no warnings
cargo clippy --all-targets --all-features -- -D warnings

# Test suite
cargo test --all

# Optimized build
cargo build --release
```

Quick pre-PR one-liner:

```bash
cargo fmt --all -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test --all
```

- **`cargo fmt`** — code must be formatted with the project's `rustfmt` configuration. PRs with formatting diffs will fail CI.
- **`cargo clippy`** — we treat clippy warnings as errors. Fix or, with justification in review, explicitly allow with a scoped `#[allow(...)]`.
- **`cargo test`** — add or update tests for any behavior you change. Bug fixes should include a regression test that fails before your fix and passes after.

---

## Pull request process

1. **Open an issue first** for anything beyond a trivial fix, so we can align on the approach before you invest time.
2. **Fork and branch.** Use a descriptive branch name (e.g. `fix/windows-conpty-resize`, `feat/sixel-scaling`).
3. **Keep PRs focused.** One logical change per PR. Smaller PRs review faster.
4. **Write a clear description.** Explain *what* changed and *why*. Link the related issue. Fill out the [pull request template](.github/pull_request_template.md).
5. **Include tests and docs.** Update [CONFIG.md](CONFIG.md) if you add or change a configuration option.
6. **Pass all checks.** `cargo fmt`, `cargo clippy -D warnings`, and `cargo test` must all be green.
7. **Use Conventional Commits** for commit messages where practical (`feat:`, `fix:`, `docs:`, `perf:`, `refactor:`, `test:`, `chore:`).
8. **Sign off on the license.** By submitting a PR you agree your contribution is dual-licensed under MIT OR Apache-2.0 (see [README License](README.md#license)).

A maintainer will review your PR. We aim to be responsive and constructive — see the Code of Conduct for the tone we hold ourselves and contributors to.

---

## Commit message convention

We use [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<optional scope>): <short summary>

<optional body explaining what and why>
```

Common types: `feat`, `fix`, `docs`, `perf`, `refactor`, `test`, `build`, `ci`, `chore`.

---

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). We are explicitly committed to treating every report and suggestion as worth a considered reply, even when the answer is no. Be kind, be specific, and assume good faith.

---

Thanks for helping build a terminal that respects its users. Welcome to the wired.
