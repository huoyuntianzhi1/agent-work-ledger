# Contributing to Agent Work Ledger

Thanks for taking the time to look at Agent Work Ledger (AWL). The
project is local-first by design — every contributor decision is
made in the open on GitHub, and the public artifact is the only
place users can find support.

## TL;DR for first-time contributors

- **Open an issue before opening a PR.** Even for tiny changes. The
  connector architecture has 11 specialized modules and we want to
  flag cross-cutting concerns before you spend a day on it.
- **Reproduce the bug / verify the feature on a real machine
  first.** Synthetic fixtures are fine for unit tests, but every
  shipped connector is expected to have been validated against the
  real data dir before it lands in `main`.
- **Match the existing module shape.** Look at the smallest
  connector in `app/src-tauri/src/connectors/` for the
  `Connector` + `ScanCtx` + `ScanReport` pattern; the test in
  the same file for the home-dir mutex pattern; the privacy
  pre-filter contract in `app/src-tauri/src/redactor.rs`.

## Code of conduct

This project follows the [Contributor Covenant](./CODE_OF_CONDUCT.md).
Be specific, be evidence-based, be patient with new contributors.
The maintainer (`@huoyuntianzhi1` at the moment) reserves the right
to lock threads when they drift off-topic.

## How to build and test

```sh
# Rust core
cd app/src-tauri
cargo test                       # 138 lib + integration tests
cargo build --release            # release binary

# Frontend
cd ../..
pnpm install
pnpm tauri dev                   # live dev mode
pnpm tauri build                 # production .app + .dmg
```

Integration tests against real Chromium IndexedDB are
`#[ignore]`-tagged by default and opt-in via the host having the
real app's data dir present. See `tests/capabilities_lint.rs` for
a 0-runtime-dep lint you should keep green.

## Commit style

- One concern per commit.
- Subject line ≤ 72 chars, imperative mood ("fix export dialog
  permission" not "fixed").
- Body explains *why* not what — code review is for the what.

## Pull request checklist

- [ ] Tests added or updated.
- [ ] `cargo test` green.
- [ ] If you touched a Tauri 2 capability, update
      `tests/capabilities_lint.rs`.
- [ ] If you added a connector, exercised it on a real data dir
      (paste the dev-output log into the PR body).
- [ ] If you bumped the binary behaviour, updated the bundle /
      re-signed the .app and verified the install on this machine.

## Privacy contract

Anything that ends up in the SQLite ledger must go through
`Redactor::global()`. This is non-negotiable. The pre-filter covers
`sk-*` / `AKIA*` / `Bearer *` / JWT / 32-char hex out of the box;
new patterns are added at the regex level, not at the connector
level, so a future cloud-provider bolt-on inherits the same
guarantee.

## Releasing

Versions follow `vX.Y.Z` where `X` tracks breaking schema changes
and `Y` tracks notable features. The maintainer cuts a release
after each merge window; the `.app` is re-signed and reinstalled
on the maintainer's machine as the canary, then a tag is pushed.
The `CHANGELOG.md` is updated at the same time.
