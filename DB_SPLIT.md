# DB Layer Split — `unified-core/src/db` into `unified-db-*` crates

## Why

`cargo check -p unified-core` could never finish in this VM: one rustc
needed ~5.1–5.3 GiB RSS and was OOM-killed 6 times on a 7.7 GiB machine.
The cause was the Diesel layer living in a single crate: 675 `table!`
macros, 1,350 model structs with Diesel derives, and 5,416
`AllowTablesToAppearInSameQuery` impls (one per ordered pair inside each
join group) in `crates/unified-core/src/db/schema.rs` (12,683 lines) and
`models.rs` (19,657 lines).

## The split

Seven new workspace members, one per bounded context. Every join-graph
connected component stays whole inside one crate, so every `joinable!`
and every `allow_tables_to_appear_in_same_query!` group lives entirely
in one crate — no cross-crate join declaration exists.

| Crate | Tables | Models | Joinables | Allow groups | Allow impls |
|---|---|---|---|---|---|
| `unified-db-core` | 110 | 220 | 83 | 2 | 3,194 |
| `unified-db-commerce` | 128 | 256 | 85 | 16 | 512 |
| `unified-db-education` | 82 | 164 | 87 | 5 | 606 |
| `unified-db-people` | 52 | 104 | 34 | 5 | 238 |
| `unified-db-operations` | 113 | 226 | 75 | 10 | 426 |
| `unified-db-platform` | 114 | 228 | 74 | 18 | 342 |
| `unified-db-infra` | 76 | 152 | 31 | 21 | 98 |
| **Total** | **675** | **1,350** | **469** | **77** | **5,416** |

- `core`: identity, tenancy, platform foundation — users, roles,
  companies, domains, tenants, licensing, store, software, hosting,
  tasks, time tracking. Holds the indivisible 57-table mega component
  (users/companies/licenses/store/software/hosting/tasks/timetracker/VCS),
  which alone accounts for 3,192 of the allow impls.
- `commerce`: products, pricing, payments, billing, subscriptions,
  marketplace, affiliates, expenses, tax, marketing.
- `education`: academic records, e-learning, curriculum, admissions,
  exams, school fees, library.
- `people`: HR, recruiting, performance, supervision, holidays, todos,
  appointments.
- `operations`: projects, manufacturing, logistics, assets, field work,
  storage, clusters, incidents, risk.
- `platform`: email, newsletters, events, webinars, community, forms,
  surveys, documents, meetings, contracts, analytics.
- `infra`: compliance, privacy, gamification, network tools, gateway,
  webhooks, vault, web3.

## Compatibility

- No migration changes, no DB changes. Migrations still live in
  `crates/unified-core/migrations/` and are still embedded by
  `unified-core`.
- `crates/unified-core/src/db/schema.rs` and `models.rs` are now
  re-export shims (`pub use unified_db_*::schema::*` /
  `pub use unified_db_*::models::*`), so every existing
  `unified_core::db::schema::<table>` /
  `unified_core::db::models::<Struct>` path keeps working. No file
  outside `crates/unified-core/src/db/` was changed for the split.
- `src/db/money.rs` (PgNumeric conversions) and `src/money.rs` (JSON
  money) were not moved.
- Table/model text is byte-identical to the originals, only regrouped
  (verified by multiset comparison of all 675 `table!` blocks, 1,350
  model structs, 469 `joinable!` lines, and 77 allow groups).

## Verification (this VM, `CARGO_TARGET_DIR=~/workspace/unified-target`, `CARGO_BUILD_JOBS=1`)

| Check | Result | Peak rustc RSS |
|---|---|---|
| `cargo check -p unified-db-people` | pass, 7 s | 746 MiB |
| `cargo check -p unified-db-infra` | pass, 36 s | 1,116 MiB |
| `cargo check -p unified-db-education` | pass, 43 s | 1,218 MiB |
| `cargo check -p unified-db-core` | pass, 67 s | 1,644 MiB |
| `cargo check -p unified-db-platform` | pass, 64 s | 1,631 MiB |
| `cargo check -p unified-db-operations` | pass, 69 s | 1,775 MiB |
| `cargo check -p unified-db-commerce` | pass, 84 s | 1,937 MiB |

The largest single rustc seen in any run was `diesel` itself at
~3.4 GiB (first build only); every db crate stays under 2 GiB.

| Check | Result | Peak rustc RSS |
|---|---|---|
| `cargo check -p unified-core` | **FAIL, exit 101, 648 s**: 42 errors, all in non-DB modules (see below); no OOM, no db error | 3,462 MiB (`diesel`) |
| `cargo check --workspace` | **FAIL, exit 101, 509 s**: only error is the same 42 `unified-core` errors; no OOM anywhere in the run; the root `unified` bin was never reached (cargo stops on the `unified-core` failure), so its own compile/memory behaviour is still unmeasured | 3,461 MiB (`diesel`) |

## Known remaining blockers (not caused by the split)

- `cargo check -p unified-core` does NOT pass: 42 errors in non-DB
  modules (`tls.rs` 22, `cache.rs` 9, `hardening.rs` 6, `storage.rs` 3,
  `middleware.rs` 3, `error.rs` 2, `routes.rs` 2, `tenancy.rs` 2,
  `context.rs` 1, `hooks.rs` 1, `modules.rs` 1). Zero errors mention
  `db`, `schema`, `models`, or `unified_db`. These are pre-existing:
  the earlier cargo-check log already showed 61 errors in
  `unified-core`, and its "zero diagnostics" runs were runs where
  rustc was OOM-killed before emitting diagnostics, not clean passes.
- `cargo check --workspace`: fails only on the same 42
  `unified-core` errors (509 s run, no OOM anywhere, peak rustc
  3,461 MiB = `diesel`). The root `unified` bin was never compiled
  in that run, so its own errors (540 in the earlier cargo-check
  log) and its memory behaviour are still unmeasured. Once the 42
  `unified-core` errors are fixed, re-run it.
- `cargo test -p unified-core` is blocked by the same 42 errors, so
  the new `db_split_reexports_resolve` test in `db/mod.rs` has not
  been executed in place. Its exact paths were verified instead by a
  throwaway crate (`/tmp/shimtest`, not part of the repo) that
  reproduces the shims and the test's expressions: `cargo check
  --tests` passes (exit 0). The test binary itself cannot link in
  this VM (no libpq dev library: `rust-lld: error: unable to find
  library -lpq`), which is an environment limitation, not a code
  problem.

## Adding a table later

1. Add the `table!` to the schema of the crate that owns the table's
   bounded context, plus its two models (`X` / `NewX`) to that
   crate's models.
2. If the table joins another table, add the `joinable!` and extend
   the allow group in the same crate. If the two tables live in
   different crates, the join graph component must move whole into
   one crate (or the join cannot be expressed — merge the crates'
   groups instead; the compiler reports a missing pair as E0277).
3. No change is needed in `unified-core`: the shims re-export the new
   items automatically. Keep the per-crate check under ~2 GiB peak
   RSS; if a crate grows past that, split it the same way.
