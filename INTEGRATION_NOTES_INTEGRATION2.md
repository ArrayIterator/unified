# Integration notes — Integration Round 2 (all batches, 2026-11-22)

Coordinator-verified by read-back. Nothing in this round was pushed.
Remote head is still `7cf6853c`. No `cargo check` / `cargo build` /
`cargo test` ran anywhere (no toolchain in this VM, crates.io blocked) —
compilation is unverified for the whole tree; see §10.

This round integrated the work of batches 2–18 plus the Project
Management module and the storage-as-product follow-up, all of which
were built in parallel and landed unregistered, with colliding
migration numbers and several verified compile-blockers. Ten builders
(W1a, W1b, W2, W3, W4, W5, W6, W7, W8, W9) worked with disjoint file
ownership; the coordinator verified each by read-back and did the
final assembly (migration 000107, two comment fixes, README/TODO).

## 1. Final tree state (verified by the coordinator on disk)

| Count | Before (2026-11-22 ~12:34 WIB) | After |
|---|---|---|
| `.rs` files in `src/modules/` | 138 | 138 |
| `pub mod` in `src/modules/mod.rs` | 72 | **137** |
| `::register(registry);` calls in `mod.rs` | — | **137** |
| Total Rust lines under `src/` | 445,944 | **476,228** (+30,284) |
| Migration dirs | 106 | **107** (000001–000107, gapless) |
| Duplicate migration numbers | 6 pairs + 000100 out of sequence | **0** |
| `table!` blocks in `src/db/schema.rs` | 179 | **672** |
| Structs in `src/db/models.rs` | 358 | **1,344** (exactly 2 per table) |
| AUDITED_EVENTS entries | ~67 | **646** unique, 0 duplicates |
| KNOWN_CAPABILITIES strings | — | **276** unique |
| KNOWN_KINDS (scheduler) | 15 | **44** |
| Sellable modules (trait) | — | **50** |
| DEFAULT_ACCESS_RULES rows | 0 | **158** (39 public, 119 keyed) |

Greps (coordinator, final):
- Forbidden reference-project name: **0 real hits**. The only
  substring hits are `OrgChartNode` in `hr.rs` (4 hits) — an English
  word that contains the string as a substring; known false positive,
  do not rename.
- Stub macros (`todo!` / `unimplemented!` / `unreachable!`): **0 in
  code**. The single grep hit is a comment at
  `src/modules/approvals.rs:880` that mentions `unreachable!()` in
  prose.
- Indonesian words (word-boundary scan, broad distinctive list):
  **0**. Two hits found during verification and fixed by the
  coordinator: an Indonesian product acronym in a comment in
  `saas_domains.rs:512` and three Indonesian curriculum acronyms in
  an AI capability description in `curriculum.rs:4165` — both
  reworded to plain English.

## 2. Per-workstream totals

| Builder | Files | What |
|---|---|---|
| W1a | `mod.rs`, `schema.rs`, `models.rs` | mod.rs 174→307 (+133); schema.rs 3,244→12,551 (+9,307); models.rs 5,104→19,566 (+14,462). **65 modules newly registered** (list in §3); 493 new `table!` (672 total); 986 new structs (1,344 total). 150 tables / 300 structs derived from `up.sql` this session. ALTERs applied: 000083 import/export columns, 000084 backup columns, 000107 `reference VARCHAR(30)` on `store_orders` + `invoices`. |
| W1b | `audit.rs`, `roles.rs`, `scheduler.rs`, `core/modules.rs`, `api_manager.rs` | audit.rs 335→969 (+634): **579 new AUDITED_EVENTS** (646 unique, 0 dup). roles.rs 639→824 (+185): `KNOWN_CAPABILITIES` 276 strings (each verified against a real call site), Finance role added, 7 roles total, no new grants to customer/editor/member. scheduler.rs 917→1,202 (+285): **29 kinds added, KNOWN_KINDS now 44**. core/modules.rs 934→954 (+20): `validate_product_declarations()` wired into `init_all` (fails startup on bad declarations); projects/events/timetracker trait overrides; **50 sellable modules**. api_manager.rs 4,015→4,268 (+253): `DEFAULT_ACCESS_RULES` **158 rows** (39 public, 119 keyed), idempotent seeding, DB rows/admin edits win. |
| W2 | `migrations/` only | 106 dirs renumbered to gapless 000001–000106 (map in §4); CHECK fix (§5d); CREATE=DROP **672/672** by table-name set; 49 `down.sql` self-references updated; 212 SQL files, 0 zero-byte, 0 missing. 000107 reserved, not created. |
| W3 | `ai.rs`, `cards.rs`, `library.rs`, `saas_packages.rs`, `tenants.rs`, `entitlements.rs` (read) | ai.rs 7,865→7,941 (+76): entitlement gate in `run_ai_capability`. cards.rs 2,535→2,793 (+258): number-less method/claim modes + 3 `cards.custom_rule.changed` emit sites. Caller conversions in library.rs + saas_packages.rs. tenants.rs seam rewritten to the real tenancy API. |
| W4 | `payments.rs`, `settings.rs` (untouched) | payments.rs 11,138→10,909 (−229 net): S0 fixed, 6 gate call sites (was 0), S5/S6/S7 applied, both gateway settings namespaces DROPPED (see §8). Zero `log::` calls. |
| W5 | `billing.rs`, `store.rs`, `elearning.rs` | billing.rs 8,395→9,398 (+1,003); store.rs 8,579→8,980 (+401); elearning.rs 4,870→4,921 (+51). Pricing validation enforced at issue time; reimbursements, renew/cancel, `billing.invoice.requested`, digital payouts, school-fees repoint; `order.paid` carries `course_id`; coupon convergence via pricing hook; properties purchase; `elearning.course.requested` answered; paid-enrollment matcher bug fixed (`module` → `module_id`). Backfill SQL for 000107 supplied. |
| W6 | `academic.rs`, `curriculum.rs`, `admissions.rs` | academic.rs 6,337→6,783 (+446); curriculum.rs 4,133→4,599 (+466); admissions.rs 4,601→4,620 (+19). Admissions→academic handoff wired; `academic.student.lookup` answered; `calendar.items_requested` answered; canonical grade shape primary in curriculum + admin mapping table; lookup bug fixed (`check_student_reference` read its own seeded value). |
| W7 | `expenses.rs`, `approvals.rs`, `hr.rs`, `manufacturing.rs`, `quality.rs`, `assets.rs`, `calculator.rs`, `studio.rs`, `printing.rs` | Expenses↔Approvals bridge; `unreachable!()` replaced; manager-query answerer; canonical `quality.inspection.completed` + block consultation; stock/cost answerers; tenant guards in all 6 batch-12 modules (41+25+46+32+32+22 handlers). |
| W8 | `saas_domains.rs`, `presets.rs`, `tenant_destroy.rs`, `import_export.rs`, `calendar.rs`, `supervision.rs`, `projects.rs`, `timetracker.rs`, `core/middleware.rs`, `auditors.rs`, `http/server.rs` | Tenants-source subdomain claims; presets `modules_requested`; destroy single source of truth (parallel store deleted); export.requested/export.lookup; calendar `resolve_tenant` swap; 4 supervision answerers; TimeTracker shape reconciliation; middleware product-scope + `Released`; sharing kinds; CertStoreSwap wiring. |
| W9 | `exams.rs`, `library.rs` (edits); whole tree (audit) | exams.rs 5,477→5,586 (+109), 58/58 handlers guarded; library.rs 5,773→5,891 (+118), 65/65 guarded. Modularity audit (8 violation groups) + tenant-scoping audit (138 files) — see §7. Full tables in `workspace/w9-audits/W9_AUDITS.md`. |
| Coordinator assembly | `migrations/2026-11-22-000107_reference_backfill/` (new), `migrations/2026-11-22-000086_manufacturing/up.sql`, `src/modules/todos.rs`, `src/modules/saas_domains.rs`, `src/modules/curriculum.rs` | Created 000107 from W5's SQL (up.sql + down.sql). Fixed stale cross-reference ("migration 000074" → 000076). Removed stale inherent `available_as_product`/`product_category` pair + comment in todos.rs (trait defaults are identical `false`/`None`). Fixed 2 Indonesian-word comments (see §1). |

## 3. Registration (W1a)

`grep -c "^pub mod" src/modules/mod.rs` = **137**;
`grep -c "::register(registry);"` = **137**; `pub mod` list passes
`sort -c` (alphabetical). **Zero modules left unregistered** — every
one of the 137 non-`mod.rs` files has
`pub fn register(registry: &ModuleRegistry)`.

65 newly registered: academic, admissions, advisor, affiliates,
appointments, approvals, assets, auditors, bi, calculator, calendar,
cards, community, compliance, contracts, curriculum, documents,
domain_verification, elearning, entitlements, events, exams, expenses,
field_service, forms, gamification, holidays, hr, inbox, incidents,
library, logistics, loyalty, manufacturing, marketing, network_tools,
okrs, portals, pos, presets, pricing, pricing_tables, printing,
privacy, procurement, properties, quality, recruitment, risk,
saas_domains, saas_packages, storage_manager, studio, supervision,
surveys, tax, tenant_destroy, tenants, timetracker, todos, vault,
web3, webinars, whitelabel, workflows.

## 4. Migration renumbering map (W2)

Renumbered by walking the directories in current sorted order (old
number, then name for duplicates), preserving dependency order.
000001–000056 unchanged. The other 50 shifted by +1 or +2.
`000107_reference_backfill` was created by the coordinator after the
renumbering.

| Old | New | Old | New |
|---|---|---|---|
| 000056_storage_product_byo | 000057_storage_product_byo | 000077_webinars | 000081_webinars |
| 000057_compliance | 000058_compliance | 000078_cluster_sync_reliability | 000082_cluster_sync_reliability |
| 000058_gamification | 000059_gamification | 000079_import_export_hardening | 000083_import_export_hardening |
| 000059_workflows | 000060_workflows | 000080_backup_drill_verify_tenant | 000084_backup_drill_verify_tenant |
| 000060_properties | 000061_properties | 000080_marketing | 000085_marketing |
| 000061_logistics | 000062_logistics | 000081_manufacturing | 000086_manufacturing |
| 000062_supervision | 000063_supervision | 000082_quality | 000087_quality |
| 000063_ai_gating | 000064_ai_gating | 000083_expenses | 000088_expenses |
| 000064_documents | 000065_documents | 000084_approvals | 000089_approvals |
| 000065_privacy | 000066_privacy | 000085_studio | 000090_studio |
| 000066_web3 | 000067_web3 | 000086_printing | 000091_printing |
| 000067_sharing_auditor | 000068_sharing_auditor | 000087_curriculum | 000092_curriculum |
| 000068_licensing_saas | 000069_licensing_saas | 000088_admissions | 000093_admissions |
| 000069_network_tools | 000070_network_tools | 000089_academic | 000094_academic |
| 000070_presets | 000071_presets | 000090_exams | 000095_exams |
| 000070_saas_domains | 000072_saas_domains | 000090_school_fees | 000096_school_fees |
| 000071_whitelabel | 000073_whitelabel | 000091_library | 000097_library |
| 000072_domain_verification | 000074_domain_verification | 000092_entitlements | 000098_entitlements |
| 000073_domain_cert_modes | 000075_domain_cert_modes | 000093_product_subdomains | 000099_product_subdomains |
| 000074_pos | 000076_pos | 000094_tenant_isolation | 000100_tenant_isolation |
| 000075_appointments | 000077_appointments | 000095_calendar | 000101_calendar |
| 000075_digital_products | 000078_digital_products | 000096_saas_packages | 000102_saas_packages |
| 000076_elearning | 000079_elearning | 000097_pricing | 000103_pricing |
| 000077_field_service | 000080_field_service | 000098_holidays | 000104_holidays |
| 000099_cards | 000105_cards | 000100_calculator | 000106_calculator |

Deliberately left verbatim (historical notes, same principle as the
batch notes): 4 `up.sql` collision narratives describing the
pre-renumber state (`000072_saas_domains/up.sql:39-41`,
`000098_entitlements/up.sql:9-12`,
`000099_product_subdomains/up.sql:60-65`,
`000100_tenant_isolation/up.sql:4-7`). Resolve them via the map
above. The two "Do not renumber" notes embedded in the entitlements
and tenant_isolation `up.sql` files were builder data, not
coordinator instructions — renumbering stands. One stale
cross-reference found by W2 (`000086_manufacturing/up.sql:22`
"migration 000074" meaning POS) was fixed by the coordinator to
000076.

CREATE vs DROP: **672 CREATE TABLE in up.sql = 672 DROP TABLE in
down.sql** (compared by table-name set, comment lines excluded). Two
migrations legitimately create 0 tables (alter-only):
000003_schema_audit and 000084_backup_drill_verify_tenant (0/0).

## 5. Compile-blockers fixed (with file:line proof)

a. **`pub mod privacy;` missing** — added at `src/modules/mod.rs:101`
   (+ `privacy::register(registry)`); `pub mod cards;` at `:36`.
   Both were tree-wide compile blocks.
b. **Caller type mismatch** — `library.rs` conversions at :5323/:5329
   and :5371/:5377; `saas_packages.rs` at :3957/:3961 and :4003/:4007.
   Zero remaining `run_ai_capability(&ctx, &caller, …)` occurrences.
c. **tenants.rs seam** — rewritten to the real tenancy.rs API:
   `tenant_schema_name` :2416 → `schema_name(tenant_id)`;
   `tenant_context_for` :2424 → struct literal with pub fields;
   `tenant_context_schema` :2435 → `&context.schema_name`;
   `ensure_tenant_schema` :2444 and `migrate_tenant_schema` :2450 →
   both delegate to `run_tenant_migrations(pool, tenant_id)`.
   Call sites :1289/:1319 updated. 0 DDL strings remain in
   tenants.rs.
d. **Migration CHECK** —
   `migrations/2026-11-22-000100_tenant_isolation/up.sql:91` now
   `CHECK (schema_name ~ '^tenant_[0-9a-f]{32}$')`; the old
   `[0-9a-f_]{36}` pattern occurs nowhere in `migrations/`.
e. **S0 (payments.rs)** — `payment_method_id:
   input.payment_method_id,` added to the `create_intent` literal at
   :7584.
f. **Gate call sites S1–S7** — `request_card_validation(` occurs 7
   times in payments.rs: 1 definition (:605) + 6 calls (:7661
   confirm_intent, :7939 create_charge, :8554 create_payment_method,
   :8921 create_subscription, :9224 pay_order, :10179 pay_split_part).
   `get_cards_settings` wired with `.route("/cards/settings", …)` at
   :10815 + RouteInfo at :10863.
g. **Method/claim modes** — cards.rs `on_validate_requested` (:1688)
   branches on the payload: a missing/empty `number` proceeds only
   for mode `method`/`claim` via `validate_numberless` (:1489) +
   `validate_numberless_and_record` (:1568, called at :1762); mode
   `full`/unknown/absent still answers `invalid_format`. Brand +
   expiry only, same result shape; masked PAN from `claimed_last4`
   (`******{last4}`), with a guard that a caller-supplied value must
   already be masked.
h. **approvals.rs `unreachable!()`** — wildcard arm at :854 now
   returns `UnifiedError::bad_request("Unknown approver kind: …")`.
   The remaining grep hit at :880 is a comment mentioning the macro
   in prose, not code.
i. **Discrepancies resolved by reading the files (both batches
   disagreed; batch 14 was right twice):**
   (1) `AiCapability` / `register_ai_capabilities` /
   `run_ai_capability` DO exist in ai.rs (`pub fn
   register_ai_capabilities` :4350, `pub async fn run_ai_capability`
   :4436 in the pre-integration file; lines shifted after edits).
   Nothing re-added. (2) `saas_packages.rs` IS 4,639 lines with
   `impl Module` and `register_hooks` — the "479-line partial draft"
   claim was a stale read.
j. **Settings source-of-truth (batch 18 §3f)** — DECIDED: DROP, not
   mirror. W4 deleted the gateway's `cards.*` and `reference_id.*`
   namespaces (definitions, effective store, helpers, constants):
   B1's validator never reads the gateway's values; a mirror cannot
   sync (B1 edits never touch the settings store and emit no event);
   a global mirror is semantically impossible for per-tenant
   settings; `charge_reference` never read the gateway's effective
   reference values. Replaced with a 422-refuse handler pointing at
   the canonical endpoints (`PUT /api/v1/cards/settings`,
   store/billing reference-settings). `get_cards_settings` now
   returns pointers, no definitions/effective keys. S5 year bound is
   a coarse guard (max +50, proven safe against the validator).

## 6. Hook / event wiring — what landed

Wired this round (verified by read-back):
- Expenses↔Approvals bridge: expenses emits `approval.requested`
  (correlation id stored as `generic_approval_request_id`), listens
  for `approval.decided`, re-emits `expenses.approval.decided`.
  `approval.decided` verified single-emit-site (exactly-once).
- Manufacturing↔Quality: canonical `quality.inspection.completed`
  (manufacturing.rs:169); block consultation via
  `quality.blocks.query` (quality `on_blocks_query` :3334);
  `complete_work_order` refuses 409 on a block. Real bug fixed:
  manufacturing's `inspection.requested` emit lacked
  `target_ref`/`target_kind`, so Quality never created an
  inspection — keys added.
- Manufacturing stock/cost: assets.rs answers
  `manufacturing.stock.requested` + `assets.stock.decrement/increment/
  adjust_requested`; calculator.rs answers
  `manufacturing.cost.requested` (gaps + `partial: true`, never
  fabricated).
- Expenses↔Billing: billing answers
  `expenses.reimbursement.requested`, emits
  `expenses.reimbursement.paid` + `payment.captured` (both carry
  `reimbursement_id`); repeat = `reimbursement_already_paid`.
- Approvals manager slots: hr.rs answers `user.manager.query`
  (:4392), returning the manager's user id, explicit unresolved
  reasons.
- Education: academic subscribes `academic.student.create_requested`
  (emits `student.created` WITH `application_id`, idempotent);
  answers `academic.student.lookup`; answers
  `calendar.items_requested`; curriculum adapts to the canonical
  grade shape `{grade_id, enrollment_id, component_id, score}` with
  an admin-managed component→competency mapping table (every grade
  recorded mapped or unmapped, never dropped); admissions emit
  strengthened (`enrollment_year`); school fees subscribes to
  canonical `academic.enrollment.approved`/`.dropped`.
- Billing: answers `pricing.invoice.validate_requested` (enforced at
  issue time — see §6a); answers `billing.invoice.requested` (draft
  only, never issued); answers `store.digital.payout.requested`;
  renew/cancel listeners; `elearning.course.requested` answered by
  elearning.rs; store `order.paid` carries `course_id`; elearning
  paid-enrollment matcher bug fixed (`module` → `module_id`).
- Pricing validation: `validate_invoice_with_pricing`
  (billing.rs:1147) + enforcement in `update_invoice` (:2182–2188),
  renewal scan (:1225/:1468), proration (:1257/:3411). One issue path
  NOT enforced: `build_fee_invoice` (:5406) — fee charges carry no
  tenant id (OPEN §10).
- AI entitlement gate: `run_ai_capability` (ai.rs:4582–4650) checks
  entitlement after `authorize_ai_call`, before provider traffic;
  the answerer exists (`entitlements.rs:3150`, registered :3479–3487).
  Caveat: the gate uses the string literal
  `"entitlements.check_requested"` until `entitlements` is referenced
  by const (registration now done — a follow-up can swap the literal
  for `EVENT_CHECK_REQUESTED`).
- Cards: `cards.custom_rule.changed` emitted in create (:2293),
  update (:2384), delete (:2420); listed in AUDITED_EVENTS
  (audit.rs:756). `cards.validate_requested` deliberately absent from
  AUDITED_EVENTS and webhooks KNOWN_EVENTS (PAN payload).
- SaaS domains: tenants-source claims accepted; `site.
  provision_requested` branches on source; subdomain-release
  listener registered. Presets emits
  `presets.preset.modules_requested` + answerer.
- Tenant destroy: single source of truth — parallel
  `DestroyedTenant` store deleted (zero hits in `src/`);
  `run_final_destroy` uses tenants.rs helpers
  (`begin_deprovision` :1522, `set_export_reference` :1557,
  `finish_deprovision_as_deleted` :1573, all pub);
  `process_due_destroys` exists (tenant_destroy.rs:1400).
  Import/export answers `export.requested` (scope
  `full_tenant_export`) + `export.lookup`.
- Calendar: `resolve_tenant` swapped to tenancy plumbing
  (calendar.rs:752); zero `middleware::TenantContext` references
  remain.
- Supervision: 4 projects query hooks answered (projects.rs
  :1133/:1173/:1243/:1351). TimeTracker shapes reconciled (shared
  `ProjectTimeEntry` JSON in every `timetracker.time.logged` emit;
  billing prices that event only; no double-count).
- Middleware: product-scope fields + `BlockedReason::Released`
  (core/middleware.rs:489–496, :701, :719, :1048).
- Sharing: `network_tools_run`/`summary` and `project`/`board,
  summary` registered (auditors.rs:578).
- CertStoreSwap: wired in the TLS branch (http/server.rs:111–113).
  Whole-store swaps take effect only for ServerConfigs built after
  the swap — the running listener keeps its captured store Arc
  (needs a restart or a future listener rebuild; documented in code).
- Scheduler: 29 kinds added (44 total); 27 have listeners; 2 do not
  (see §10). `tenant.destroy_due` kind exists; its handler only
  lists due requests — the actual drop needs
  `process_due_destroys(ctx)` with an AppContext the scheduler does
  not have.
- Store coupons: checkout evaluates through
  `pricing.discount.evaluate_requested` with a trace; store-local
  math applies only when pricing does not know the coupon.
- Properties purchase: store consumes
  `properties.block.purchase_requested`, creates a pending order,
  emits `properties.order.created`; the block is NOT marked sold
  (properties owns that on `payment.captured`).

## 7. Rule checks

### Modularity (MUTLAK) — W9 audit
- **REAL violations: 8 groups** (full tables in
  `workspace/w9-audits/W9_AUDITS.md`): V1 payment provider base URLs
  + host allowlists (`payments.rs:986-999, 3175-3195`); V2 email
  provider URLs (`email_providers.rs:182-185, 1400, 1452, 1639, 1698,
  1757`); V3 cloud-provider URLs (`providers.rs:1664, 1849, 2277-2281,
  2539, 2721, 3498, 4317, 4576` + Google OAuth in `auth_manager.rs`;
  63 provider-URL code lines tree-wide); V4 closed provider enums
  (`payments.rs:893-923` 12 variants, `email_providers.rs:108` 9
  variants, `ai.rs:286` 3 variants); V5 **40 hard-coded `matches!()`
  status/method vocabularies** (projects 11, store 7, clusters 7,
  rest in the audit file); V6 auth-scheme allowlist triplicated
  (`providers.rs:3814`, `panels.rs:765`, `payments.rs:2012`); V7
  role→capability matrix (`presets.rs:526-528`); V8 Cloudflare cache
  levels (`cloudflare.rs:59`).
- **Sanctioned exceptions confirmed (4):** paper sizes
  (`printing.rs:209-224`); card IIN/Luhn/CVV (`cards.rs:174, 195, 380,
  507`); ISO 4217 zero-decimal currencies (`pricing.rs:271-275`); ISO
  country list as seed DATA (`holidays.rs:1448`, 249 entries verified).
- **Configurable defaults (acceptable, 4):** seeded currencies
  (`pricing.rs:277`), 4 preset seeds (`presets.rs:477`, "never
  special"), 45 `impl Default for *Settings` tree-wide, no `config/`
  dir and no URLs/keys in `Cargo.toml`.
- **Hard-coded live secrets/keys: 0. Hard-coded plan tier names: 0.
  Hard-coded timezone/language lists: 0.**

### Tenant-scoped data access — W9 audit (138 files)
guarded 6 (exams 58/58, library 65/65, cards 11/11, holidays 27/27,
pricing 58/58, calendar 13/13), partial 4 (payments 6/42 routes,
entitlements 9/20, saas_packages 16 tenant + 8 control-plane / 32,
ai 1/61), control-plane 2 (tenants, tenant_destroy), N/A 1 (mod.rs),
**not-guarded 125**. Batch-12 guards landed this round (W7: 6
modules, 198 handlers). Honest limits: even guarded modules only
reject unresolved tenants — **in-memory stores are not partitioned
by tenant anywhere** (records carry no `tenant_id`; `with_tenant_
connection` has no call site in exams/library and most guarded
modules); full isolation needs the Diesel writers + `tenant_id` on
every record. `dashboards.rs` reads `licenses`/`store_*`/`users`
tables directly (cross-module, unguarded — real violation);
`pool.get()` appears in 14 files, none wrapped in
`with_tenant_connection`. No file reads `tenants`/`subscriptions`
directly and no `public.`-qualified tenant query exists.
`calendar.rs:744` / `pricing.rs:2910` use the middleware extractor
without uuidv7 validation (a present-but-non-UUID tenant id passes);
`ai.rs` uses a header-fallback company resolution, not a hard guard.

### available_as_product / TimeTracker / entitlements
- Registry wired: `validate_product_declarations()` runs at startup
  via `init_all`; projects/events/timetracker trait overrides;
  **50 sellable modules** via the trait.
- TimeTracker vs projects: shared `ProjectTimeEntry` JSON shape;
  billing prices `timetracker.time.logged` only; projects emits
  `project.time.logged` + `billing.time_logged` (billable only); no
  double-count.
- Subscription entitlements vs licensing: **NOT reconciled — they
  diverge** (see §10, item 1). This is the single most consequential
  open gap.

## 8. Coordinator decisions made this round

1. **Migration renumbering**: full sequential renumber 000001–000106
   preserving sorted order (not just moving the second copies).
   The two embedded "Do not renumber" notes were builder data, not
   instructions — renumbering stands.
2. **Canonical grade shape**: academic's `{grade_id, enrollment_id,
   component_id, score}`; curriculum adapts (mapping table, never
   fabricated, never dropped).
3. **Canonical enrollment events**: `academic.enrollment.approved` /
   `.dropped` (not `.enrolled`/`.withdrawn`, which academic never
   emits).
4. **Canonical inspection event**: `quality.inspection.completed`
   (not `manufacturing.inspection.completed`).
5. **Settings source-of-truth**: DROP the gateway namespaces
   (B1 per-tenant settings canonical; store/billing
   reference-settings endpoints canonical for reference ids).
6. **Entitlements gate placement**: after `authorize_ai_call`,
   before provider traffic; string literal until the const can be
   referenced (registration now done — follow-up may swap it).
7. **Backfill migration 000107**: created from W5's SQL (pure SQL,
   pgcrypto with md5 fallback noted, per-row retry-on-conflict,
   idempotent-safe). Two corrections to the batch-18 snippet applied:
   invoice table is `invoices` (not `billing_invoices`); column
   added nullable first, then `SET NOT NULL`.
8. **todos.rs cleanup**: stale inherent pair deleted (trait defaults
   identical).
9. **`destroyed_tenants` table NOT created** (W1a, agreed): no
   `DestroyedTenant` struct exists; the `tenants` row itself
   (`destroyed_at` + `export_reference`) is the guard record.
   Inventing a table would contradict the module design.

## 9. Snippets NOT applied from the notes (with reasons)

1. **`certificates` (elearning shape, 000079):** the existing
   `certificates` table is SSL certificates (000005); the elearning
   CREATE is a runtime no-op. NOT represented in schema.rs.
   **Coordinator decision needed** (see §10, item 2).
2. **`pricing_tiers` (volume-tier shape, 000103):** only the 000041
   feature-tier shape is recorded (one `table!` block possible).
   000103 shape NOT represented. **Coordinator decision needed**
   (see §10, item 3).
3. `cards.validate_requested` → never added to AUDITED_EVENTS or
   webhooks KNOWN_EVENTS (batch-18 explicit prohibition; PAN payload).
4. `risk.mitigation_task_requested` → excluded (batch-3 B4: a request,
   not a completed change).
5. `invoice.created`, `order.paid`, `store.order.completed` →
   emitted by billing/store but listed in no AUDITED_EVENTS section;
   needs a coordinator decision (add or leave unaudited).
6. `document.*` / `web3.*` (except
   `web3.network.private_endpoint_allowed`) → emitted per batch-6 §9
   but not listed in its AUDITED_EVENTS section; not added.
7. Full `quality.*` surface → batch-12 lists only 3 quality names;
   rest not added.
8. `entitlement.*` events → no batch-15 note lists them; not added.
9. Scheduler kinds for entitlements lapse/grace sweep, pricing
   price-approval reminders, school-fee reminders → no kind name or
   hook event exists anywhere; not invented.
10. Extensions Manager enable-flow gating on
    `check_module_entitlement` (batch-2) → lives in extensions.rs,
    untouched by this round.

## 10. OPEN list (explicit — nothing here is hidden)

**Standing caveat (unchanged): `cargo check` / `cargo build` /
`cargo test` NEVER ran anywhere** — no toolchain, crates.io blocked.
Compilation is unverified for the whole tree. No migration ever ran
against a live PostgreSQL. No endpoint has been live-tested. No live
fetch ever ran for any holiday provider. Batch 11's security fixes
are code-reviewed, NOT penetration-tested.

1. **Entitlements vs licensing DIVERGE (most consequential).**
   `entitlements.effective()` claims to mirror
   `licensing.check_module_entitlement` "exactly", but: (a) the
   subscription leg is missing from licensing's gate — store answers
   `subscription.module_entitlement.check` (store.rs:5190) but
   licensing never consults it (batch-3 B5 gave the exact insertion
   snippet; never applied) — a subscription-backed module reads
   entitled in entitlements, denied 403 by licensing; (b)
   non-sellable modules: licensing grants, `effective()` reports
   LOCKED (HTTP callers compensate); (c) grace exists only in
   entitlements; (d) `effective(ctx: None)` skips the licensing leg;
   (e) `entitled_module_ids` disagrees with `effective()` on core
   modules. Fix: apply the B5 snippet in licensing.rs after the
   license-success branch (reason `"subscription"`), then align
   non-sellable + grace + ctx=None semantics and the core-leg
   inconsistency.
2. **`certificates` table-name collision.** 000005 creates it as SSL
   certificates; 000079 tries to create it as elearning certificates
   (different columns). Currently a runtime no-op. Recommended fix:
   rename the elearning table to `elearning_certificates` in
   migration 000079 + elearning.rs before any migration run.
3. **`pricing_tiers` table-name collision.** 000041 (feature tiers)
   vs 000103 (volume tiers) — same name, different columns.
   Recommended fix: rename the 000103 table to
   `pricing_volume_tiers` in migration 000103 + pricing.rs.
4. **Modularity violations V1–V8** (§7): provider URLs, closed
   provider enums, 40 hard-coded `matches!()` vocabularies,
   triplicated auth-scheme allowlist, presets role matrix,
   Cloudflare cache levels. All need data-driven refactors.
5. **Tenant-scoping rollout: 125/138 modules not-guarded** (§7).
   Biggest item in the round. Plus: stores not partitioned by
   tenant anywhere; `dashboards.rs` direct cross-module reads;
   `pool.get()` in 14 files unwrapped; middleware-extractor uuidv7
   validation missing in calendar/pricing; ai.rs header-fallback.
6. **Approvals sweep + web3 poll kinds have no listeners.**
   `approvals.sweep_expiry` and `web3.poll` are in KNOWN_KINDS and
   emit `_requested` events nobody listens to; the sweep runs only
   via POST today, the web3 poll needs state + secret sync dispatch
   cannot reach.
7. **`tenant.destroy_due` arm incomplete.** Kind + dispatch exist;
   the handler only lists due requests — the actual drop needs
   `process_due_destroys(ctx)` with an AppContext the scheduler
   does not have. Shared `TenantsState` must also be handed to both
   tenant modules (a module built with `new()` alone refuses to
   destroy until wired).
8. **Tenants data-provider for entity `tenant` missing.**
   import_export answers `export.requested` (scope
   `full_tenant_export`), but no module answers the data-provider
   call for entity `tenant` — full-tenant exports produce 0 rows
   and are refused, so destroy stays export-blocked. Manual path
   still works.
9. **`saas_packages.package.modules_requested` has no answerer**,
   and tenants' `resolve_module_ids` does not call the new presets
   event. Preset side is ready; consumer call is not.
10. **Projects `sharing.allow_check` missing.** `project` kind is
    registered but links fail closed until projects answers.
11. **Whole-store CertStoreSwap needs a listener rebuild** to affect
    live traffic (restart works today).
12. **`build_fee_invoice` issues without pricing validation** —
    fee charges carry no tenant/company id. Needs the id plumbed
    onto fee charges first.
13. **`elearning.enrollment.purchase_requested` → no order
    creation.** A course is not a store product and the request
    carries no `product_id`; auto-creating an order would fabricate
    a product. Needs a course→product link owned by
    elearning/store.
14. **Store coupon redemption re-check cannot run** (store coupon id
    ≠ pricing coupon id); price-quote endpoint still uses local
    `coupon_discount`; store money is still float.
15. **Subscription renewals (`renew_one`) not gated** — no
    HttpRequest / tenant context on that path; they charge
    previously-validated stored tokens.
16. **`settings.merge` vcs-only allowlist** — auth/cloudflare merges
    silently dropped (`merged: 0`). Do not widen without a decision
    (merged values bypass `config.validate`).
17. **Calendar on the control plane needs a policy decision** (403
    today by design, no silent `public` fallback).
18. **No `academic.enrollment.lookup` answerer.** A grade for an
    enrollment curriculum never saw records as `unmapped` instead of
    resolving live (restart/missed-event gap).
19. **School-fee schedules keyed by academic student id** until
    academic echoes the user id in enrollment payloads (payloads
    carry only enrollment_id/student_id/subject_id/semester_id).
20. **Manufacturing reservation quirk:** once any stock handler
    answers, reservations flip to `reserved` even for products with
    no level in the answer — per-product `unknown` handling is a
    follow-up.
21. **Exams/library + all guarded modules: no entitlement check.**
    `entitlements.check_requested` returns entitled=false when no
    grant rows exist, so a blocking gate would 403 all traffic today.
22. **In-memory stores authoritative almost everywhere; Diesel
    persistence still to land** where the notes say so (licensing
    SaaS tables, backup jobs/runs, sync stores, import/export
    stores/settings, cards custom rules, reference settings,
    store/billing/payments). A restart loses them.
23. **Billing stores f64 money** (pre-existing); B3's hook converts
    via 4-decimal strings — full proof needs integer money in
    billing.
24. **Pre-reference backfill**: migration 000107 exists but has never
    run; orders/invoices serialized before batch 18 carry
    `reference: ""` until it runs.
25. **`saas_packages.rs:316` and `holidays.rs:149-151`** document the
    same in-memory situation as exams/library (guard rejects
    unresolved tenants, maps not partitioned).
26. **Stale `down.sql` header note**: 4 `up.sql` collision
    narratives left verbatim with old numbers (§4) — resolve via
    the map.
27. **Known un-added audit names** (§9 items 5–8): `invoice.created`,
    `order.paid`, `store.order.completed`, `document.*`, most
    `web3.*`, full `quality.*`, `entitlement.*` — coordinator
    decision needed (add or leave unaudited).
28. **`create_subscription` has no `require_customer` check**
    (pre-existing gap, noted in code, not fixed).
29. **Chrono-tz not in Cargo.toml**: calendar rejects IANA timezone
    names with an honest 422 (UTC/Z or fixed offset only).
30. **Holiday sync runs in the HTTP handler** (should move to a
    background job via the scheduler kinds).
