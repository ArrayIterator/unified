# C3 task 315 — ControlPlaneGuard + write-strictness harmonization: decision brief

Status: ADOPTED 2026-11-23 — option (c) strict by default + named carve-outs, and platform decision (a) explicit audited platform scope. Implemented in the tree; see "Implementation (2026-11-23)" below and the guard map section "Tasks 315 + 320 outcomes".

## What 315 covers (three linked decisions)

### 1. ControlPlaneGuard adoption
Control-plane handlers (tenant registry, subscriptions, domains, billing,
licensing, platform admin — the `public`-schema control plane) should use one
guard, `ControlPlaneGuard`, instead of each handler rolling its own
platform-admin check. Adoption means: enumerate the control-plane handlers,
wire the guard, and record which handlers are control-plane vs tenant-facing
(the guard map already distinguishes these).

### 2. Platform-admin scope — the open product decision
A platform admin has NO company. Today, in the 16 strict modules, a
company-less caller gets 403 on writes. Questions for the user:
- May a platform admin write tenant data at all? If yes: scoped how —
  (a) only via an explicit platform scope / break-glass flag (audited),
  (b) only on resources explicitly marked platform-managed,
  (c) anywhere (platform = superuser)?
- Pilot-2 residual: the admin arms (`manage_buckets`, `manage_byo`,
  `manage_plans`) can currently touch ANOTHER company's buckets/BYO/plans.
  Whatever the answer, those arms need an owner-or-platform rule, not the
  current open access.
- Recommendation (parent's): (a) — platform writes only through an explicit,
  audited platform scope; never silent, never by default. A superuser platform
  breaks the tenant-isolation story the whole C3 program just built.

### 3. Write-strictness harmonization — the three variants in the tree today
Recorded (not harmonized) by pilots 4 and 319:
- **Strict (403, no company → write denied):** pilot-4 groups A+E (16
  modules); pilot-1 groups; 319: cards, library, store (seller/manage).
- **Mismatch-only (403 only on tenant-vs-verified disagreement;
  company-less writes proceed):** pilot-4 groups B+D (17 modules);
  319: billing, delivery_notes, exams, plm, referral, rental, sign,
  sms/social/spreadsheets (reads).
- **Tenant-gate only (`require_tenant`, no company check on writes):**
  pilot-4 group C (8 modules); 319: esg, forums, entitlements (admin),
  kyc/phone (tenant loaders, 404 cross-tenant).

Options:
- **(a) Strict everywhere.** Simplest story, strongest isolation. Cost:
  breaks pilot-4 group D's deliberate self-service writes (applicant
  submit/withdraw, consent grant/withdraw, own notifications) and any
  legitimate company-less flow — each needs an explicit carve-out.
- **(b) Mismatch-only everywhere.** Weakest; company-less writes proceed
  tree-wide. Effectively blesses the loosest variant. Not recommended.
- **(c) Strict by default + named carve-outs (recommended).** Default rule:
  writes require a resolved company and a match, 403 otherwise. Carve-outs
  are explicit, named, and tested: (1) self-service writes (D's list),
  (2) platform-scope writes per decision 2, (3) genuinely tenant-scoped
  writes where the record carries no company field yet (C11 territory —
  these stay tenant-gate-only until C11 lands, and are listed as such).
- **(d) Keep the variants, document only.** Zero code churn, but the tree
  keeps three different answers to "may I write without a company?" —
  the exact inconsistency C3 was meant to end.

Recommendation (parent's): **(c)**. It keeps the strict modules strict,
gives D's self-service flows a legitimate home, and makes every exception
visible instead of accidental.

## What implementing (c) touches
- One shared write-guard helper (or per-module adoption of the existing
  `require_company_match*` family) + a carve-out registry (explicit list,
  not a flag on the caller).
- The pilot verifiers encode today's per-module variant — every verifier
  must be updated to the new rule and re-run.
- Task 320 (read rule) is separate; 315 is the WRITE rule only. The two
  decisions are independent and can land in either order.

## Out of scope for 315
- Read-rule unification (task 320 — separate brief).
- Record-level `tenant_id` stamping (C11 / task 271).
- `inbox.tenant_visible` (product decision, open since task 312).
- Store `digital_caller` raw-header reads (task-317 follow-up).

## Implementation (2026-11-23)

- **C.1 match fix:** `require_company_match_for` family now
  403s `(None, None)` ("A write requires a resolved company
  scope"); same for `require_company_change` /
  `require_company_change_uuid` (explicit clear by a resolved
  caller kept).
- **C.2 writable record checks:**
  `require_record_writable_in_company_scope` (42 files +
  delivery_notes' `require_note_writable_in_company_scope`),
  403 on cross-company AND on global records; 151
  direct mutation-handler call sites switched (the (a) pattern:
  handler mutates the checked record itself or creates a record
  stamped with the caller's company). (b)-pattern handlers
  (load a parent for visibility, write a child) keep the parent
  load on the read rule.
- **C.3 strict gate:** `require_company_for_write(tenant, req)`
  next to each module's `caller_company` def (56 modules); 977
  call sites renamed in routed mutation handlers, except the
  carve-out lists. The gate was briefly applied to the 20
  carve-out-3 modules and then reverted module-wide.
- **C.4 mutation-only helpers flipped:** `transition_course`,
  `transition_event`, `change_exam_status`, `load_editable_sale`,
  `set_digital_product_status`, `move_digital_payout`,
  `require_instance_in_scope`, `require_task_risk_in_scope`,
  `require_grade_in_scope`.
- **C.5 control plane:** `ControlPlaneGuard`
  (`crates/unified-core/src/tenancy.rs:340`) + manual mirror
  `require_control_plane`; guarded: tenants (14), saas_domains
  (7), licensing (29); saas_packages already adopted (8).
  Pilot-2 residual fixed: storage_manager admin arms now use
  `is_platform_admin` (capability AND no resolved tenant
  context); `is_storage_admin` removed; 46 derivations
  switched.
- **Carve-out registry:** in the guard map ("Tasks 315 + 320
  outcomes"): (1) self-service writes, (2) platform-scope
  writes, (3) the 20 tenant-gate-only modules, (4) the
  billing/delivery_notes/exams settings/scan/print lists,
  (5) (b)-pattern parent loads, (6) pre-315 strict idioms.
- **Verifiers:** all 11 pilot group verifiers updated (strict315
  kind for billing / delivery_notes / exams; writable markers
  elsewhere) and re-run with the documented exclusions — 135
  runs, all exit 0 (logs
  `~/workspace/unified-check/c3-verify-20261123/`).
  `c3_guard.py` / `c3_repair_order.py` are now method-aware.
- **Residuals:** documents module skipped (follow-up); shared
  loaders left on the read rule (risk, exams); storage_manager
  create paths need a ControlPlaneGuard-based platform arm (8
  sites); 42 handler tests flagged by a static survey (fix list
  in the guard map); the root bin was never `cargo check`ed
  after the switches (OOM wall) — the coordinator must check it
  before pushing.
