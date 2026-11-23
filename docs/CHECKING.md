# Code checking

`scripts/check.sh` is the one-command verification suite for the Unified
Pentagonal Project. It runs every check the project has — formatting,
clippy, per-crate `cargo check`, per-crate `cargo test`, the TypeScript
check, and finally the root `unified` bin — prints a PASS/FAIL/SKIP line
per stage, prints a summary, and exits non-zero when any stage failed.
Every stage runs even if an earlier one failed.

## Stages

1. `cargo fmt --all -- --check` — report only; the script never reformats
   the tree. Fix diffs with `cargo fmt --all` before pushing.
2. `cargo clippy --workspace --all-targets` — warnings denied.
3. `cargo check` on every workspace member except the root `unified` bin
   (unified-core, unified-scripting, unified-ts, the seven unified-db-*
   crates, the five external-* crates, example-hello-module,
   unified-calculator).
4. `cargo test` on the member crates that have tests (unified-core,
   unified-scripting, unified-ts, the five external-* crates,
   unified-calculator). The seven unified-db-* crates and
   example-hello-module have no tests.
5. TypeScript check (`npm run check`, i.e. `tsc --noEmit`) when
   `node_modules` exists; otherwise the stage is skipped with a note.
   The script never runs `npm install`.
6. Root bin, last: `cargo check -p unified` plus `cargo test -p unified`.

`--features gateway` additionally runs the clippy and check steps for the
root bin with the Pingora-gated gateway feature enabled.

## Running locally

```sh
bash scripts/check.sh                  # everything
bash scripts/check.sh --no-root-bin    # small VM: skip stage 6
bash scripts/check.sh --no-clippy --no-test --no-ts   # fastest meaningful run
```

Skip flags: `--no-clippy`, `--no-test` (also skips the root-bin tests),
`--no-root-bin`, `--no-ts`. `UNIFIED_SKIP_ROOT_BIN=1` is the same as
`--no-root-bin`. `UNIFIED_OFFLINE=0` allows network access; the default is
`--offline` everywhere, so a local run never downloads anything.

## Why the root bin is last (RAM)

The dev VM has 1.5 shared CPUs and 7.7 GiB of RAM. A single `rustc`
compiling the root `unified` bin crate peaks at ~5.7 GiB RSS and is killed
by the OOM killer there (lean build run 4, 2026-11-23: exit 137, zero
compile errors). The per-crate checks in stages 3–4 fit comfortably with
the lean environment the script sets (`CARGO_BUILD_JOBS=1`,
`CARGO_INCREMENTAL=0`, `RUSTC_WRAPPER=sccache` when sccache is installed,
and the shared `~/workspace/unified-target` target directory when it
exists). Only stage 6 needs a bigger machine — skip it locally with
`--no-root-bin` and let CI cover it.

## CI

`.github/workflows/ci.yml` runs on push and pull requests to `main`, on
`ubuntu-latest`, in a single job: it installs Rust stable (rustfmt +
clippy), `libpq-dev` and `mold` (mold is best-effort), sccache
(best-effort), caches the cargo registry and target directory, runs
`npm ci`, and then runs `bash scripts/check.sh` with no skips — CI
runners have the RAM the root bin needs. A final step repeats the root
bin check and clippy with `--features gateway`.
