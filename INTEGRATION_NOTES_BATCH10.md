# INTEGRATION NOTES — Batch 10 (Webinars, E-Learning, Digital Products, POS, Appointments, Marketing Automation, Field Service)

Coordinator-verified by read-back. Nothing pushed. Remote head still `7cf6853c`.

## 1. What landed (all verified by the coordinator, not taken on trust)

| Module | File | Lines | Routes | Migration | Tables |
|---|---|---|---|---|---|
| webinars | `src/modules/webinars.rs` (new) | 4,724 | 44 | `000077_webinars` | 7 |
| elearning | `src/modules/elearning.rs` (new) | 4,660 | 57 | `000076_elearning` | 12 |
| digital products | `src/modules/store.rs` (+2,921; 5,504 → 8,425) | +2,921 | +32 | `000075_digital_products` | 8 |
| pos | `src/modules/pos.rs` (new) | 3,789 | 35 | `000074_pos` | 7 |
| appointments | `src/modules/appointments.rs` (new) | 4,087 | 38 | `000075_appointments` | 5 |
| marketing | `src/modules/marketing.rs` (new) | 5,603 | 54 | `000080_marketing` | 10 |
| field_service | `src/modules/field_service.rs` (new) | 5,486 | 58 | `000077_field_service` | 7 |

**Batch totals: 31,270 new Rust lines, 318 new routes, 56 new tables, 7 migrations.**
Tree at verification time: `src/modules/` = 128 `.rs` files, 359,525 lines; 93 migration dirs
(batches 11 and 12 are landing concurrently — recount at push time).

Coordinator verification (all green):
- Line counts match every builder report exactly (including store.rs 8,425).
- Forbidden reference-project name: **0** in all batch-10 files (module + SQL).
- `todo!` / `unimplemented!`: **0** in all batch-10 files.
- Indonesian words (tight Indonesian-only word list, word-boundary): **0**.
- CREATE TABLE count = DROP TABLE count in every batch-10 migration.
- `src/modules/mod.rs`: **0** real mentions of the 6 new modules (the 2 grep hits are the
  word "exposes" in doc comments). Registration is integrator work.
- Route parity: every file shows `RouteInfo` count = `.route(` count + 2; the +2 is
  structural (`use ... RouteInfo;` import + `fn route_info(&self) -> Vec<RouteInfo>`).
  Builders verified route sets by comparison; coordinator spot-confirmed the +2 source.

## 2. mod.rs registration (exact lines for the integrator)

```rust
pub mod appointments;
pub mod elearning;
pub mod field_service;
pub mod marketing;
pub mod pos;
pub mod webinars;
```

And in the registry function (same form as the other modules):

```rust
appointments::register(registry);
elearning::register(registry);
field_service::register(registry);
marketing::register(registry);
pos::register(registry);
webinars::register(registry);
```

`store.rs` is already registered — the digital-products extension needs no new `mod` line.

Module metadata (all declare `available_as_product = true` in `impl Module`):
- webinars — category `communication`, requires `["meetings", "chat", "storage", "billing"]`
- elearning — category `productivity`, requires `["store", "storage", "billing", "roles"]`
- pos — category `commerce`, requires `["store", "billing", "assets", "tax"]`
- appointments — category `productivity`, requires `["events", "users", "notifications", "billing"]`
- marketing — category `communication`, requires `["mail", "newsletter", "crm", "compliance"]`
- field_service — category `productivity`, requires `["assets", "projects", "incidents", "timetracker"]`
- digital products — `DIGITAL_PRODUCTS_AVAILABLE_AS_PRODUCT = true`, category `commerce`
  (module constants + docs in store.rs; `StoreModule::available_as_product()` deliberately
  stays `false` — the store is the marketplace, not a sellable module).

## 3. Schema / models — 56 tables (paste into `src/db/schema.rs` + `src/db/models.rs`)

Webinars (000077): `webinars`, `webinar_registrations`, `webinar_polls`, `webinar_poll_votes`,
`webinar_questions`, `webinar_attendance`, `webinar_recordings`.
E-Learning (000076): `courses`, `course_modules`, `lessons`, `enrollments`, `lesson_progress`,
`quizzes`, `quiz_questions`, `quiz_attempts`, `certificates`, `reviews`, `learning_paths`,
`learning_path_courses`.
Digital products (000075): `digital_products`, `digital_product_files`,
`digital_product_previews`, `digital_royalties`, `digital_downloads`,
`digital_product_types`, `digital_payout_requests`, `digital_watermark_formats`.
POS (000074): `pos_registers`, `pos_shifts`, `pos_sales`, `pos_sale_items`, `pos_payments`,
`pos_refunds`, `pos_receipts`.
Appointments (000075): `appointment_services`, `appointment_staff_availability`,
`appointment_staff_services`, `appointments`, `appointment_reminders`.
Marketing (000080): `marketing_campaigns`, `marketing_campaign_variants`, `marketing_segments`,
`marketing_segment_size_history`, `marketing_sequences`, `marketing_sequence_steps`,
`marketing_enrollments`, `marketing_scores`, `marketing_score_rules`, `marketing_sends`.
Field service (000077): `field_equipment`, `field_maintenance_plans`, `field_work_orders`,
`field_work_order_items`, `field_work_order_parts`, `field_dispatch_assignments`,
`field_signoffs`.

Column definitions live in each migration's `up.sql` — copy from there, do not re-derive.
No foreign keys to other modules' tables (plain references, per project convention).

## 4. Migration collisions — RENUMBER REQUIRED (verified by coordinator, 2026-11-22 ~12:07 WIB)

Full duplicate-number list across the whole tree (6-digit prefix, verified by listing):

| Number | Dirs sharing it | Action |
|---|---|---|
| 000056 | `000056_community` (batch 4) + `000056_storage_product_byo` (storage follow-up) | renumber one |
| 000070 | `000070_presets` (batch 9) + `000070_saas_domains` (batch 8) | renumber one |
| 000075 | `000075_appointments` + `000075_digital_products` (BOTH batch 10) | renumber one |
| 000077 | `000077_field_service` + `000077_webinars` (BOTH batch 10) | renumber one |
| 000080 | `000080_backup_drill_verify_tenant` (batch 11) + `000080_marketing` (batch 10) | renumber one |
| 000081 | `000081_curriculum` + `000081_manufacturing` (both batch 12) | renumber one |

NOT duplicated (earlier builder claims were stale — verified by listing):
000068 (only `000068_licensing_saas`; network_tools moved to 000069),
000074 (only `000074_pos`; batch 11's backup drill is 000080),
000079 (only `000079_import_export_hardening`).
`000100_calculator` (batch 3) is still out of sequence — renumber into the gapless sequence.
Suggested renumber target range: the tree is at 000086 + 000100; use the next free
sequential numbers above the current max, keeping `down.sql` filenames in sync.

## 5. Scheduler kinds to add to `KNOWN_KINDS`

- `webinar.start` / `webinar.reminder` (webinars listens to `webinar.start_requested` /
  `webinar.reminder_requested`; enqueue currently records `declined`)
- `appointments.reminders.send` (appointments enqueues via `scheduler.enqueue`; rows record
  `scheduler_status: "declined"` until added)
- `marketing.campaign.send` / `marketing.sequence.process_due` (marketing enqueues with
  queue `marketing`; scheduler must know the callback hooks)
- `field_service.plans.check` (field service runs on demand via `POST /plans/check-due` or
  the `field_service.plans.check_requested` hook until added)

## 6. Hook listeners the OTHER side must add (exact contracts)

- **Store → elearning**: `order.paid` / `store.order.completed` payloads must carry
  `course_id` + `module: "elearning"` (or an `items` entry with them); otherwise paid
  enrollments sit `pending_payment` (fail-closed, `skipped` hook result).
- **Assets → POS / field service**: `assets.stock.decrement_requested` /
  `assets.stock.increment_requested` (POS) and `assets.stock.adjust_requested`
  (field service) have NO listener today — assets registers only
  `assets.warranty.check_requested`. Stock is never touched directly by these modules.
- **Billing → appointments**: `billing.invoice.requested` has no listener; no invoice is
  created by appointments today. Billing also must listen to
  `store.digital.payout.requested` (digital royalties) — no listener found.
- **CRM → appointments / marketing**: `crm.activity.requested` unverified/no listener;
  marketing segments evaluate only the synced audience — CRM should emit snapshots or
  answer a contact-list event.
- **Chat → webinars**: no listener for `webinar.chat.message`; the endpoint honestly
  reports `delivered: false` until one subscribes.
- **Surveys → webinars / field service**: webinars emits `surveys.send_requested` for
  follow-up (needs a `survey_distribution_id`); field service emits
  `surveys.send_requested` on sign-off (same requirement, else `pending_no_distribution`).
- **Compliance → marketing**: `compliance.consent.check` is unanswered; contacts with no
  consent on record are skipped, never mailed (fail-closed by design).
- **Web3 → elearning**: elearning emits `web3.anchor.requested`
  (`record_type: "elearning.certificate"`, hash only, never personal data) and listens for
  `web3.anchor.confirmed` / `web3.anchor.failed`.
- **Meetings → webinars**: webinars links a room via `link-meeting` and asks it to go live
  through `meeting.start_requested`, which meetings already listens to.
- **TimeTracker → field service**: `timetracker.time.log_requested` has no listener;
  reported as `handler_answered: false`.
- **Incidents → field service**: `incidents.create_requested` has no listener;
  reported as `handler_answered: false`.
- **Mail → marketing**: marketing correlates `mail.sent` / `mail.failed` by job id; tracking
  is receive-only (no tracking pixel is added by marketing — opens/clicks stay 0 until a
  tracking source reports them).
- **Forms / store → marketing**: `forms.submission.received` carries no submitter identity
  (form-triggered enrollment needs explicit `marketing.sequence.enroll`); `store.order.completed`
  carries `user_id` but no email (resolved via audience); no tag event exists.
- **Tax → POS**: POS computes line tax from caller-supplied rates (or the configured
  default); tax's own computation from `order.created` is authoritative. No tax-quote hook.
- **Store/products → POS**: `pos.product.search_requested` has no catalogue handler;
  endpoint honestly returns `catalogue_available: false` until one registers.
- **Notifications**: webinars, appointments, field service all emit `notification.requested`
  (already answered by the notifications module).

## 7. Roles capabilities to add

- `webinars:*` (view/manage/host), `elearning:*` (view/teach/manage),
  `pos.view` / `pos.sell` / `pos.manage` / `pos.refund`,
  `appointments:*` (view/manage/book), `marketing.view` / `marketing.manage` /
  `marketing.send`, `field_service.view` / `field_service.manage` /
  `field_service.technician`, `store.digital:manage` / `store.digital:read`.
- Field-service note: the roles module answers `role.check` for roles, not users, so a
  technician's capability is enforced when they act (`can_act_on_order`), not at
  assignment time.

## 8. ApiAccessRule rows (public routes — default deny otherwise)

- E-learning: `GET /certificates/verify/{code}` is intentionally public (anyone holding a
  code must verify it; unknown/revoked both 404).
- Appointments: 4 public handlers (active-services list, public slot lookup, guest booking,
  confirmation-code lookup) — gated by API Manager `ApiAccessRule`, default deny.
- Digital products: 3 intentional storefront reads (catalog, previews, sample) — anonymous
  allowed; role-present callers must hold `store.digital:read`; drafts stay 404 to
  non-managers.
- Webinars: replay is auth-gated by design; no public routes.
- POS / marketing / field service: no public routes.

## 9. AUDITED_EVENTS additions (suggested)

`webinar.*`, `elearning.enrollment.*`, `elearning.certificate.*`, `pos.sale.*`,
`pos.shift.*`, `appointments.*`, `marketing.campaign.*`, `marketing.send.*`,
`field_service.work_order.*`, `store.digital.*`. (Exact per-module event lists are in each
module's header comments.)

## 10. README / TODO lines (recount at push time — batches 11/12 landing concurrently)

- Module count: 6 new modules (webinars, elearning, pos, appointments, marketing,
  field_service) + digital products inside store.
- Migrations: 93 dirs at verification (was 86 before batch 10's 7).
- Tables: +56 batch-10 tables.
- Note the 6 duplicate migration numbers in §4 and the `000100_calculator` renumber.

## 11. Odoo modules considered and NOT built (gap list stays visible)

Built in this batch (all 4 bounded picks): POS, Appointments, Marketing Automation,
Maintenance / Field Service (as `field_service.rs` — `maintenance.rs` already exists as the
maintenance-mode/status-page module and was not touched).
Already covered by existing Unified modules (no build needed): CRM (crm.rs), Sales
(store.rs), Purchase (procurement.rs), Inventory (assets.rs), Accounting/Billing
(billing.rs), HR (hr.rs), Projects (projects.rs), Helpdesk (tickets.rs), Website/Site
Builder (site_builder.rs), Documents (documents.rs / vault.rs), eCommerce (store.rs),
Events (events.rs), Discuss/Chat (chat.rs), Surveys (surveys.rs), Calendar (events +
appointments now), Contacts (crm.rs), Employees (hr.rs), Expenses (batch 12 building),
Manufacturing / Quality / Studio / Approvals / Curriculum (batch 12 building — see overlap
note below), Invoicing incl. print engine (batch 12 building).
**Overlap warning for the integrator:** batch 12 has `000081_curriculum` — check it against
this batch's `elearning.rs` for duplication before registering both.
Future (not built, recorded only): Fleet, Planning, PLM, Sign (contracts e-sign covers
in-app signing), Rental, Subscription is covered (subscriptions in batch 3), VoIP, IoT,
Social Marketing (inbox has no social module — noted in batch 4).

## 12. Modularity rule check (standing rule — nothing hard-coded)

Disclosed hard-coded spots (per builder, all reviewed):
- Status vocabularies, capability names, numeric caps (lengths, price/attempt bounds),
  `ELRN-` certificate code prefix (elearning); seed initial values + wizard-style
  defaults (all modules); POS seeded payment-method defaults (cash/card/bank_transfer/
  e_wallet, explicitly marked configurable defaults, admin CRUD); field-service seeded
  equipment types/statuses (admin-editable registries, `is_system` flagged).
- Behavioural lists are settings-driven everywhere: elearning levels/categories/languages/
  lesson types/pass mark; appointments slot length/buffers/cancellation policy/location
  types; marketing field/operator allowlists + scoring rules; webinars config via
  `GET/PATCH /config`; digital-product type registry + watermark-format registry.
- No hard-coded provider/plan/preset/network lists found in batch-10 code.

## 13. OPEN list (consolidated — anything unfinished or unverified)

1. **`cargo check` / `cargo build` NEVER ran anywhere** (no toolchain, crates.io blocked).
   Compilation is unverified for all 7 items. Riskiest surfaces: the hand-rolled
   `signed_download` digital branch in store.rs, marketing's segment rule-tree evaluation,
   field service's interval/date math, elearning's quiz grading.
2. **Migrations never executed** against live PostgreSQL; in-memory DashMap stores are
   authoritative everywhere (same standing limit as all other modules).
3. No live HTTP test of any endpoint, gate decision, or migration.
4. **Six duplicate migration numbers** (§4) + `000100_calculator` out of sequence —
   integrator MUST renumber before any migration run.
5. All of §5–§8 (scheduler kinds, listeners, access rules, capabilities) is pending
   integrator work; several cross-module calls fail closed until wired (listed in §6).
6. **Appointments double-booking check is check-then-insert in memory, not atomic** —
   two simultaneous requests for the same slot can both pass; needs a DB exclusion
   constraint / serialisable transaction (integrator follow-up).
7. Digital products: byte-level watermark embedding NOT implemented — the module computes,
   records, and emits the descriptor (`store.digital.download` event); a storage/delivery
   adapter must apply it to file bytes. Royalties in a payout request do not auto-reverse
   on refund (manual adjustment). Sample rate limiting is in-memory per identity header,
   resets on restart, not multi-instance safe.
8. POS: payment-method definitions and POS settings are runtime-only config (same pattern
   as billing settings); durable form is a follow-up. Offline queueing/sync is FUTURE —
   server-side only. Receipt print lines are printer-neutral structured data; ESC/POS byte
   generation belongs to the batch-12 print engine.
9. E-learning: certificate learner name falls back to user id (no profile lookup);
   `elearning.path.completed` is reported in progress payloads, not emitted as an event
   on read — emit from `maybe_complete_course` in a follow-up if a listener needs it.
10. Webinars: no SFU/media server — same browser-to-browser WebRTC limit meetings
    documents. Scheduler kinds not in `KNOWN_KINDS` yet.
11. Marketing: six `.expect()` calls (lock-poisoning / post-insert "still present" only,
    same pattern as mail/newsletter), none on user input. Tracking is receive-only.
12. Field service: technician capability enforced at action time, not assignment time.
    Scheduler kind `field_service.plans.check` not in `KNOWN_KINDS` yet.
13. Appointments: reminder rules have no DB table (in-memory); times are UTC only (no
    per-staff timezone); guest rate limit is per-process memory; staff user ids validated
    for shape only (no user-directory lookup hook exists).
