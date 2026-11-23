# C3 task 315 — ControlPlaneGuard + write-strictness harmonization: decision brief

Status: DRAFT — awaiting the user's pick. Do not implement until the user chooses.

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
