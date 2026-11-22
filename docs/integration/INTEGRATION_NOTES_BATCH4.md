# Integration notes — Batch 4 (18 modules, migrations 000042–000059)

Coordinator-verified 2026-11-22. This file is the EXACT work list for the
integrator. Batch 4 builders did NOT touch `src/modules/mod.rs`,
`src/db/schema.rs`, `src/db/models.rs`, `README.md`, or `TODO.md` — all of
that is integrator scope. Do NOT push from this file; the parent asks the
user first.

## What batch 4 added (verified by the coordinator by reading/counting)

- 18 module files, **50,360 Rust lines**, **491 routes** (RouteInfo counts
  verified per module, see table below), **18 migrations, 103 tables**
  (every up.sql CREATE TABLE count equals its down.sql DROP TABLE count).
- Greps over all 54 batch-4 files: forbidden reference-project name **0**,
  `todo!`/`unimplemented!`/`unreachable!`/`panic!` **0**, Indonesian
  words (word-boundary, broad list) **0**. All files UTF-8, no NUL bytes.
- Every module has exactly 1 `impl Module`, 1 `pub fn register`, and
  declares `available_as_product = true` with a valid `ProductCategory`
  (verified against `ProductCategory::ALL` =
  productivity/communication/commerce/infrastructure/ai).
- None of the 18 ids is registered in `src/modules/mod.rs` yet (verified 0
  hits) — registration is item 1 below.
- Tree totals at verification time (other rounds are writing concurrently,
  so these move): 329 files, 152 `.rs` files, 299,616 Rust lines,
  68 migration directories.

| Module | File lines | Routes | Migration | Tables | Category |
|---|---|---|---|---|---|
| workflows | 3,919 | 15 | 000059_workflows | 3 | productivity |
| bi | 2,727 | 21 | 000042_bi | 3 | productivity |
| okrs | 2,000 | 22 | 000043_okrs | 5 | productivity |
| affiliates | 3,073 | 30 | 000044_affiliates | 7 | commerce |
| loyalty | 2,722 | 26 | 000045_loyalty | 5 | commerce |
| portals | 2,123 | 19 | 000046_portals | 3 | productivity |
| forms | 2,677 | 20 | 000047_forms | 4 | productivity |
| contracts | 2,993 | 27 | 000048_contracts | 4 | productivity |
| procurement | 2,963 | 31 | 000049_procurement | 6 | productivity |
| assets | 2,678 | 28 | 000050_assets | 6 | productivity |
| recruitment | 3,000 | 32 | 000051_recruitment | 6 | productivity |
| vault | 2,265 | 27 | 000052_vault | 6 | productivity |
| incidents | 3,197 | 36 | 000053_incidents | 10 | infrastructure |
| inbox | 2,004 | 18 | 000054_inbox | 4 | communication |
| surveys | 2,889 | 22 | 000055_surveys | 5 | communication |
| community | 3,018 | 38 | 000056_community | 9 | communication |
| compliance | 3,152 | 41 | 000057_compliance | 10 | communication |
| gamification | 2,960 | 38 | 000058_gamification | 7 | communication |

## 1. Register the modules (`src/modules/mod.rs`)

Insert one `pub mod` line per module, keeping the file's alphabetical order,
and one `register` call per module where the other modules register (follow
the existing pattern, e.g. `events::register(registry);`):

```rust
pub mod affiliates;
pub mod assets;
pub mod bi;
pub mod community;
pub mod compliance;
pub mod contracts;
pub mod forms;
pub mod gamification;
pub mod incidents;
pub mod inbox;
pub mod loyalty;
pub mod okrs;
pub mod portals;
pub mod procurement;
pub mod recruitment;
pub mod surveys;
pub mod vault;
pub mod workflows;
```

Then run `ModuleRegistry::validate_product_declarations` (or its equivalent
check) — all 18 declare a valid category, so it must pass.

## 2. Schema / models (`src/db/schema.rs`, `src/db/models.rs`)

Add the tables from each batch-4 migration, following the exact pattern the
000030_ai tables use in those files (same column types, same index style).
Table list per migration:

- 000042_bi: `bi_reports`, `bi_report_schedules`, `bi_kpi_dashboards`
- 000043_okrs: `okr_cycles`, `okr_objectives`, `okr_key_results`,
  `okr_checkins`, `okr_links`
- 000044_affiliates: `affiliates`, `affiliate_commission_rules`,
  `affiliate_links`, `affiliate_clicks`, `affiliate_conversions`,
  `affiliate_commissions`, `affiliate_payouts`
- 000045_loyalty: `loyalty_programs`, `loyalty_earn_rules`, `loyalty_tiers`,
  `loyalty_ledger`, `loyalty_redemptions`
- 000046_portals: `portals`, `portal_menus`, `portal_views`
- 000047_forms: `forms`, `form_fields`, `form_submissions`,
  `form_submission_values`
- 000048_contracts: `contract_templates`, `contracts`, `contract_signers`,
  `contract_audit_trail`
- 000049_procurement: `vendors`, `purchase_orders`, `purchase_order_lines`,
  `purchase_order_approvals`, `goods_receipts`, `procurement_budgets`
- 000050_assets: `assets`, `asset_assignments`, `asset_stock`,
  `asset_stock_adjustments`, `asset_stock_takes`, `asset_stock_take_lines`
- 000051_recruitment: `jobs`, `applicants`, `applicant_stage_history`,
  `interviews`, `evaluations`, `offers`
- 000052_vault: `vault_folders`, `vault_documents`,
  `vault_document_versions`, `vault_access_log`, `vault_share_links`,
  `vault_access_requests`
- 000053_incidents: `incidents`, `incident_timeline`, `on_call_rotations`,
  `on_call_schedules`, `escalation_policies`, `incident_escalations`,
  `page_attempts`, `incident_postmortems`, `postmortem_action_items`,
  `auto_open_rules`
- 000054_inbox: `inbox_threads`, `inbox_assignments`,
  `inbox_canned_replies`, `inbox_thread_reads`
- 000055_surveys: `surveys`, `survey_questions`, `survey_distributions`,
  `survey_responses`, `survey_answers`
- 000056_community: `community_categories`, `community_topics`,
  `community_posts`, `community_post_edits`, `community_votes`,
  `community_flags`, `community_moderation_actions`, `community_members`,
  `community_announcements`
- 000057_compliance: `compliance_export_requests`,
  `compliance_deletion_requests`, `compliance_deletion_items`,
  `compliance_consents`, `compliance_retention_policies`,
  `compliance_retention_reviews`, `compliance_processing_records`,
  `compliance_checklists`, `compliance_breach_log`,
  `compliance_breach_timeline`
- 000058_gamification: `gamification_rules`, `gamification_points_ledger`,
  `gamification_badges`, `gamification_awards`, `gamification_seasons`,
  `gamification_rewards`, `gamification_reward_requests`
- 000059_workflows: `workflows`, `workflow_runs`, `workflow_run_steps`

Money columns are NUMERIC(19,4); ids are UUID PKs; timestamps TIMESTAMPTZ.
The in-memory DashMap stores stay authoritative until the migrations have
been executed against a live PostgreSQL (never done anywhere yet).

## 3. Migration order

Batch-4 migrations run in this order (numbers are unique in the tree now):
000042_bi, 000043_okrs, 000044_affiliates, 000045_loyalty, 000046_portals,
000047_forms, 000048_contracts, 000049_procurement, 000050_assets,
000051_recruitment, 000052_vault, 000053_incidents, 000054_inbox,
000055_surveys, 000056_community, 000057_compliance,
000058_gamification, 000059_workflows.

RESOLVED COLLISION (coordinator did this, verified): batch 3 and batch 4
both created a `000041_*` directory. `000041` on disk is now solely
`000041_pricing_marketplace` (batch 3). The workflows migration was moved
to `000059_workflows` and both in-file references updated; verified 0
remaining `000041` references in the workflows files. NOTE: 000059 was the
start of batch 5's assigned range — batch 5 on disk starts at
`000060_properties`, so there is no conflict today, but confirm 000059 is
not double-claimed when batch 5 lands. OUT OF SEQUENCE (not mine):
`000100_calculator` (batch 3) still needs renumbering by the integrator.

## 4. Scheduler `KNOWN_KINDS` — add these kinds + arm dispatch

Each module records `scheduler_status: "declined"` honestly until its kind
is added; the dispatch arm must emit the `*_requested` event named:

| Kind | Dispatch emits | Consumer |
|---|---|---|
| `workflows.run` | `workflows.run_requested` `{"workflow_id", "trigger": "schedule"}` | workflows.rs (already listens; re-exports `WORKFLOWS_RUN_JOB_KIND`) |
| `bi.report.deliver` | `bi.report.deliver_requested` | bi.rs |
| `contracts.expiry_alert` | `contracts.expiry_alert_requested` `{"contract_id", "days_before"}` | contracts.rs |
| `assets.warranty.check` | `assets.warranty.check_requested` | assets.rs |
| `recruitment.interview.reminder` | `recruitment.interview.reminder_requested` | recruitment.rs |
| `inbox.unsnooze` | `inbox.unsnooze_requested` | inbox.rs |
| `surveys.send` | `surveys.send_requested` | surveys.rs |
| `compliance.retention.review` | `compliance.retention.review_requested` | compliance.rs |
| `incidents.escalations.run` | (call `POST /api/v1/incidents/escalations/run` on a schedule, e.g. every minute, with a manage-capable caller) | incidents.rs (no background timer in-module) |

## 5. Hook listeners to add on the OTHER side (batch-4 modules emit, nobody answers yet)

- `task.create_requested` — add a listener in `tasks.rs`. Needed by:
  workflows (create_task action), surveys (low-score follow-ups),
  incidents postmortem action items (`tasks.create.requested`).
- `tasks.update_requested` / `tickets.update_requested` /
  `tasks.assign_requested` / `tickets.assign_requested` /
  `tasks.tag_requested` / `tickets.tag_requested` — tasks.rs / tickets.rs
  (workflows update_record/assign/add_tag actions; steps record `skipped`
  until these exist).
- `loyalty.discount.requested` — store module (apply the discount).
- `affiliates.payout.requested` — billing module (pay the commission).
- `forms.mapping.requested` — CRM / tickets / tasks (answer
  `{"created": true, "id": "..."}`; until then mappings record `failed`).
- `portals.domain.requested` — domains module (move `domain_status` past
  `requested`).
- `compliance.deletion.item.execute_requested` — each owning module
  (execute one deletion item, then emit `compliance.deletion.item.executed`).
- `calculator.depreciation_requested` — calculator module (it currently
  answers only `calculator.rate_requested`; assets records `pending`).
- `hr.onboarding.started` — HR module (batch 3 owns the consumer side).
- `{source}.data.requested` — billing/store/tickets/licensing/crm/projects/
  timetracker/subscriptions (BI mirror is the live data path until then).
- `mail.send_requested` — mail module (workflows send_email action;
  verify the event name against `mail.rs` before wiring).

Also add `okrs.kr.override` to `AUDITED_EVENTS` in `audit.rs` so manual
OKR overrides land in the audit log (the override check-in row is stored
regardless).

## 6. API Manager — public-access rules (`ApiAccessRule`, default deny)

Create `public_access: true` rules for these public routes, or they stay
denied (correct default — do not open them silently):

- `POST /api/v1/affiliates/track`
- `GET /api/v1/portals/public/{slug}`
- `GET /api/v1/forms/public/{slug}`, `POST /api/v1/forms/public/{slug}/submit`
- `GET /api/v1/recruitment/public/jobs`, `GET /api/v1/recruitment/public/jobs/{id}`,
  `POST /api/v1/recruitment/public/jobs/{id}/apply`
- `GET /api/v1/community/public/topics`, `GET /api/v1/community/public/topics/{id}`
- `GET /api/v1/surveys/public/{token}`, `POST /api/v1/surveys/public/{token}`
- `GET /api/v1/vault/shared/{token}`
- `POST /api/v1/workflows/hooks/{token}`
- `GET /api/v1/events/public/*` (batch 2, if not already done)

## 7. Role capabilities to add (36)

`<module>.view` + `<module>.manage` for: workflows, bi, okrs, affiliates,
loyalty, portals, forms, contracts, procurement, assets, recruitment,
vault, incidents, inbox, surveys, community, compliance, gamification.

## 8. README / TODO lines for the integrator

- README: add the 18 modules to the module list with one line each
  (name + one-sentence description from the table in this file's header),
  and update the counts (modules, migrations, tables, routes).
- TODO: append a "Batch 4" section recording: 18 modules, 50,360 lines,
  491 routes, 103 tables, migrations 000042–000059, the 000041 collision
  resolution, and the OPEN list from section 9.

## 9. OPEN — honestly not built / not verified (do not close these silently)

1. `cargo check` / `cargo build` NEVER ran anywhere (no toolchain in this
   VM, crates.io blocked). Compilation is unverified for all 18 modules.
   Riskiest unverified surfaces: the Dispatcher/hook machinery in the new
   modules, Diesel `PgNumeric` handling, the `regex` crate use in forms.rs
   (pattern compile at definition time), and the hand-rolled iCalendar
   output in events (not batch 4, noted for context).
2. Migrations never executed against a live PostgreSQL (persistence
   targets only; DashMap stores authoritative).
3. All items in sections 4–7 above (scheduler kinds, listeners, access
   rules, capabilities) are pending integrator work.
4. Workflows webhook SSRF check has a TOCTOU rebinding window (DNS checked
   once before connect; the client re-resolves). The per-workflow host
   allowlist is the primary defence; a resolving client would close it.
5. BI forecasts return 422 insufficient-data when source data is missing
   (by design — never fabricated). OKR project links are alignment-only:
   numeric roll-up counts linked tasks, not project totals.
6. Contracts e-sign is an in-app signing flow with a full audit trail,
   NOT a certified third-party e-signature provider (no eIDAS/QES claim).
7. Inbox has no `mail.received` event source and no social module exists:
   email threads arrive via `mail.sent` (defensive) or manual
   registration; social threads via manual registration only.
8. Portal projection only covers records announced on the hook bus after
   startup; pre-existing billing/ticket/store/license records are not
   visible until re-emitted.
9. Vault: file bytes, encryption at rest, and virus scanning belong to the
   storage module; vault stores references/checksums only.
10. Escalation timing in incidents only moves when
    `POST /escalations/run` is called (see section 4).
11. Staff gamification points and customer loyalty points are separate
    ledgers by design and must never be mixed.
12. Environment note: shell writes to existing files hit EPERM (Seccomp)
    intermittently in this sandbox (the coordinator's `sed -i` on
    workflows.rs failed with "Operation not permitted"; file-tool edits
    worked). The PM coordinator reported the same. Verify at push time
    that manifest generation still works.
