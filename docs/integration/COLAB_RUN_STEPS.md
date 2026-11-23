# Colab Run Steps — Unified root-binary `cargo check` (task 307)

Last verified: 2026-11-23. Notebook `colab/Unified_cargo_check_private.ipynb`
is byte-identical in three places: the live tree, GitHub `main`
(`ArrayIterator/unified`, blob `5813251cbf1616ae34ab8d75adb994f4e2ef09f0`),
and Google Drive (`Unified_cargo_check_private.ipynb`, modified
2026-11-22 20:09 WIB). Any of the three copies is the current one.

## Current status — READ BEFORE RUNNING (2026-11-23)

The notebook is ready, but the tree is NOT: the lean `cargo check` run
on 2026-11-23 (log `~/workspace/unified-check/lean-build-20261123-prod.log`)
finished with **exit 101 — 341 compile errors** in the root `unified`
binary (`error: could not compile 'unified' (bin "unified") due to 341
previous errors`). A Colab run right now will fail Step 5 with the same
errors. Do not run the notebook expecting exit 0 until a fix round lands.

What the 341 are (top counts): `require_tenant` not found ×95,
`caller_company` ×93, `require_record_in_company_scope` ×57, `debug`
×30, `require_course_by_id_in_scope` ×14, `AdmissionsState` defined
multiple times ×8, `course_in_scope` ×6, `in_company_scope` ×5,
`Equipment` defined multiple times ×3, unresolved imports
`self::auth::resolve_company_id` / `self::helpers::resolve_company_id`
×3, plus smaller E0425/E0432/E0252. Affected files span properties,
elearning, domain_verification, content, field_service, library,
timetracker, gamification — a shared-helper wiring break, not one module.

This is a NEW error set, not the old 29: run3 (2026-11-22 19:32) had 29
errors of a different kind (`allow_private_probes`, `MAX_REFERENCE_LEN`,
`hash_value`, `parse_timestamp`, …) which were fixed in source, and run4
(2026-11-22 20:17) died of OOM before printing any diagnostics — that is
where the "zero error diagnostics" impression came from. The 341 arrived
with the checkpoint sync `1ee8657` (2026-11-23 10:37 WIB, full local tree)
and task 324 `417bee0`; the React track `ed95d5f` touched none of the
top error files. One rustc in the lean run was also SIGKILLed (signal 9,
OOM) at the end, but cargo still completed and reported exit 101 — the
341 errors are real, not an OOM artifact. Fix round needed first; then
re-run this notebook (or the lean VM check) to confirm exit 0.

## Why Colab

The root unified binary (`cargo check` on the workspace root) has zero
error diagnostics in source — all 29 pre-existing compile errors were
fixed — but the VM kills the `rustc` process with OOM at ~5.7 GiB RSS on
a 7.7 GiB machine. The code cannot be proven on this VM. Google Colab's
free runtime (~12 GB RAM, 2 vCPU) has enough headroom, so the check runs
there instead. This is a verification route only: nothing about the
codebase changes.

## What you need before you start

- The notebook: open the Drive copy
  (`Unified_cargo_check_private.ipynb`) and choose **Open with >
  Google Colaboratory**, or open the copy in the repo at
  `colab/Unified_cargo_check_private.ipynb` directly in Colab.
- A GitHub classic Personal Access Token (PAT) with the `repo` scope.
  Create one at <https://github.com/settings/tokens> (Tokens classic).
  A fine-grained token does NOT work for the git clone step.
- No other setup: the notebook installs Rust itself if it is missing.

## Run steps (in order — Runtime > Run all, or cell by cell)

1. **Step 1 — GitHub token.** A password box appears. Paste the classic
   PAT and press Enter. The token is written to `~/.git-credentials`
   (mode 0600) for git only and is NEVER saved in the notebook file.
   It is asked fresh on every run.
2. **Step 2 — Clone.** Clones `https://github.com/ArrayIterator/unified.git`
   into `/content/unified` and prints the last 3 commits. Confirm the
   head commit is the one you expect (the React track landed as
   `ed95d5f` on 2026-11-23; anything newer is fine as long as it is the
   pushed head).
3. **Step 3 — Dependency fixes.** Applies the known fixes to the
   throwaway clone only (never committed to main):
   `x509-certificate` 0.27 → 0.25, reqwest `rustls` →
   `rustls-tls-webpki-roots`, lettre `tokio1-rustls` →
   `tokio1-rustls-tls`, diesel gains `64-column-tables` + `serde_json`,
   and `src/main.rs` gains `#![recursion_limit = "1024"]`.
   NOTE: as of 2026-11-23 these fixes are already present in the pushed
   tree (`Cargo.toml` lines 22/39/45/50 and `src/main.rs` line 1), so
   this step is currently a harmless no-op — the `sed` commands simply
   match nothing and the `git diff` prints empty. That is expected, not
   a failure. Leave the step in place: it is the safety net if any fix
   ever regresses.
4. **Step 4 — Install Rust.** Installs stable Rust only if `cargo` is
   missing, then prints `rustc --version` and `cargo --version`.
5. **Step 5 — `cargo check`.** The real check. Full output goes to
   `cargo-check.log`. The cell prints `cargo check exit: N`, the count
   of `error` lines, the first 20 error lines (if any), and the last
   30 lines of the log.
6. **Step 6 — `cargo build --release`.** Runs ONLY if Step 5 exited 0.
   Skipped automatically otherwise. This is the slow step.
7. **Step 7 — Release binary.** Only if the build exited 0. Finds
   `target/release/unified`, prints size + sha256, and uploads it as
   an asset to a new prerelease tagged `colab-build-<timestamp>`. If
   the upload fails, the step reports it and continues — the binary
   stays in Colab for manual download. Binaries over 2 GB are left in
   Colab (GitHub Release asset limit).
8. **Step 8 — Push results.** Copies `cargo-check.log` (and, when
   present, the per-crate fingerprints + `deps-list.txt`) to the
   `colab-logs` branch as `colab/logs/cargo-check-<timestamp>.log`.
   Main is never touched; the branch holds only result files. Needs a
   token with Contents write. If the push fails, the check result
   printed in Step 5 is still valid — report it in chat instead.

## What to expect

- `cargo check exit: 0` and `error lines: 0` in Step 5: the root binary
  compiles. Warnings are fine — the VM runs already showed 29 warnings
  and zero errors before the OOM kill.
- `cargo check exit: 101` (or any non-zero) with error lines: copy the
  printed error lines (first 20) plus the tail into chat; do not rerun
  the whole notebook until the errors are fixed in source.
- If the Colab runtime disconnects mid-run, the clone and build cache
  are gone (the session is ephemeral), but anything already pushed to
  the `colab-logs` branch or uploaded as a Release asset stays. Just
  re-run the notebook from Step 1.

## What an exit 0 means for task 307

An exit 0 from Step 5 is the first observed successful root-binary
check. It closes task 307 as Done: it proves the 29 pre-existing errors
are fixed in source and the earlier failures were the VM running out of
RAM, not code errors. Report the exit code, the `colab-logs` branch
path of the pushed log (`colab/logs/cargo-check-<timestamp>.log`), and
the head commit the notebook cloned, so the result can be tied to an
exact tree. If Step 6 also exits 0, the release binary in the
`colab-build-<timestamp>` prerelease is a bonus, not a requirement for
closing 307.

## Notes

- `cargo run` is intentionally not in the notebook: it needs PostgreSQL
  and a config file. Run it only after `cargo check` passes, with
  `unified -t` first.
- The Colab session is ephemeral: re-running later always starts from
  a fresh clone of whatever is on GitHub `main` at that moment. If the
  repo does not yet contain the latest work (the coordinator pushes
  once everything is ready), re-run after the push.
