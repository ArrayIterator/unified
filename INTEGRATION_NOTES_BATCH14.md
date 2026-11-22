# Integration notes — Batch 14 (Smart Modules: Online Exams, Library, SaaS Packages, AI Capabilities Contract)

Coordinator-verified by read-back. Nothing pushed. Remote head untouched (`7cf6853c`).

## 1. What was built

| Item | Files | Lines | Routes | Migration | Tables |
|---|---|---|---|---|---|
| Online Exams | `src/modules/exams.rs` (new) | 5,477 | 58 | `000090_exams` | 10 |
| Library | `src/modules/library.rs` (new) | 5,773 | 65 | `000091_library` | 11 |
| SaaS Packages | `src/modules/saas_packages.rs` (new) | 4,639 | 32 | `000096_saas_packages` | 4 |
| AI contract | `src/modules/ai.rs` (+692: 7,173 → 7,865) | +692 | +3 generic | — | 0 |
| AI first wave (wired) | `content.rs` (+181), `knowledgebase.rs` (+101), `mail.rs` (+77), `academic.rs` (+211), `curriculum.rs` (+191), `elearning.rs` (+210) | +971 | +8 | — | 0 |
| **Totals** | **3 new modules + ai.rs + 6 wired modules** | **~17,552 new/changed** | **166 new** | **3** | **25** |

Tree at verification: 137 `.rs` files in `src/modules/`, 105 migration dirs, 441,182 Rust lines.

### Module shape
- **exams** (`id: "exams"`, sellable, `productivity`): question banks (5 types, versioned), timed exams (server-side timer, seeded randomisation, autosave, resume), proctoring signals only (no camera proctoring — stated in header), auto + manual grading (rubrics, second grader), results/analytics (discrimination index), grades → SIAKAD via `academic.grade.recorded` with academic.rs's exact payload `{grade_id, enrollment_id, component_id, score}`. Requires `elearning, academic, notifications, billing, storage, roles`. Permissions `exams.view/manage/take/grade`.
- **library** (`id: "library"`, sellable, `productivity`): catalog (ISBN-10/13 checksum, copies, data-driven taxonomy), data-driven loan policies (409 when no policy matches, never a baked-in fallback), loans/holds/fines (fines via `library.fine.requested` — billing owns money), signed HMAC digital access, inventory audits, reminders. Requires `documents, storage_manager, billing, notifications, users, roles`. Permissions `library.view/manage/circulate/catalog`.
- **saas_packages** (`id: "saas_packages"`, sellable, `commerce`): packages as DATA (CRUD, versioning, clone, validate), example "Online Exam SaaS" seed package (data only), purchase via store events, six-step provisioning (entitlements → subdomain → enable modules → preset settings → wizard pre-fill → welcome) with per-step outcomes + retry; entitlement failure BLOCKS provisioning (never silently skipped); upgrade/downgrade module diff (never disables without `confirm_disable`); cancel + grace. Requires `products, store, billing, extensions, licensing, notifications, roles` (saas_domains + presets deliberately NOT in `requires` — unregistered in `mod.rs`, event-only integrations). Permissions `saas_packages.view/manage/purchase/provision`.
- **AI contract** (`ai.rs`): `AiCapability` struct, `AiCapabilityRegistry` (DashMap + OnceLock), `register_ai_capabilities(module_id, capabilities)`, `pub async fn run_ai_capability(ctx, caller, company_id, module_id, capability, input)` — registry lookup (404 `ai_capability_not_found`), exact input-field validation (422), PII redaction via `privacy::redact_value` when `!allows_pii`, grounding hook `<module>.ai.<capability>_requested`, `authorize_ai_call` FIRST (verified by reading: authorize ~line 4555, `run_completion` ~line 4580 in the function), completion via existing `run_completion`, usage + `ai.call.completed`/`ai.call.failed`. Generic endpoints: `GET /api/v1/ai/capabilities`, `GET /api/v1/ai/capabilities/{module_id}`, `POST /api/v1/ai/capabilities/{module_id}/{capability}` (all require `ai:use`). AI module's own capabilities registered: `summarize`, `draft`, `analyze`, `translate`.

### AI first wave — wired (7 modules)
| Module | Capabilities | Grounding |
|---|---|---|
| ai | summarize, draft, analyze, translate | — |
| content | draft (title, brief), summarize (text) | related published posts/pages |
| knowledgebase | answer_draft (question) | published articles (limit 5) |
| mail | draft (subject, bullets) | none (`grounded: false` — send log holds recipient addresses, never offered) |
| academic | summarize_progress (student_id) | anonymised aggregates only (GPA, credits, standing, counts, attendance rate, per-subject averages; no user_id/student_number/names) |
| curriculum | draft_lesson_plan (core_competency_id, basic_competency_id, topic) | KI/KD code/title/description/grade_year + subject + framework |
| elearning | draft_quiz (lesson_id, count, difficulty), summarize_lesson (lesson_id) | lesson title/type/body (cap 8,000 chars) + course; quiz adds ≤5 existing questions; no learner data |

### AI declared by owner builders (3 modules)
- exams: `draft_questions` (topic, count, difficulty, question_type), `analyse_results` (exam_id). Compatible with `ai::Caller`.
- library: `search_assist` (query), `recommendations` (user_id). **INCOMPATIBLE — see §4.**
- saas_packages: `recommend_package` (business_type, needs), `describe_package` (package_id). **INCOMPATIBLE — see §4.**

## 2. Coordinator verification (read-back, not trust)

- Line counts match builder reports exactly except 1-line trailing-newline differences on ai.rs (7,865 vs 7,866), content.rs (1,260 vs 1,261), knowledgebase.rs (1,314 vs 1,315), mail.rs (1,108 vs 1,109), academic.rs (6,337 vs 6,338), curriculum.rs (4,133 vs 4,134), elearning.rs (4,870 vs 4,871), saas_packages.rs (4,639 vs 4,640). Exams (5,477) and library (5,773) exact.
- Route counts: exams 58, library 65, saas_packages 32 `RouteInfo` entries (verified by grep; builders verified 1:1 vs `configure_routes`).
- Forbidden reference-project name: **0** across all batch-14 files (grep over the 10 files).
- Indonesian words (word-boundary, broad list incl. kavling/denah/ujian/perpustakaan/buku/pinjam): **0** in the 3 new module files. The `(RPP)`, `(KI)`, `(KD)` abbreviations in curriculum.rs:3718 are task-specified domain terms, not prose.
- Stub macros (`todo!`/`unimplemented!`): **0** in the 3 new files and in ai.rs additions.
- Delimiter balance: 0/0/0 on exams.rs, library.rs, saas_packages.rs (verified with python counts).
- Migrations: `000090_exams` 10 CREATE = 10 DROP; `000091_library` 11 CREATE = 11 DROP; `000096_saas_packages` 4 CREATE = 4 DROP.
- `run_ai_capability` calls `authorize_ai_call` before any provider traffic (verified by reading the function body).
- `src/core/tenancy.rs` exists on disk (`schema_name`, `TenantContext::from_resolved`, `ControlPlaneGuard`, `with_tenant_connection`); saas_packages uses it. Exams + library do NOT use it (built before it landed) — see §7.

## 3. Integrator snippets — registration

`src/modules/mod.rs` — add declarations (alphabetical) and registrations:
```rust
pub mod exams;
pub mod library;
pub mod saas_packages;
pub mod privacy; // REQUIRED: ai.rs + import_export.rs use crate::modules::privacy — without this ai.rs will not compile (see §4)
```
```rust
exams::register(registry);
library::register(registry);
saas_packages::register(registry);
```

`src/db/schema.rs` / `src/db/models.rs` — 25 tables (all TEXT ids, no FKs, money NUMERIC(19,4)):
- exams (`000090_exams`): `exam_question_banks`, `exam_questions`, `exam_question_versions`, `exams`, `exam_question_links`, `exam_rubrics`, `exam_attempts`, `exam_grades`, `exam_proctor_signals`, `exam_results`.
- library (`000091_library`): `library_items`, `library_categories`, `library_copies`, `library_members`, `library_loans`, `library_holds`, `library_fines`, `library_loan_policies`, `library_inventory_audits`, `library_inventory_discrepancies`, `library_digital_access`.
- saas_packages (`000096_saas_packages`, control-plane `public` schema): `saas_packages`, `saas_package_modules`, `saas_package_purchases`, `saas_package_provision_steps`. Tenant enabled-module state is tenant-schema data (via `tenancy::schema_name`), not a `public` table.

`src/modules/audit.rs` — add to `AUDITED_EVENTS`:
```rust
"exams.exam.published", "exams.exam.closed", "exams.attempt.submitted", "exams.attempt.graded",
"exams.result.published", "exams.certificate.requested", "academic.grade.recorded",
"library.item.created", "library.item.updated", "library.item.withdrawn", "library.loan.created",
"library.loan.returned", "library.loan.overdue", "library.loan.marked_lost", "library.loan.marked_damaged",
"library.hold.created", "library.hold.fulfilled", "library.hold.expired", "library.fine.requested",
"library.fine.waived", "library.fine.paid", "library.inventory.completed", "library.inventory.discrepancy.resolved",
"saas_packages.package.created", "saas_packages.package.updated", "saas_packages.package.published",
"saas_packages.package.archived", "saas_packages.package.deleted", "saas_packages.package.cloned",
"saas_packages.purchase.requested", "saas_packages.purchase.completed", "saas_packages.provision.started",
"saas_packages.provision.step_completed", "saas_packages.provision.completed", "saas_packages.provision.failed",
"saas_packages.module.enabled", "saas_packages.module.disabled", "saas_packages.upgraded",
"saas_packages.downgraded", "saas_packages.cancelled", "saas_packages.renewal.requested",
"saas_packages.settings.updated",
```

Role capability seeds (no seed precedent found for module capabilities in roles.rs — wire per the roles module's convention): `exams.view/manage/take/grade`; `library.view/manage/circulate/catalog`; `saas_packages.view/manage/purchase/provision`. `ai:use` already exists.

README/TODO lines (suggested):
```
- Online Exams (src/modules/exams.rs, migration 2026-11-22-000090_exams): versioned question banks, timed exams with server-side timers, auto + manual grading with rubrics, results/analytics, grades flow to SIAKAD; AI draft_questions + analyse_results (smart module).
- Library (src/modules/library.rs, migration 2026-11-22-000091_library): catalog/ISBN/copies, data-driven loan policies, loans/holds/fines via billing events, signed digital access, inventory audits; AI search_assist + recommendations (smart module).
- SaaS Packages (src/modules/saas_packages.rs, migration 2026-11-22-000096_saas_packages): data-defined sellable bundles of modules, purchase via store, six-step tenant-scoped provisioning, upgrade/downgrade diff, cancel + grace; AI recommend_package + describe_package (smart module).
- AI Capabilities Contract (src/modules/ai.rs): per-module capability registry + run_ai_capability; wired: ai, content, knowledgebase, mail, academic, curriculum, elearning; owner-declared: exams, library, saas_packages.
```

## 4. AI contract — integrator MUST-FIX items (compile-blocking)

1. **`pub mod privacy;` missing from `src/modules/mod.rs`** (verified by grep — confirmed). `ai.rs` (via `privacy::redact_value`) and the pre-existing `import_export.rs:114` both use `crate::modules::privacy`. Without the declaration, ai.rs will not compile. Add it (see §3).
2. **Caller type mismatch — library + saas_packages will not compile.** `run_ai_capability` requires `&crate::modules::ai::Caller`, but `library.rs:1020` defines its own local `struct Caller { actor, role }` and `saas_packages.rs:1152` defines its own local `pub struct Caller`; both pass their local type at the call sites (library.rs:5203, 5243; saas_packages.rs:3954, 3993). Exams already converts correctly. Fix (same pattern at all four sites):
   ```rust
   let ai_caller = crate::modules::ai::Caller { actor: caller.actor.clone(), role: caller.role.clone() };
   let output = crate::modules::ai::run_ai_capability(&ctx, &ai_caller, company_id, "library", "search_assist", input).await;
   ```
   Recommend the owners also switch company resolution to `ai::resolve_capability_company` (tenant-first) where they use their own header resolution.
3. **No package-entitlement gate on capability calls.** `entitlements.check_requested` has no registered answerer and `entitlements.rs` is not in `mod.rs`. The gate belongs in `run_ai_capability` once the entitlements module (batch 15) is registered and answering. Until then, capability calls are gated by the allowlist + layered gate only.
4. **Capability outputs are NOT stored as `AiDraft`s** (spec-literal; draft endpoints own the human-review flow). Integrator decision: persist an `AiDraft` for review, or keep `run_ai_capability` stateless.
5. **`run_ai_capability` has no provider_id / use_personal_key / product context** — default connection, connection key, product layer defaults to allow (no request context in the signature). A later round should add optional provider/product parameters if per-call provider selection is wanted.
6. **PII redaction path:** there is NO `privacy.redact_requested` hook in `privacy.rs`; `run_ai_capability` calls the public `privacy::redact_value` helper directly. Response carries `pii_redacted: bool`. Do not claim hook-based redaction.
7. **Tenant-first company resolution:** `resolve_capability_company` (new in ai.rs) — on a tenant request the middleware `TenantContext` is authoritative (validated via `tenancy::TenantContext::from_resolved`, non-UUID → reject); invalid tenant context resolves to `None` (gate denies when enforcement is on), never falls back to the header claim. Control-plane requests fall back to `resolve_caller_company` (header, then `user.company_requested` hook). No DB touched on this path, so no `public`-schema fallback exists in AI code.

## 5. Migration collisions (integrator must renumber)

Verified tree-wide by the coordinator (duplicate numbers, each twice): **000056, 000070, 000075, 000077, 000080, 000090**; plus **000100_calculator out of sequence**. Batch-14's own numbers: `000090_exams` collides with batch 13's `000090_school_fees` (exams took 000090 only because 000089 was taken by `000089_academic`); `000091_library` and `000096_saas_packages` are unique. The live race during this round: 000087 (curriculum, batch 13), 000088 (admissions, batch 13), 000089 (academic, batch 13), 000090 (exams + school_fees), 000091 (library), 000092 (entitlements, batch 15), 000093 (product_subdomains, batch 15), 000094 (tenant_isolation, batch 17), 000095 (calendar, batch 16), 000096 (saas_packages). Renumber into a gapless sequence at integration; do NOT renumber by hand-editing one side only — the module files reference their migration numbers (library.rs had 2 internal references updated 000090 → 000091; saas_packages.rs references 000096 in 2 spots).

## 6. Convergence points (integrator — cross-batch, fail closed today)

1. **Grade payload mismatch — academic vs curriculum (CONFIRMED by batch 13).** Exams emits `academic.grade.recorded` with academic.rs's canonical payload `{grade_id, enrollment_id, component_id, score}`. Curriculum expects `{academic_grade_id, student_ref, basic_competency_id, score}` and `academic.student.created {student_ref, level_id}`. The grade→competency mapping is undecided. Integrator must pick one canonical shape (or a bridge event) — until then, curriculum-side grade recording from exams will not land.
2. **Exams vs curriculum 3-domain assessment overlap.** Exams has its own per-question grading/analytics; curriculum has 3-domain (knowledge/skills/attitudes) assessment. Reconcile at integration: exams results feed curriculum assessment via the grade event, or keep them separate with documented boundaries — do not let both claim the same record.
3. **Admissions → academic handoff dead-ends (batch 13, confirmed).** Academic does NOT subscribe to `student.create_requested`; `student.created` payload lacks `application_id`. School fees consumes `academic.student.enrolled/.withdrawn`, which academic never emits (`enrollment.approved/.dropped` exists as the alternative). `elearning.course.requested` has no answerer; `billing.invoice.requested` has no listener (admissions fee degrades to manual `mark_paid`); `academic.student.lookup` has no answerer.
4. **Expenses ↔ approvals event-name bridge REQUIRED (batch 12).** `expenses.approval.requested` vs `approval.requested` — until bridged, submitted expense reports stay submitted. Manufacturing ↔ quality completion-event name mismatch (batch 12).
5. **SaaS packages provisioning handoffs (no listeners yet):** no domains listener for `site.provision_requested` (saas_domains answers resolve/lookup/reservation calls and emits `site.provisioned` as an outcome only — step completes as `requested` + `subdomain_pending: true`); no presets listener for `saas_packages.wizard.prefill_requested` (presets owns `presets.wizard.*` but no prefill event); hook-path entitlement check defers as `blocked` (hook handlers have no `AppContext`, so they cannot call `check_module_entitlement` — durable fix: licensing answers `saas_packages.entitlement.check_requested`, or hook handlers receive an `AppContext`); billing has no documented listener for `billing.subscription.renew_requested` / `.cancel_requested`; grace-period expiry has no scheduler sweep (purchase moves to `cancelled` only on `subscription.expired` or admin action).
6. **Batch 15 convergence (per-product selling + smart pricing):** saas_packages is the BUNDLE layer; per-product entitlement/pricing is batch 15's (`entitlements.rs` + `000092_entitlements` on disk, `pricing.rs` landed). Confirm the canonical entitlement source (licensing `check_module_entitlement` vs batch 15 entitlements) and point all checks — including the hook-path query — at it. Pricing stays a product-id reference in saas_packages; batch 15 owns price math/discounts/proration — never duplicate it here.
7. **Batch 17 convergence (tenant isolation):** plumbing landed (`src/core/tenancy.rs`, migration `000094_tenant_isolation`; `tenants.rs` + `tenant_destroy.rs` now exist per batch 17's completion). TWO CRITICAL batch-17 items the integrator must fix first: (a) the tenants seam mismatch (assumed `TenantContext::new` / `context.schema()` / `create_tenant_schema` / `run_tenant_migrations` do not exist — fix isolated to the seam functions); (b) the migration CHECK regex rejects real schema names (`^tenant_[0-9a-f_]{36}$` vs actual `tenant_` + 32 hex — must become `^tenant_[0-9a-f]{32}$`). Sequence tenant record/schema creation BEFORE package provisioning, or have the tenants module answer `site.provision_requested` / a provisioning handoff. Naming warning: two types named `TenantContext` (middleware vs tenancy — alias imports).
8. **Library assumed event names (verify against source):** `billing.payment.completed` / `billing.invoice.paid` (payload `fine_id` or `reference_type="library.fine"` + `reference_id`) and `user.deleted` — handlers read both shapes defensively, but the integrator must confirm the emitters exist.
9. **Batch 18 (credit cards + reference order ID) adds a SECOND sanctioned hard-code exception** (card-brand IIN/BIN ranges + Luhn checksum + per-brand CVV lengths are international standards and may be hard-coded; admin can also define custom rules). Sanctioned exceptions to the modular rule are now: (1) standard paper sizes, (2) card-brand IIN/BIN + Luhn. Everything else stays data-driven.

## 7. Tenant-scoped data access (standing rule — status per module)

- **saas_packages: APPLIED.** Uses `core::tenancy` (verified on disk): `require_tenant_scope` on all 15 tenant-data endpoints (+ `change_package`), `require_control_plane` on the 8 package-admin/settings endpoints, `tenant_schema()` built only via `tenancy::schema_name`, every purchase/step/history record keyed by company id (uuidv7), no hard-coded tenant/schema/package ids. Runs no SQL and never sets a search_path (in-memory stores) — the DB half applies when the persistence target is wired.
- **AI contract: APPLIED (call path).** `resolve_capability_company` is tenant-first (see §4.7); AI stores are in-memory with no tenant partition and no Postgres access, so there is no tenant-column filtering to enforce yet. When AI state gains Postgres persistence, usage/policies/capability results must go through `with_tenant_connection` in the tenant schema; provider connections/global settings through `public` (via `ControlPlaneGuard`). Grounding handlers must filter by the payload's caller company id once their stores carry one.
- **exams + library: NOT YET WIRED.** Both finished before `core/tenancy.rs` landed on disk. Their data is keyed by company/user ids in-memory, but they do not call the tenancy extractor and do not fail closed on missing tenant context. Integrator convergence point: add `require_tenant_scope` (or the module's equivalent guard) to their tenant-data endpoints once the tenancy middleware is wired into the request path — do NOT invent a second tenancy helper.
- **No tenant context → fail closed, never silently fall back to `public`.** No tenant/schema/package id is hard-coded anywhere in batch-14 code.

## 8. AI rolling checklist — all 136 modules on disk

Status: **W** = wired this wave · **O** = declared by owner builder · **P** = pending (later round). Capabilities for P modules are the obvious candidate, not declarations. Full per-module table (from the AI builder's report, verified against `src/modules/mod.rs`):

**Wired (7):** ai (summarize, draft, analyze, translate) · academic (summarize_progress) · content (draft, summarize) · curriculum (draft_lesson_plan) · elearning (draft_quiz, summarize_lesson) · knowledgebase (answer_draft) · mail (draft).
**Owner-declared (3):** exams (draft_questions, analyse_results) · library (search_assist, recommendations) · saas_packages (recommend_package, describe_package).
**Pending (126):** admissions (draft_admission_letter, summarize_application) · advisor (draft_advice) · affiliates (draft_outreach) · ai_support (support_reply — surface exists via `ai.support_draft`) · analytics (summarize_report, explain_trend) · announcements (draft_announcement) · api_manager (explain_endpoint, draft_docs) · apikeys (none obvious) · app_sync (summarize_sync_conflicts) · appointments (draft_reminder) · approvals (summarize_pending_approvals) · assets (none obvious) · audit (summarize_findings, anomaly_summary) · auditors (summarize_audit) · auth_manager (none obvious) · backup (summarize_backup_health) · bank_transfer (none obvious) · bi (explain_metric) · billing (explain_invoice, draft_dunning) · cache (none obvious) · calculator (explain_calculation) · calendar (draft_event_summary) · certificates (none obvious) · chat (chat_reply — surface exists via `ai.chat_draft`) · cloudflare (explain_dns_change) · clusters (summarize_cluster_health) · community (draft_post, summarize_thread) · company (draft_company_profile) · compliance (summarize_compliance_gap) · contracts (summarize_contract) · crm (summarize_customer, draft_follow_up) · dashboards (explain_widget, narrate_dashboard) · departments (none obvious) · documents (summarize_document) · domain_verification (none obvious) · domains (none obvious) · downloader (none obvious) · email_providers (none obvious) · email_templates (draft_template) · entitlements (suggest_package — hook `entitlements.ai.suggest_requested` exists) · events (draft_event_description) · expenses (summarize_expenses, categorize_expense) · extensions (none obvious) · field_service (draft_visit_summary) · files (summarize_document) · forms (draft_form_questions) · gamification (none obvious) · geo (none obvious) · holidays (none obvious) · hosting (explain_incident) · hr (draft_job_post, summarize_candidate) · i18n (translate_string) · import_export (summarize_import_errors) · inbox (draft_reply, summarize_thread) · incidents (summarize_incident) · licensing (explain_license_terms) · load_balancer (summarize_traffic) · logistics (summarize_shipment) · logs (summarize_errors) · loyalty (draft_offer) · maintenance (draft_maintenance_notice) · manufacturing (summarize_production) · marketing (draft_campaign) · mcp (none obvious) · meetings (summarize_meeting, draft_minutes) · network (explain_network_issue) · network_tools (explain_diagnostic) · newsletter (draft_newsletter — surface exists via `ai.email_draft`) · notifications (draft_notification) · okrs (draft_objective, summarize_progress) · panels (none obvious) · payments (explain_payment_failure) · planner (suggest_plan) · portals (none obvious) · pos (none obvious) · presets (none obvious) · pricing (explain_price, suggest_price) · pricing_tables (draft_pricing_copy) · printing (none obvious) · privacy (none obvious) · procurement (draft_rfq) · products (draft_product_description) · projects (summarize_project_status) · properties (draft_listing) · providers (none obvious) · quality (summarize_inspection) · records (summarize_record) · recruitment (draft_job_post, summarize_candidate) · risk (summarize_risks) · roles (none obvious) · routes (none obvious) · saas (draft_offering) · saas_domains (none obvious) · saved_views (none obvious) · scheduler (explain_schedule) · search (none obvious) · security (summarize_security_events) · seo (draft_meta, suggest_keywords) · service_monitor (explain_outage) · settings (none obvious) · site_builder (draft_page — surface exists via `ai.content_draft`) · software (draft_release_notes) · statuspage (draft_incident_update) · storage (none obvious) · storage_manager (none obvious) · store (draft_product_description) · studio (draft_field_suggestion) · supervision (summarize_supervision) · surveys (draft_survey, summarize_responses) · tasks (summarize_tasks, draft_task) · tax (explain_tax_rule) · tenant_destroy (none obvious) · tenants (none obvious) · theme (none obvious) · tickets (support_reply — surface exists via `ai.support_draft`) · timeline (summarize_timeline) · timetracker (summarize_time) · todos (none obvious) · users (none obvious) · vault (none obvious) · vcs (summarize_changes) · web3 (none obvious) · webhooks (none obvious) · webinars (summarize_webinar) · whitelabel (none obvious) · workflows (suggest_workflow).

No module is claimed wired that is not. Existing AI surfaces NOT yet routed through the contract (later round may adopt them): `ai.support_draft`, `ai.chat_draft`, `ai.email_draft`, `ai.content_draft` (used by ai_support, chat, newsletter, site_builder, tickets).

## 9. OPEN list (consolidated — anything unfinished or unverified)

1. **Cargo check / cargo build has NEVER run anywhere** — no toolchain, crates.io blocked. Compilation is unverified for all batch-14 code. Known compile risks: library + saas_packages `Caller` mismatch (§4.2, will fail); `pub mod privacy;` missing (§4.1, will fail); `tenancy::TenantContext::from_resolved` usage in ai.rs (verified from source, returns `Result<Self, UnifiedError>`); DashMap/OnceLock patterns follow existing ai.rs usage.
2. **Migrations never executed** against live PostgreSQL; no endpoint live-tested; no live AI provider call; no live tenant provisioning.
3. **Integrator must:** register exams/library/saas_packages (+ `privacy`, REQUIRED) in `mod.rs`; add schema/models for 25 tables; renumber duplicate migrations (§5); apply the Caller conversion (§4.2); add the entitlement gate to `run_ai_capability` once batch 15's entitlements module is registered and answering; decide whether capability outputs persist as `AiDraft`s; add the missing listeners in §6.4–6.5; reconcile the grade payload (§6.1) and the exams/curriculum overlap (§6.2); wire tenant scoping into exams + library (§7).
4. **Exams:** short-answer grading is exact-match only (no fuzzy/partial credit); attempt-limit check is check-then-insert in memory (same race class as batch 10's appointments note); exam-link marks vs question marks — auto-grading awards the question's marks, not a per-link override; line count 5,477 vs 3,000–4,500 target (no features cut).
5. **Library:** billing paid event names assumed (`billing.payment.completed`, `billing.invoice.paid` — handlers read both shapes defensively); `user.deleted` event name assumed; digital redeem requires `library.view` PLUS the token (stricter than documents' token-only redeem — confirm intended); uniqueness checks are check-then-insert in memory (migration carries matching unique indexes); line count 5,773 vs 2,500–4,000 target.
6. **SaaS Packages:** hook-path entitlement defers as `blocked` (no `AppContext` in hook handlers); no domains/presets listeners (§6.5); seed modules exams/elearning/academic unregistered at write time (provisioning reports them unknown until registered); package version snapshots are in-memory only (no frozen versions table — integrator adds one if durable snapshots are needed); grace-period expiry has no scheduler sweep.
7. **AI contract:** no provider_id / use_personal_key / product context on `run_ai_capability` (default connection, product layer defaults to allow); envelope `code` is the numeric HTTP status, string reason codes live in `metadata` (tree convention); all AI stores in-memory with no tenant partition (see §7).
8. **Modularity — disclosed hard-coded spots (not hidden):** status vocabularies (exam/attempt/grade/result, loan/copy/hold statuses, package/purchase/step vocabularies, provisioning pipeline step names, capability names); field length limits and mark/percentage bounds (exams); policy wildcards `"all"`/`"default"` and caps (library TTL max 604800s, rate-limit max 10000, grounding max 50); ISBN checksum math; fine reason strings; seed initial values + builtin preset/package seeds (data, editable); wizard step vocabulary. Sanctioned exceptions (international standards): standard paper sizes, card-brand IIN/BIN + Luhn. No package, tenant id, schema name, price, provider list, or business list is hard-coded anywhere in batch-14 code.
9. **Pre-existing tree issues (not batch 14's to fix):** migration duplicates (§5); 16 pre-existing cert routes have no capability checks (batch 8); older billing handlers have no capability checks (batch 12); `viewer_id`/`viewer_role` trusted params until the auth extractor lands.
