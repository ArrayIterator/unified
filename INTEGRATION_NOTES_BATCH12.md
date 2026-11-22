# INTEGRATION NOTES — BATCH 12 (Invoicing Print Engine + Remaining Odoo Business Modules)

Coordinator-verified by read-back. Nothing pushed. Remote head untouched (`7cf6853c`).

Batch 12 totals (verified on disk): **6 new module files, 1 extended file
(`billing.rs`), 6 migrations, 42 tables, 198 new routes, ~22,878 new Rust
lines** (printing 2,517 + manufacturing 4,431 + quality 3,486 + expenses
3,800 + approvals 3,186 + studio 3,277 = 20,697 new module lines; billing.rs
grew by ~1,817 lines of printing work on top of concurrent batch-13
school-fees work — see §8). Tree at verification: 176 `.rs` files,
417,343 Rust lines under `src/`, 99 migration dirs.

## 1. What was built

| Item | File | Lines | Routes | Migration | Tables |
|---|---|---|---|---|---|
| Print engine | `src/modules/printing.rs` (new) + `billing.rs` extension | 2,517 + ~1,817 in billing.rs | 22 + 14 in billing.rs | `000086_printing` | 5 |
| Manufacturing / MRP | `src/modules/manufacturing.rs` | 4,431 | 46 | `000081_manufacturing` | 12 |
| Quality | `src/modules/quality.rs` | 3,486 | 32 | `000082_quality` | 5 |
| Expenses | `src/modules/expenses.rs` | 3,800 | 41 | `000083_expenses` | 8 |
| Approvals | `src/modules/approvals.rs` | 3,186 | 25 | `000084_approvals` | 6 |
| Studio (customizer) | `src/modules/studio.rs` | 3,277 | 32 | `000085_studio` | 6 |

### 1a. Invoicing print engine (B1)
- Invoice templates as DATA in `billing.rs`: CRUD + paginated list,
  per-company templates + per-company default, versioning (edit archives the
  previous snapshot first, then bumps the version; restore copies an old
  version forward as a new version — history never rewritten), preview (real
  invoice via `?invoice_id=` or deterministic sample), print-ready HTML
  renderer with `@page` size rules, validated hex colours / font-family
  allowlist / column-key allowlist / logo URL rules, all user strings
  HTML-escaped.
- Paper sizes in `printing.rs`: standards as constants (ISO 216 A0–A6,
  Letter, Legal, Tabloid, thermal 58mm/80mm) — documented as the one
  sanctioned hard-coding, since they are international standards — plus
  admin-editable custom sizes (CRUD, standards immutable via API) and a
  per-company default size (A4 fallback). Same 12 rows seeded in the
  migration.
- ESC/POS thermal: byte-stream generation (init, code page, alignment, bold,
  double size, cut, drawer kick), code-page table, word-wrap + alignment text
  preview, `/thermal/preview` (base64 stream + preview, no job stored).
  Honest docs: the server EMITS the stream; the hardware connection is
  deployment-side via the printer profile (`printing.job.dispatch_requested`
  event). No driver code claims otherwise.
- Print jobs: queue/history/reprint/cancel/fail/process, statuses
  `queued/printing/done/failed`. Billing wires in via events only:
  `print_invoice` renders and calls `printing.job.requested` synchronously,
  then emits `billing.invoice.printed`; `email-pdf` sends the print-ready
  document via `mail.send_requested` as HTML body (mail has no attachment
  field — stated in code and in the response `note`); `pdf` jobs carry the
  print-ready HTML — binary PDF conversion is a deployment step, never
  claimed. Printing also consumes `school_fees.receipt.print_requested`
  (renders the receipt as a thermal-80 job); no emitter exists yet in
  `billing.rs` (batch 13 wires it).
- Security: all 36 endpoints do `X-User-Id`/`X-Role` + fail-closed
  `role.check` (`billing.template.view/manage`, `billing.print`,
  `printing.view/manage/job`).

### 1b. Manufacturing / MRP (B2)
Versioned multi-level BOMs (cycle detection, 409 naming the cycle;
`GET /boms/{id}/explode` tree + flat totals), work orders with a
settings-stored transition map (illegal transition → 409 with
`allowed` + `current_status`), work centers with load-vs-capacity schedule
(capacity enforcement on confirm opt-in, default off), production runs
(scrap > 0 requires an active admin-defined reason code; over-production
rejected 409), reservations with draw-down, cost roll-up listing unpriced
items in `gaps` with `partial: true` (never fabricated). Reports: work
orders by status, output per period, component usage, scrap rate.

### 1c. Quality (B3)
Data-driven quality points + criteria with per-criterion tolerance bands;
inspections (`requested → completed/cancelled`, per-measurement verdicts,
conditional only with written justification); alerts with fail policy
(`none`/`alert_only`/`block`) + `GET /blocks`; CAPA state machine
(`open → in_progress → verified → closed`); reports with the supplier-score
formula and all inputs in the response (never a black-box score).

### 1d. Expenses (B4)
Admin-editable categories/policies (limits, receipt thresholds, mileage/
per-diem rates); reports with checked state machine (`draft → submitted →
approved/rejected → reimbursed`); mileage/per-diem amounts computed
server-side from policy rates; receipts as storage references only;
reimbursements with retry; adjustments as separate entries (never mutate
history); stored-rates-only currency conversion (422 when no stored rate
exists — never guessed); analytics (spend by category/employee/period,
pending approvals, backlog, violations). No second approval engine inside.

### 1e. Approvals (B5)
Generic data-driven approval workflows: one active definition per
(module, record_type); ordered steps with `all`/`any` mode; approver slots
(user/role/manager via hooks, never duplicated); date-bounded delegation
with `acting_for`; append-only decisions (Postgres trigger rejects
UPDATE/DELETE on `approval_decisions`); decision rounds on resubmit; expiry
never auto-approves (escalation rules, max escalations, then expiry).
Hook contract (exact payloads in the module header): consumes
`approval.requested`, emits `approval.decided` exactly once per request,
answers `approval.status.query`.

### 1f. Studio (B6)
Strictly metadata-driven customizer: 7 field types; versioned field
definitions (old versions never mutated); generic key-value value store (no
new columns, no runtime schema changes, no code generation, no user-code
execution — stated as a hard boundary in the module docs); list views
(private/shared, per-scope defaults); form layouts (can never hide a
required/system field); data-driven validation rules with regex compiled at
definition time; hook contract `studio.fields/layout/views.requested` +
`studio.record_type.register` and change events for cache invalidation.

## 2. Modularity-rule check (standing, MUTLAK)

All six builders disclosed their hard-coded spots; none are behaviour
lists. The only sanctioned hard-coding is the PAPER-SIZE RULE (see §1a):
standard paper sizes are international standards. Everything else is data
or admin-editable settings. Disclosed constants, by module:
- printing: standard paper-size constants, ESC/POS command bytes (published
  printer standard), ESC/POS code-page table, closed vocabularies (output
  kinds, job statuses, connection types, code pages, template column keys,
  permission names, event names), numeric validation bounds.
- manufacturing: status vocabularies as literal strings + SQL CHECKs (BOM
  `active/draft/archived`, work-center `active/inactive`, reservation/run
  statuses); seed defaults (5 scrap reasons, 8 units, transition map,
  capacity/currency/depth defaults) are starting values only; hard ceilings
  (`MAX_BOM_LINES` 500, `MAX_ROUTING_STEPS` 200, depth cap 50) are safety
  bounds.
- quality: status/result vocabulary constants; default supplier-score scopes
  used only when no `scope=` filter is given; numeric caps are validation
  bounds.
- expenses: report status vocabulary + transition table (the state machine);
  capability names, violation flag strings, input abuse guards; settings
  defaults are configurable defaults only.
- approvals: request state-machine vocabulary + small enums (step modes
  `all`/`any`, approver kinds, decision values, escalation actions) are
  validated constants so every consumer of `approval.decided` can rely on
  them; default numbers (72h timeout etc.) are marked configurable defaults.
- studio: 7-type field-type vocabulary is a closed set (each type carries
  server-side validation semantics); `FIELD_KEY_PREFIX = "x_"`, 8 default
  record types, 10 default system fields are seeded configurable defaults;
  capability names, status/visibility vocabularies, length caps are
  constants.

## 3. Integrator work list

### 3a. Register the modules (`src/modules/mod.rs`)
```rust
pub mod printing;
pub mod manufacturing;
pub mod quality;
pub mod expenses;
pub mod approvals;
pub mod studio;
```
and in the registration function, following the existing form:
```rust
printing::register(registry);
manufacturing::register(registry);
quality::register(registry);
expenses::register(registry);
approvals::register(registry);
studio::register(registry);
```
Verified: `mod.rs` currently has 0 mentions of any batch-12 module (72
`pub mod` lines total on disk at verification).

### 3b. Diesel schema/models (`src/db/schema.rs`, `src/db/models.rs`)
Add `table!` blocks + Queryable/Insertable structs for all 42 tables
(CREATE = DROP verified in every migration):

- printing (5): `paper_sizes`, `invoice_templates`,
  `invoice_template_versions`, `printer_profiles`, `print_jobs`.
- manufacturing (12): `mfg_boms`, `mfg_bom_versions`,
  `mfg_bom_version_lines`, `mfg_bom_routing_steps`, `mfg_work_centers`,
  `mfg_work_orders`, `mfg_work_order_requirements`, `mfg_production_runs`,
  `mfg_reservations`, `mfg_scrap_reasons`, `mfg_units`, `mfg_stock_notices`.
- quality (5): `quality_points`, `quality_criteria`, `quality_inspections`,
  `quality_alerts`, `quality_capas`.
- expenses (8): `expense_categories`, `expense_policies`,
  `expense_reports`, `expenses`, `expense_receipts`,
  `expense_reimbursements`, `expense_adjustments`, `expense_currency_rates`.
- approvals (6): `approval_definitions`, `approval_steps`,
  `approval_requests`, `approval_decisions`, `approval_delegations`,
  `approval_escalations`. (Note: `approval_decisions` has a Postgres
  trigger rejecting UPDATE/DELETE — keep it in the migration, do not drop
  it when wiring models.)
- studio (6): `studio_record_types`, `studio_system_fields`,
  `studio_field_definitions`, `studio_field_values`, `studio_list_views`,
  `studio_form_layouts`.

### 3c. Migration order / renumbering
Batch-12 numbers are all unique on disk (verified): 000081 manufacturing,
000082 quality, 000083 expenses, 000084 approvals, 000085 studio,
000086 printing. (B1 moved from 000080 to 000086 because 000080 was taken
twice by concurrent rounds.)

Pre-existing collisions the integrator must still renumber (verified on
disk at 2026-11-22 ~12:12 WIB; 99 migration dirs total):
- `000056` twice: `000056_community` vs `000056_storage_product_byo`.
- `000070` twice: `000070_presets` vs `000070_saas_domains`.
- `000075` twice: `000075_appointments` vs `000075_digital_products`
  (batch 10's ebook work).
- `000077` twice: `000077_field_service` vs `000077_webinars`
  (batch 10's field-service pick and webinars).
- `000080` twice: `000080_backup_drill_verify_tenant` vs
  `000080_marketing` (batch 10's marketing automation).
- `000090` twice: `000090_exams` vs `000090_school_fees` (batch 13).
- `000100_calculator` still out of sequence.

### 3d. AUDITED_EVENTS additions (`src/modules/audit.rs`)
Add: `billing.invoice_template.changed`, `billing.invoice.printed`,
`printing.*` (job/template/paper/profile events), `manufacturing.*`,
`quality.inspection.completed`, `quality.alert.raised`,
`quality.capa.closed`, `expenses.*`, `approval.*` (decided/created/
resubmitted/cancelled/escalated/expired), `studio.fields.changed`,
`studio.values.changed`, `studio.views.changed`, `studio.layouts.changed`,
`studio.record_type.registered`.

### 3e. Role capabilities (roles module — all checks fail closed until defined)
- `billing.template.view/manage`, `billing.print`
- `printing.view/manage/job`
- `manufacturing.view/manage` (check the module for its exact names)
- `quality.view/manage/inspect`
- `expenses.view/submit/manage`
- `approvals.view/manage/decide`
- `studio.view/customize/admin`

### 3f. Hook convergence (the important part — names must be reconciled)
1. **Expenses ↔ Approvals (event-name bridge REQUIRED):** Expenses emits
   `expenses.approval.requested` and listens for
   `expenses.approval.decided`; the generic Approvals module consumes
   `approval.requested` and emits `approval.decided`. The integrator must
   bridge them — either a listener in Approvals that maps
   `expenses.approval.requested` into its generic flow and answers with
   `expenses.approval.decided`, or an Expenses change to emit the generic
   names. Until bridged, submitted expense reports stay `submitted` by
   design (never auto-advanced).
2. **Manufacturing ↔ Quality (completion-event name):** Manufacturing
   listens for `manufacturing.inspection.completed`; Quality emits
   `quality.inspection.completed`. Reconcile to one canonical name (either
   side may adopt the other; Quality also listens to
   `manufacturing.inspection.requested`, which Manufacturing does emit —
   that half already matches).
3. **Manufacturing stock/cost (no answerers yet):** nothing answers
   `manufacturing.stock.requested`, `manufacturing.cost.requested`
   (inventory/assets and calculator must land listeners; until then stock
   reports `unknown`, costs are all gaps, reservations stay
   `pending_confirmation` by design).
4. **Expenses ↔ Billing (reimbursement):** billing must listen for
   `expenses.reimbursement.requested` and answer with
   `expenses.reimbursement.paid` / `.failed` (or `payment.captured` /
   `failed` carrying `reimbursement_id`). Until then, approved reports stay
   `approved`.
5. **Approvals manager slots:** `user.manager.query` has no producer yet —
   HR/users must answer it; until then manager slots stay unresolved and
   need admin reassignment.
6. **Approvals sweep:** no scheduler job calls
   `POST /requests/sweep-expiry`; add a scheduler arm or expiry never runs.
7. **Printing:** `AUDITED_EVENTS` lacks the printing/billing-template
   events (§3d); billing print/reprint endpoints return an explicit 500
   ("printing module is not registered") until `printing` is registered;
   `school_fees.receipt.print_requested` has no emitter yet (batch 13
   wires it).
8. **Quality blocking is advisory:** owning modules must consult
   `GET /blocks` (or `quality.alert.raised`) before proceeding with a
   blocked record.
9. **Studio adoption:** owning modules do not yet read Studio metadata when
   rendering — each must adopt the `studio.*.requested` query hooks.
10. **Quality event contracts defined by Quality** (emitters must adopt):
    `procurement.supplier_quality_check.requested`,
    `logistics.shipment.inspection.requested`.

### 3g. Scheduler arms to add
- Approvals expiry sweep (calls `POST /requests/sweep-expiry`).

### 3h. README / TODO lines
- Module count: add 6 (printing, manufacturing, quality, expenses,
  approvals, studio). Migration count: add 6 (000081–000086). Table count:
  add 42.
- TODO.md: add a batch-12 section with the OPEN list from §5 and the
  convergence items from §3f.

## 4. Odoo gap list — considered and NOT built (with reasons)

Batch 10's picks (confirmed on disk, NOT duplicated by this batch): POS
(`pos.rs`, 000074), Appointments (`appointments.rs`, 000075), Marketing
Automation (`marketing.rs`, 000080), Field Service / Maintenance
(`000077_field_service`), E-learning (`elearning.rs`, 000076), Webinars
(`webinars.rs`, 000077), Ebook/digital products (`000075_digital_products`
— store extension).
- **Fleet Management — NOT built: already covered.** Logistics
  (`logistics.rs`) owns fleet vehicles and drivers; maintenance logs live
  with assets/maintenance. A separate fleet module would duplicate them.
- **Helpdesk — NOT built: already covered** by the tickets + departments
  modules.
- **CRM / Sales / eCommerce / Website / Inventory / Purchase / Accounting /
  HR / Project / Events / Surveys / Forum / Sign / Chat — NOT built:
  already covered** by the existing crm / store / site_builder / assets /
  procurement / billing / hr / projects / events / surveys / community /
  contracts / chat modules respectively.
- **PLM (product lifecycle) — NOT built: future candidate.** Overlaps
  software/assets + manufacturing BOMs; no builder assigned this round.
- **Manufacturing, Quality, Expenses, Approvals, Studio — BUILT this round**
  (§1b–1f).

## 5. OPEN list (explicit)

1. **`cargo check` / `cargo build` has NEVER run anywhere** (no toolchain,
   crates.io blocked) — compilation is unverified for all six modules plus
   the billing.rs extension. Riskiest surfaces: billing.rs after two
   concurrent editors (B1 printing + batch-13 school-fees); the
   `HookContext` synchronous read-back in `print_invoice`; approvals'
   trigger-bearing migration; studio's regex handling.
2. **Migrations never executed** against live PostgreSQL; in-memory DashMap
   stores authoritative everywhere; no Diesel schema/models wiring yet (§3b).
3. **Registration not wired** (§3a); until `printing` is registered, billing
   print/reprint endpoints return an explicit 500.
4. **Older billing handlers have no capability checks at all** (pre-existing
   gap observed by B1, not introduced by this batch) — the integrator must
   decide whether to add them (behaviour change).
5. **Settings are runtime-only** in manufacturing, quality, and studio
   (same pattern as POS/billing) — durable settings store is a follow-up.
6. **Manufacturing scheduling** is a deterministic sequential layout, not a
   full finite-capacity optimiser; capacity enforcement on confirm is
   opt-in (default off).
7. **Expenses CSV export** returns a plain-text body, not the JSON envelope
   (mirrors the properties export precedent; documented in the route
   description). `spend_analytics` uses two `web::Query` extractors —
   valid in actix-web, but worth a compile-time eyeball.
8. **Studio:** no value migration when a definition's type changes (old
   values stay attached to the old version by design); no drag-and-drop
   layout-builder UI (layouts are JSON metadata over HTTP);
   `user_reference` values are format-validated only (no live existence
   check against users).
9. **Approvals:** escalation notifies but does not reassign (reassign is an
   explicit admin call); manager-slot matching falls back to stored
   `slot_ref` when the manager cannot be resolved at read time.
10. **Quality:** settings in-memory only; procurement/logistics event names
    are contracts defined by Quality — emitters must adopt them (§3f.10).
11. **Printing:** mail has no attachment field, so `email-pdf` sends the
    print-ready document as HTML body (stated in code and in the response
    `note`); binary PDF conversion is a deployment step.
12. **Batch-13 overlap (not batch 12's to fix):** batch 13 is concurrently
    editing `billing.rs` (school fees, `school_fees.receipt.print_requested`
    emitter pending) and owns migrations 000087–000092 including the
    `000090` collision (`000090_exams` vs `000090_school_fees`).

## 6. Coordinator verification (by read-back, not by trusting reports)

- Line counts match every builder's report exactly: printing 2,517;
  manufacturing 4,431; quality 3,486; expenses 3,800; approvals 3,186;
  studio 3,277; billing.rs 8,211 (3,341 original + ~1,817 printing +
  ~1,763 batch-13 school-fees at B1's count — the remaining ~1,290 lines
  are further concurrent batch-13 growth after B1's verification; the
  printing section is intact: 38 hits for
  `InvoiceTemplate`/`printing.job.requested`/`billing.invoice.printed`,
  and `school_fees` appears 32 times).
- Greps over all six new module files + six migrations: **0**
  forbidden reference-project name hits, **0** `todo!`/`unimplemented!`
  macros, **0** Indonesian words (word-boundary scan, broad list).
- Route parity (`.route(` == `RouteInfo {` literals): printing 22/22,
  manufacturing 46/46, quality 32/32, expenses 41/41, approvals 25/25,
  studio 32/32 — **198 routes total**. (A naive `grep -c RouteInfo`
  reads +2 per file: the `use` import line and the `route_info` fn
  signature.)
- CREATE TABLE == DROP TABLE in all six migrations (5/5, 12/12, 5/5, 8/8,
  6/6, 6/6 = 42 tables).
- Batch-12 migration numbers 000081–000086 are all unique on disk; batch
  12 caused no new collision.
- `src/modules/mod.rs` has 0 mentions of any batch-12 module (registration
  is integrator work, as designed).
