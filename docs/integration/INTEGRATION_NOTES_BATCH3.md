# Integration notes — Batch 3 (coordinator-merged)

Batch 3 delivered 6 new modules (Calculator, HR, Tax, Advisor, Risk,
Pricing Tables + Marketplace), subscription selling, and 6 migrations.
Nothing in this batch is wired into the binary yet: the builders did not
edit `src/modules/mod.rs`, `src/db/schema.rs`, `src/db/models.rs`,
`README.md`, or `TODO.md` (the PM coordinator owned those files for its
own round; batch 3 leaves shared files to the integrator).

Per-builder verbatim snippets live in the six files
`INTEGRATION_NOTES_BATCH3_B1.md` … `INTEGRATION_NOTES_BATCH3_B6.md`
(3,731 lines total). This file is the checklist that ties them together.
Read the per-builder files for the exact `table!` blocks, model structs,
and README/TODO lines — do not re-derive them from memory.

## 1. What landed (verified by the coordinator by reading the files back)

| Builder | Files | Lines | Routes | Migration |
|---|---|---|---|---|
| B1 Calculator Enterprise (`calculator`, submodule of Company Management, `requires: ["company"]`, not sellable) | `src/modules/calculator.rs` | 2,322 | 28 | `2026-11-22-000100_calculator` (4 tables) — see §2 |
| B2 HR (`hr`, `requires: users, departments, notifications, audit`, not sellable) | `src/modules/hr.rs` | 4,648 | 62 | `2026-11-22-000037_hr` (14 tables) |
| B3 Tax (`tax`, `requires: billing, settings, notifications, scheduler, audit`, not sellable) | `src/modules/tax.rs` | 3,853 | 26 | `2026-11-22-000038_tax` (6 tables) |
| B4 Advisor (`advisor`, `requires: billing, licensing, crm, notifications, audit`, not sellable) + Risk (`risk`, `requires: notifications, audit, projects`, not sellable) | `src/modules/advisor.rs`, `src/modules/risk.rs` | 1,659 + 2,170 | 8 + 30 | `2026-11-22-000039_advisor_risk` (10 tables, shared) |
| B5 Subscription selling (extends products/store/billing) | `src/modules/products.rs` +237, `src/modules/store.rs` +1,170, `src/modules/billing.rs` +1,128 | 2,535 | 6 new (4 store, 1 billing scan, 1 products plan) | `2026-11-22-000040_subscriptions` (3 tables) |
| B6 Pricing Tables + Marketplace (`pricing_tables`, `requires: products, store, billing, site_builder, notifications`, sellable, category `commerce`) | `src/modules/pricing_tables.rs` + `src/modules/site_builder.rs` +49 | 3,785 + 49 | 49 | `2026-11-22-000041_pricing_marketplace` (8 tables) |

Totals for this batch: ~21,021 new Rust lines, ~209 new routes,
45 new tables, 6 migrations. Whole-tree counts at merge time (other
batches were writing concurrently, so these are tree totals, not batch
deltas): 329 files, 152 `.rs` files, 299,616 Rust lines.

## 2. Migration renumber (already done by the coordinator)

- `000036_calculator` COLLIDED with batch 2's `000036_ai_allowlists`.
  The coordinator renamed the batch-3 directory to
  `2026-11-22-000100_calculator` and updated all 8 in-file references
  (module header, `down.sql`, B1 notes). `000036` now belongs solely to
  batch 2. The integrator must still renumber `000100` into the final
  sequence if it wants a gapless series — the number is out of sequence
  on purpose (it sits above every number any parallel batch was using).
- B5's note that "migration 000041 exists twice" was NOT reproduced:
  only `2026-11-22-000041_pricing_marketplace` exists. Treat as stale.
- Batch-3 migration numbers are otherwise unique and gapless
  000037–000041. `000040_subscriptions` exists exactly once.

## 3. Shared-file wiring checklist (integrator)

1. `src/modules/mod.rs`: add `pub mod calculator;`, `pub mod hr;`,
   `pub mod tax;`, `pub mod advisor;`, `pub mod risk;`,
   `pub mod pricing_tables;` and the six `register` calls. Exact
   placement is in each B-note (§1 of each file).
2. `src/db/schema.rs` / `src/db/models.rs`: add the `table!` blocks and
   model structs from each B-note. Money is `NUMERIC(19,4)` /
   `PgNumeric` everywhere except the calculator exchange-rate ratio
   (DOUBLE, justified in the migration) and the AI-style cost estimates
   (none in this batch).
3. `AUDITED_EVENTS` in `src/modules/audit.rs`: add the tax names (12,
   B3 notes §6), the advisor/risk names (17, B4 notes), and the `hr.*`
   names (7, B2 notes). Audit only records listed events — without this
   the modules emit events the audit log never records. (Tax also keeps
   its own `tax_audit` table; advisor/risk rely on the audit module.)
4. Scheduler arms: add a kind that emits `billing.subscriptions.scan`
   (B5 notes §4). No scheduler kind can emit
   `advisor.evaluate_requested` (scheduler `KNOWN_KINDS` is fixed) —
   advisor evaluation currently runs on demand or via the hook.
5. Licensing hook: insert the `subscription.module_entitlement.check`
   call into `check_module_entitlement` (exact snippet in B5 notes §5,
   after the license check, before deny, `reason: "subscription"`,
   fail-closed). Store answers the check; licensing does not duplicate
   the logic.
6. Roles: no default role holds any `hr:*` capability (B2 notes §6d–6e).
   Until an admin grants them, only the admin role can use HR
   write/payroll endpoints and employees need `hr:read` for
   self-service leave. Decide the default grants at integration time.
7. README.md / TODO.md: apply the lines from each B-note. B3's notes
   assume tax lands as README item 72 / 73 total modules — renumber if
   several batches land together. The PM coordinator already updated
   README counts for its own round; recount at integration time.

## 4. Event-name corrections (verified against source by the coordinator)

- Billing emits `invoice.created` (billing.rs:1011, 1071) — NOT
  `billing.invoice_created`. The Store emits `order.paid`
  (store.rs:1580, 4338) and `store.order.completed` (store.rs:4341,
  4471) — NOT `store.order_paid`. Tax (B3) and the subscription code
  (B5) use the real names; tax additionally subscribes to the alias
  spellings defensively. Do not "fix" these back to the briefed names.
- `audit.record_requested` does NOT exist anywhere. Tax emits
  audit-shaped `tax.*` events + its own table (see §3.3).
- `storage.file_requested` has NO handler in the tree yet (B2 notes
  §6c): HR document creation stores metadata when nothing answers and
  fails only on an explicit `{"exists": false}` answer.

## 5. Convergence points (integrator must reconcile)

- **TimeTracker event name**: the TimeTracker module emits
  `timetracker.time.logged` (timetracker.rs:258) with payload
  `{entry_id, user_id, started_at, ended_at, duration_secs, hours,
  source}`. The HR brief documented `timetracker.entry_logged` with
  `{user_id, date, minutes, source}`. HR subscribes to BOTH names and
  accepts both payloads, so it works either way — pick one canonical
  name/payload at integration time (B2 notes §6a).
- **Risk → Projects**: no handler exists yet for
  `risk.mitigation_task_requested` (projects registers only
  `project.task_from_ticket_requested`,
  `project.ticket_from_task_requested`,
  `project.billable_hours_requested`). Mitigation tasks stay `requested`
  with `awaiting_projects_handler: true` until the integrator wires it
  (B4 notes §7; exact request payload documented there).
- **Subscription renewal vs payments**: B5 deliberately does NOT use
  scheduler kind `payment.subscription.renew` (its `subscription_id`
  is the payments module's gateway id, not the store subscription id).
  The payments module needs a `subscription.renewal_due` listener
  (B5 notes §6).
- **Department validation in HR is advisory**: `department.membership_check`
  answers which departments a user agents for, not the department they
  belong to; HR stores a `department_verified` flag instead of
  rejecting (B2 notes §6b).
- **products.rs baseline**: B5 started from 4,925 lines, not the 4,806
  the batch-3 brief measured — batch 2 had added the
  `list_module_offerings` endpoint (`GET /api/v1/products/module-offerings`)
  in between. B5 re-read before editing and did not clobber it
  (verified present at products.rs:2787, 5080).

## 6. Coordinator verification (read-back, not compilation)

- Line counts of all 6 builders match their reports exactly, except
  advisor.rs: 1,659 on disk vs 1,653 reported (a final fix applied
  after B4 counted — the disk count governs).
- Stub macros (`todo!` / `unimplemented!`): 0 across all batch-3 files
  (including the three files B5 edited).
- Forbidden reference-project name: 0 real hits. The only grep hits are
  the identifier `OrgChartNode` in hr.rs (4 occurrences — the substring
  "tnode" inside "ChartNode"), a false positive.
- Indonesian words (word-boundary scan): 0 in all batch-3 `.rs` files.
- Delimiter balance: 0 difference on every batch-3 file, including the
  four files edited in place (products/store/billing/site_builder).
- B1, B3: each recovered from a shell-write EPERM that truncated its
  module file to 0 bytes mid-edit; both rewrote via the file tool and
  re-verified. Both files are intact on disk (calculator.rs 2,322
  lines, tax.rs 3,853 lines, delimiters balanced).
- B6's `store.order.completed` claim was doubted, then confirmed in
  source (store.rs:4341, 4471).

## 7. OPEN (unfinished / unverified — carried from the builders)

1. **`cargo check` / `cargo build` NEVER ran anywhere** — no Rust
   toolchain exists in this VM and crates.io is blocked. Compilation,
   `cargo fmt`, clippy, live HTTP behaviour, and live Diesel migration
   runs are all unverified for every item in this batch.
2. Integrator items in §3 above (registration, schema/models,
   `AUDITED_EVENTS`, scheduler arms, licensing hook, HR capabilities,
   README/TODO recount).
3. Convergence points in §5 above.
4. Tax minor limits: `PUT /rules/{id}` cannot clear `threshold_amount`
   or `effective_to` back to null; `uq_tax_filings_period` does not by
   itself forbid duplicate country-wide (NULL region) filings
   (enforced in code).
5. Pricing-table rendering is structured JSON only (no HTML); the
   exactly-one-highlighted-tier rule is enforced in code, not SQL;
   paid-commission settlement on mark-paid settles whole commissions
   only (a single pending commission larger than the remaining payout
   amount stops settlement, remainder stays pending).
6. Persistence writers for the three subscription tables do not exist
   yet (tables exist, nothing inserts — the DashMap store is
   authoritative until the integrator adds the writer).
7. Advisor rules that depend on data no module emits (CRM
   last-activity for churn, seat usage for underused subscriptions)
   evaluate only snapshots supplied in the re-evaluate payload and
   report the gap in `data_gaps`.
