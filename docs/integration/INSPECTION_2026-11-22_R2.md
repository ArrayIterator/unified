# Total Re-Inspection — Round 2 (2026-11-22)

**Inspection window:** 2026-11-22 ~16:26–16:35 UTC (23:26–23:35 WIB).
**Tree:** `/home/hatch/workspace/unified-push` (push-staging tree, no `.git` — no commit hash recordable).
**Repo:** `ArrayIterator/unified`, remote head at dispatch `e05b0a51`.
**Method:** 10 parallel read-only worker subagents, scripted greps/counts only. No file edited, no `cargo check`/`test`/`build` (VM OOM rule), no push. Every number re-measured; earlier reports were read as reference only, never trusted.
**Previous inspection:** `docs/integration/INSPECTION_2026-11-22.md` (~21:03 WIB, 15 criticals C1–C15). Since then: fix pass, refactor program, forums + 9 Odoo-gap modules, icon system, minify pipeline, TS runtime, compile tail (287 RouteInfo.auth literals).

## Baseline — the tree moved during inspection

| Metric | 16:26 UTC | 16:35 UTC (final) |
|---|---|---|
| `pub mod` in `src/modules/mod.rs` | 145 | **146** (`esg` registered 16:30 UTC) |
| Migration dirs in `crates/unified-core/migrations/` | 118 | **120** (`000119_esg`, `000120_referral` landed mid-inspection) |
| `.rs` files (excl. `target/`, `node_modules/`) | ~2,163 | **2,160** (count drifted 2,159–2,177 during the window; sibling agents editing) |
| Rust lines (`src` + `crates`, excl. `target/`) | — | **709,129** |
| `table!` definitions / Diesel structs | — | **675 / 1,350** (verified exact; raw grep 681 includes 6 doc-comment snippets) |

99 `.rs` files were modified in the 60 minutes before inspection began. All findings below are snapshots as of ~16:27–16:35 UTC; HOT files are flagged where judged.

## Executive verdict — DO NOT EXPOSE THE API PUBLICLY (unchanged)

Round 1's bottom line still holds, and round 2 adds two new compile-breakers that did not exist (or were not found) in round 1:

1. **The tree very likely does not compile right now.** Two independent static findings, each alone fatal to a build: a duplicate top-level function in `auth_manager/recovery.rs` (Rust E0428) and an orphaned method chain / parse error in `service_monitor/system.rs:403-407`. Neither was confirmed by a compiler run (OOM rule) — but both are visible in plain source and neither is `cfg`-gated.
2. **C3 tenant isolation is still essentially open.** 0 of 146 modules call `with_tenant_connection`; 116/146 have no tenant edge guard; `can_access_record(None, _) == true` still stands in `files` and `studio`, and `None` is reachable by any JWT-authenticated user with no company mapping.
3. **The role gap is unchanged.** Every verified caller is `role() == "anonymous"` with empty roles; ~119 `require_permission` call sites across 102 files deny everyone, including admins.
4. **Revoked JWTs still pass the global middleware.** Revocation is persisted but only the auth-manager's own verify endpoint checks it.

Verified-fixed since round 1: **C1** (verified-Caller middleware, 287/287 literals, 0 auth-less literals tree-wide), **C2** (middleware + allowlist), **C4** (middleware defaults now `NotFound`), **C10** (licensing upgrade events emitted), **H7** (password-reset backend exists). Everything else is partial or open — see the C1–C15 table.

## C1–C15 status (round 2 re-verification)

| ID | Round 1 | Round 2 verdict | Evidence |
|---|---|---|---|
| C1 self-asserted auth headers | CRITICAL | **FIXED** | `src/http/auth.rs` middleware (priority 925) wraps whole `App`; `Caller` built only from verified HS256 JWT; 0 non-test `X-User-Id`/`X-Role` identity reads in production code; 4,148 `RouteInfo` literals tree-wide, 0 without `auth`; 9 assigned files 287/287 (14 Public, 273 Authenticated, 0 Capability). Shorthand `auth,` literals in plm/kyc/phone/delivery_notes/spreadsheets are migrated — the migration script misreports them (LOW, tooling). |
| C2 unauthenticated mutating endpoints | CRITICAL | **FIXED** | Same middleware; allowlist = 11 hardcoded entries (`src/http/auth.rs:80-115`) + 90 `RouteAuth::Public` literals; everything else 401s. No bypass found; `/ws` 401s without a token; pause/maintenance wrapper is a deny, not a bypass. |
| C3 tenant isolation | CRITICAL | **OPEN (partial)** | See CRITICAL-3 below. Edge guard 29/146 modules; DB guard 0/146; `can_access_record(None,_) == true` in `files/types.rs:257-262` and `studio/auth.rs:196-201`. |
| C4 middleware fail-open defaults | CRITICAL | **FIXED** | `crates/unified-core/src/middleware.rs:580-581`: both policies default `NotFound`; `#[default]` on `NotFound`; `ControlPlane` opt-in only. Caveat: no typed `[saas]` config section — policies are env-var-only (`UNIFIED_SAAS_*_HOST_POLICY`), undocumented in `config.example.yaml`. |
| C5 idempotency check-then-act | CRITICAL | **PARTIAL** | `claim_idempotency()` atomic (DashMap entry) at 7 paths, claimed before the provider call; webhooks claim event-id at receipt. BUT durable claim tables exist as DDL only (migration `000112` header: "The read path does not use these tables yet") — multi-instance double-charge still possible. `create_subscription` first-period charge has no claim/replay (MEDIUM). `void_transaction` has no claim (LOW). |
| C6 webhook partial refunds as FULL | CRITICAL | **PARTIAL** | Amount-less webhook refund → pending row, amount 0, never full; API refund with no amount refunds the remainder; over-refund guard atomic via `REFUND_ADMISSION` mutex. BUT `complete_refund` cannot record the real amount (`CompleteRefundRequest` has no amount field) — a completed refund keeps `amount_minor = 0` forever (MEDIUM). |
| C7 dunning never enforced; `period_end` anchors to now | CRITICAL | **PARTIAL** | Dunning enforced in code; renewal anchoring fixed (`end.max(Utc::now())` at both sites). BUT no default schedule ships (operator must create the scan job) and the expire-due sweep is HTTP-only, not scheduler-wired (no `store.subscription.expire*` kind in `KNOWN_KINDS`). |
| C8 subscription events never emitted | CRITICAL | **PARTIAL** | `subscription.activated` emitted; `POST /api/v1/store/subscriptions/expire-due` sweep emits `subscription.expired`; `store.order.upgraded` + `billing.plan.changed` emitted with listeners. The expire sweep has no scheduler kind (see C7). |
| C9 library fine-payment listeners dead | CRITICAL | **PARTIAL** | Listeners now on `invoice.paid` + `payment.completed`. BUT nothing puts a fine link on an invoice yet, so end-to-end correlation still cannot happen. |
| C10 licensing upgrade listeners dead | CRITICAL | **FIXED** | `store.order.upgraded` + `billing.plan.changed` now emitted (verified with listeners). |
| C11 in-memory DashMap stores authoritative | CRITICAL | **OPEN** | 784 DashMap declarations (round 1) not re-counted; but all 9 newest modules (forums, rental, kyc, plm, sign, delivery_notes, phone, spreadsheets, esg) are in-memory only with zero DB calls — migrations are persistence targets, not live storage. Only 15/146 modules contain any real Diesel/`DbPool` code. Schema hub covers only through migration 000108; 68 tables from 000109–000118 have no Diesel-typed access. |
| C12 modularity (closed registries, hard-coded enums) | CRITICAL | **OPEN — not re-verified in round 2** | No round-2 worker was assigned a full C12 re-audit. Round-1 evidence (156 pub enums, 385 `match!`, closed provider registries) stands unrefuted. Spot evidence that the pattern persists: MCP allowlist/denylist are code constants, not admin-editable; sign duplicates contracts' token machinery with zero imports; `minify_enabled()` dead code. A dedicated re-audit is still owed. |
| C13 auth hardening gaps | CRITICAL | **PARTIAL** | Per-tail status below. Two HIGH residuals: revoked JWTs pass the middleware; duplicate `recovery_code_is_valid` (NEW, CRITICAL-1). |
| C14 entitlements vs licensing partial | CRITICAL | **PARTIAL (fix-pass claim, not re-verified)** | Fix pass introduced one shared `entitled_via_live_sources`. No round-2 worker re-audited entitlement enforcement end-to-end. |
| C15 scheduler kinds with no listeners | CRITICAL | **FIXED (fix-pass claim, not re-verified)** | Fix pass registered listeners for `approvals.sweep_expiry` + `web3.poll`. No round-2 worker re-checked listener registration. |

### C13 tails

| Tail | Status | Evidence |
|---|---|---|
| H1 ephemeral signing-key fallback | FIXED for JWT | `auth_manager/state.rs:131-165` fails loudly on empty/short/placeholder keys; prod config requires ≥32 chars (`config.rs:716-727`). Residual: privacy module still falls back to an ephemeral in-memory anonymization key (`privacy/state.rs:120-142`). |
| H2 in-memory JWT revocation | PARTIAL — HIGH gap | Revocations DB-persisted (`auth_revoked_tokens`, migration `000109`) + DashMap cache hydrated from DB. **Global middleware never checks revocation** (`security/auth.rs:128+` checks signature/expiry/`iss`/`iat`/`nbf` only, no `jti`). Only the auth-manager verify endpoint checks it (`tokens.rs:332`). Logout only clears the cookie. Fixed sub-items: `SessionTokenizer::parse` enforces expiry; JWT TTL server-capped at 1 h. |
| H3 recovery codes plain SHA-256 | Storage FIXED; **NEW compile-breaker** | Codes now Argon2id PHC (`recovery.rs:63-65`). But `recovery_code_is_valid` is defined TWICE (see CRITICAL-1). Also: codes live only in an in-memory DashMap — lost on restart, no DB table. |
| H4 TOTP secrets plaintext | FIXED at rest, with caveats | New secrets encrypted (`totp.rs:214-227`, custom HMAC-CTR + Encrypt-then-MAC, `src/auth/totp.rs:100-165`); key derived from app secrets, not a separate KMS/env key. Pre-encryption plaintext rows still accepted (`totp.rs:148-151`, re-encrypted on next login). Pending TOTP secrets in-memory only. |
| H5 legacy unpeppered API-key hashes | PARTIAL | New keys peppered HMAC `hmac1:` (`apikeys/service.rs:149-171`); legacy plain-SHA-256 still accepted until sunset **2027-03-01** (`:59`, `:269-295`). If no pepper is configured, `key_pepper()` returns an empty string (`:117-127`) — HMAC with an empty key. |
| H6 cookie auth without CSRF | PARTIAL | CSRF enforcement in only 3 modules: security, auth-manager, dashboards. All other modules' cookie-auth writes unchecked server-side (client TS sends the token everywhere — see Frontend). Mitigation: `HttpOnly; SameSite=Lax` session cookie. |
| H7 password-reset backend | FIXED | `POST /forgot-password` + reset handlers in `auth_manager/reset.rs` (60-min TTL, 5-min cooldown, single-use, sibling invalidation); HMAC-SHA256 token hashes only; `password_reset_tokens` table in migration `000109`. Email via `mail.send_requested` hook, 503 if no handler. Migration never executed against live PostgreSQL. |

## CRITICAL findings

### CRITICAL-1 — Duplicate `recovery_code_is_valid` (E0428): auth-manager likely uncompilable
`src/modules/auth_manager/recovery.rs:80` and `:135` define the same top-level `pub(crate) fn recovery_code_is_valid` twice in one compiled module (`src/modules/auth_manager.rs:24` includes the file). Two identical top-level definitions is Rust E0428. The second copy is additionally broken: it re-hashes the candidate and compares PHC strings with `constant_time_eq`, which has no import in the file, and can never match a fresh-salted Argon2id hash. The public `POST /recovery-codes/verify` endpoint calls it (`recovery.rs:277`). **Coordinator re-verified by direct grep: both definitions present at lines 80 and 135 (plus a test fn at 433 with a different name).** Static evidence only — no compiler run (OOM rule). If confirmed, the whole binary fails to build.

### CRITICAL-2 — Parse error in `service_monitor/system.rs:403-407`: CONFIRMED (minify agent's report was correct)
```rust
let params = query.into_inner();
    .config
    .read()
    .expect("config lock poisoned")
    .clone();
```
The statement ends at the semicolon; the `.config…` chain is an orphaned expression beginning with `.` — invalid Rust. **Coordinator re-verified by direct read (lines 398–412).** The same `let params = query.into_inner();` at line 250 is followed by correct code, so this is a localized breakage in `list_services`, not a file-wide pattern. Static evidence only — no compiler run. If confirmed, the crate fails to parse.

### CRITICAL-3 — C3 tenant isolation still open (fail-open `None` scope)
- `with_tenant_connection`: **0 real call sites in any module.** 38 occurrences tree-wide = 1 definition (`tenancy.rs:392`) + 1 real call site in the scripting SDK (`crates/unified-scripting/src/sdk_db.rs:354`, HOT) + 36 comment/doc mentions. The 29 occurrences in `src/modules/` are all doc comments, most stating "has no call site here yet". Round 1's "29 call sites" were comment mentions, not calls.
- Edge guard (`from_http_request` / `require_tenant(` in non-comment code): **29 modules guarded, 116 unguarded** (1 HOT: `esg`). Unguarded modules hold 1,231 of 1,789 module `.rs` files (69%). Largest unguarded: storage_manager (31 files), projects (30), curriculum (23), timetracker (22), web3 (20).
- `can_access_record(None, _) == true` still present in `src/modules/files/types.rs:257-262` and `src/modules/studio/auth.rs:196-201`, with tests asserting it. `None` is reachable by **any JWT-authenticated user with no company mapping anywhere**: `auth::Caller` carries no company id (`crates/unified-core/src/caller.rs:116` always `None`); `files/types.rs:225` consults only the always-`None` JWT company + a hook; `studio/auth.rs:143` adds tenant-context company + `X-Company-Id`, but an empty company still yields `None`. Result: such a user is a de-facto platform caller with read access to all companies' files/studio records (and write access in studio). Mitigations: `require_caller` enforces a verified JWT; studio requires a resolved tenant first. Fix design exists (replace `Option<&str>` with `CallerScope::{Company, Platform}`); `files/types.rs` was skipped as hot in the fix pass.
- Edge guard does not partition stores even where present: `exams/handlers_banks.rs` discards the resolved tenant (`let _tenant = require_tenant(&req)?;`); `library/auth.rs:58-72` admits unfiltered reads when company scope is absent.
- New: **nil-UUID gap** — the nil check exists only in `with_tenant_connection` (`tenancy.rs:397-405`). `run_tenant_migrations`, `schema_exists`, `drop_tenant_schema` do not check nil; `schema_name(Uuid::nil())` = `tenant_000…0` passes the regex, so provisioning with a nil id would CREATE and migrate a bogus empty schema.
- Destroy-after-grace has no working scheduler path: the scheduler arm only emits `tenant.destroy_due` (`scheduler/service.rs:282-290`); the handler only lists due requests (`tenant_destroy/module.rs:205-219`); zero code enqueues the event; `process_due_destroys` needs an `AppContext` the dispatcher lacks. Only manual `POST /destroy-requests/process-due` / `…/execute` can run the drop.
- Control-plane API routes outside the 6 blocked path prefixes (e.g. `/api/v1/tenants/*`, incl. destroy endpoints) are not path-blocked on tenant hosts and depend on JWT + capability checks alone; zero handlers extract `ControlPlaneGuard`.
- Verified sound: schema-name helper (regex `^tenant_[0-9a-f]{32}$`, transaction-scoped `SET LOCAL search_path`), tenant registry in `public` (migration `000100`, `CHECK` constraint on `schema_name`), no `public.`-qualified business SQL anywhere.

## HIGH findings

### HIGH-1 — Role gap denies every role-gated check (task 279, still open)
`crates/unified-core/src/caller.rs:110-125` sets `roles: Vec::new()`; `role()` returns `"anonymous"` for verified callers too. No server-side user→roles lookup exists in the middleware/request path (a `user_roles` DB lookup exists in `users/hooks.rs:310-315` but serves user-management endpoints only). The `role.check` hook handler (`roles/module.rs:84`) answers from the role store, but callers pass `role="anonymous"` and no `anonymous` role is seeded (7 defaults: admin/editor/author/member/customer/reseller/finance) → `allowed=false`. Scale: `require_permission` 119 call sites / 102 files; `check_capability` 328 hits; `can_moderate`/`can_manage` 222 hits / 44 files. Concrete: forums moderator gates (hidden-post visibility, poll create/close, pin/lock/solve, report review), gamification manager gates. Author-based checks still work (actor = verified `user_id`). Only 2 scope-based handler enforcements exist in the whole tree (scripting routes, `has_scope("scripting:manage")`); `RouteAuth::Capability` is otherwise unused.

### HIGH-2 — Revoked JWTs still pass the global middleware
See C13/H2 above. Revocation is persisted + hydrated, but `src/http/auth.rs` / `security/auth.rs:128+` never consult it. A revoked token works on every API route until expiry.

### HIGH-3 — Tenant fail-open scope (files/studio)
See CRITICAL-3. Counted separately here because it is independently exploitable without the wider C3 program: any company-less JWT user reads all companies' files.

### HIGH-4 — Durable idempotency tables unused (multi-instance double-charge still possible)
`claim_idempotency()` is atomic per-process (DashMap) at 7 paths, all claiming before the provider call; webhooks claim event-ids at receipt. But migration `000112`'s claim tables are DDL-only — the read path does not use them, and the migration has never run against live PostgreSQL. `create_subscription`'s first-period charge has no claim/replay at all (`handlers_subscriptions.rs:490-505`).

### HIGH-5 — README is stale on every headline number (task 249)
`README.md` claims 142 modules vs **146** `pub mod` (missing: `delivery_notes`, `spreadsheets`, `phone` — 0 mentions each; all three `available_as_product() = true`); "50 of the 137 modules sellable" vs measured **58 of 145** (method: 73 overrides; 58 literal `true` + `projects` const `true`; test doubles excluded); "108 Diesel migrations" vs **120** dirs, 000001–000120; "645,165 Rust lines" vs **709,129** measured; round-2 list says "65 modules below" but names 67 (66 real modules after excluding the `calculator` crate). "675 tables (1,350 Diesel structs)" **verified exact**. No External modules section — the 5 `crates/external-*` crates are never named.

## MEDIUM findings

- **M-1 Migrations: schema hub lags the migrations.** 118 dirs at worker time (120 now), 000001–000120, zero gaps, zero dups, every dir exactly `up.sql` + `down.sql`. **741 `CREATE TABLE` in migrations vs 675 `table!` in schema — difference exactly 68 tables from 000109–000118** (now 000109–000120): no Diesel-typed access for forums/rental/PLM/Sign/KYC/phone/delivery-notes/spreadsheets/auth-revocation/payment-claims tables. 2 schema tables have no migration (`reference_id_settings`, `tenant_destroy_requests`). 34 `CREATE TABLE` without `IF NOT EXISTS` in 6 older migrations. None of the newest migrations ever ran against live PostgreSQL (headers say so). New migrations 108–118: 69 tables; `tenant_id` on all module tables except 108/109/112 (global auth/payment keys, defensible); up/down table sets equal; no `DROP TABLE` in any up.sql.
- **M-2 TS runtime `db.query` WITH-CTE write bypass.** `unified-scripting` (3,361 lines + smoke test) + `unified-ts` (272 lines) + `unified.d.ts` (193 lines, canonical, no root copy) + `packages/unified-sdk-types` (byte-identical, in sync) + example-ts module are real and wired (`register_all` → `ScriptingModule` → routes `/api/v1/ts/...`). QuickJS via rquickjs 0.14 (no V8/deno/boa); OXC 0.159.0 transpile; per-module isolated contexts (64 MiB heap, 512 KiB stack, 5 s timeout); SSRF-safe http; storage scoping confirmed. **But the SELECT-only check looks at the first keyword only — a `WITH …` data-modifying CTE (e.g. `WITH d AS (DELETE …) SELECT …`) passes the check and PostgreSQL allows it inside the wrapper SELECT.** Also: 1000-row cap enforced after load; `db.kv` in-memory only; scripting settings not in `config.example.yaml`.
- **M-3 New modules: all in-memory, two duplications.** 9 new modules (forums, rental, kyc, plm, sign, delivery_notes, phone, spreadsheets, esg): 337 routes, 71 tables, all `available_as_product() = true`, none uses `RouteAuth::Capability`, 0 `todo!`/`unimplemented!` — but **all in-memory only** (DashMap authoritative, zero DB calls). Forums duplicates community's forum engine (14 parallel `forum_*` tables vs community's 9; zero imports of community; split justified in comments only: public-facing, SEO slugs, 2-level nesting, polls, solved/best-answer, reputation). Sign duplicates contracts' token machinery (same SHA-256 `hash_value` + 48-char token generator, zero `contracts::` imports). Delivery-notes ↔ logistics verification hook query is dead (`logistics.shipment.lookup` has no handler — every shipment link recorded `unverified`). KYC `product_category` string `"compliance"` collides with the compliance module name (cosmetic). Inventory still scattered, no dedicated module. 12 Public routes across forums/rental/sign/spreadsheets rely on the API Manager default-deny gate, which the worker did not verify.
- **M-4 Minify: claims hold, with latent risks + dead code.** `src/themes/assets/minify.rs` (1,101 lines, 8 tests — parent re-verified 8/8 standalone rustc pass) + wiring in `assets.rs`/`server.rs`/`config.rs`: release-minified / debug-unminified **holds with qualification** — the decision is mode-based, not build-profile-based (`config.assets.minify.unwrap_or(!config.is_debug())`; a release binary with mode `development` serves unminified). Fingerprint hashes the served bytes — holds. Dependency-free — holds. Measured CSS −22.8%, JS −35.9% (on `.ts` sources as proxy). **Latent: CSS descendant-combinator space dropped before `:`, `[`, `#`, `*`** (0 occurrences in shipped CSS); JS non-ASCII identifiers can merge (`café in x` → `caféin x`); regex after `)`/`]` read as division (documented); `{` after `;`/`:` misclassified as Object. **`minify_enabled()` is dead code — zero call sites.** Doc drift: "three stylesheets, ~52 KiB" is actually 4 CSS files, 56,452 bytes. Wiring is type-untested (needs Colab/CI cargo check). The agent chose synchronous minification at startup over the user's "background" ask (documented: fingerprint correctness).
- **M-5 HBS templates exist but nothing serves them.** 58 `.hbs` files under `templates/default/` (36 list/detail for 18 modules) + 22 under `themes/editorial/` (second theme, no module templates). `render_module_template` / `register_module_template` have **zero production callers** — engine definitions + tests only. 127 of 146 modules have no list/detail templates. No route serves them.
- **M-6 Server CSRF is opt-in per handler, not global.** Client side is fully wired: all 6 TS files (3,758 lines) fetch the token from `GET /api/v1/security/csrf-token` and attach `X-CSRF-Token` via wrappers; no raw mutation fetch exists in TS. Server side: no CSRF middleware in `src/http/server.rs`; only 8 files tree-wide call a CSRF-enforcing write helper (security, auth-manager, dashboards reseller/view-as). Concrete gap: the notifications mark-read `POST` the dashboard sends a token for is not validated server-side (`src/modules/notifications/` has no CSRF handling). Three parallel CSRF-check implementations (`dashboards/auth.rs:190-213`, `security/auth.rs:317-355`, `auth_manager/login.rs:322`). `tsc --noEmit` status not run by the worker; `.js` template references verified intentional (in-RAM compiled-scripts map, zero `.js` files in repo).
- **M-7 MCP privacy: scrubbing real, allowlist narrow, config not admin-editable.** `DENYLISTED_COLUMNS` = 11 fragments verbatim: `"password", "secret", "token", "credential", "key", "private_key", "api_key", "key_hash", "license_key", "totp", "salt"` (case-insensitive substring; false positives like `tokenized`, `monkey` deliberate). Gaps: no fragment matches `passwd`, `pwd`, bare `hash`/`auth`/`session`/`otp`/`bearer`/`pin`/`cert`; scrub is key-name-only (secrets in string values under innocent keys pass through). `READ_ONLY_TABLES` = 5 tables only: companies, company_sites, store_products, store_orders (no `user_id`), store_order_items. Secret tables confirmed absent: users, licenses, store_entitlements, vcs_provider_settings. No unscrubbed MCP data path found; `db.query` inside `SET TRANSACTION READ ONLY`. **Allowlist/denylist are code constants only, NOT admin-editable.** KYC MCP source unreachable (dead code, fail-closed): handler demands `tenant_id` the fetcher never sends, and `kyc` is not in `CONTEXT_SOURCES`. No module answers any of the 10 catalogued `mcp.context.*` sources today (only DB fallback + `site` serve data).
- **M-8 Money: f64 split verified; refund amount recording gap.** `*_percent_bp: i64` + `*_fixed_minor: i64`; zero f64 in cards/pricing; wire format integer minor units. Remaining f64 are rates, day ratios, display/serialization, boundary conversions; one float round-trip on the internal event bus for proration. `complete_refund` cannot record a real amount (see C6). Several stale comments contradict current code.
- **M-9 Nil-UUID guard missing outside `with_tenant_connection`.** See CRITICAL-3.
- **M-10 Destroy-after-grace has no scheduler path.** See CRITICAL-3.
- **M-11 Recovery codes in-memory only; legacy API keys until 2027-03-01; plaintext TOTP rows accepted.** See C13 tails.

## LOW findings

- **L-1** `scripts/migrate_routeinfo_auth.py` cannot parse shorthand `auth,` literals — reported 29 phantom insertions (plm) + 1 error (auth.rs test helper); both are migrated. Treat those outputs as noise on re-runs.
- **L-2** Stale header-auth tests and doc comments in per-module helpers (e.g. `webhooks/helpers.rs:1-7,74-99`, `vcs/helpers.rs:146-184`, `site_builder/helpers.rs:236-266`) assert the old self-asserted-header behaviour and would fail if run.
- **L-3** Ephemeral-key fallback survives in the privacy module (`privacy/state.rs:134-142`) — anonymization key only, not JWT signing.
- **L-4** Library "Public catalog search" is `Authenticated` (`library/module.rs:447`) despite its description — confirm intent (fail-closed either way).
- **L-5** C4 fixed in code but no typed `[saas]` config section; policies env-only, undocumented in example config.
- **L-6** Migration `000100` header misstates the schema-name format ("uuidv7 with hyphens as underscores" — code builds 32 hex chars, no underscores).
- **L-7** `minify_enabled()` dead code (see M-4); doc drift on stylesheet count/size.
- **L-8** Icons: the icons worker (16:27–16:29 UTC) found 148 icons, 3 newest modules (delivery_notes, phone, spreadsheets) without icons, and stale "142 modules" docs. **Superseded during this inspection:** the icons-fix coordinator completed at ~23:32 WIB (16:32 UTC) — all 150 icons redrawn fill-only, sprite rebuilt, 151 files / 151 symbols / 151 unique `module-icon-*` ids, 145/146 modules covered (only `esg` falls back to the placeholder by design), AI icon = brain + nodes (no Gemini sparkle), animation on 6 icons only, grayscale via CSS vars. Parent-verified from both render grids (color + grayscale, zero black blobs). No push yet — awaiting user approval of the renders. The worker's icon findings are therefore historical as of this report.
- **L-9** TS runtime LOWs: 1000-row cap enforced after load; `;` rejection over-strict; stale dispatcher-auth comment; `db.kv` in-memory only.

## Counts (re-measured)

- Modules: **146** `pub mod` in `src/modules/mod.rs` (145 at dispatch, `esg` added 16:30 UTC; `referral` migration `000120` landed by 16:35 UTC).
- Migrations: **120 dirs**, 000001–000120, zero gaps, zero dups (single location `crates/unified-core/migrations/`).
- Rust: **2,160 `.rs` files, 709,129 lines** (`src` + `crates`, excl. `target/`); 675 `table!` / 1,350 Diesel structs; 741 `CREATE TABLE` in migrations.
- Routes: **4,148 `RouteInfo` literals** tree-wide (script parser), 0 without `auth`; 90 `RouteAuth::Public`, 2 `RouteAuth::Capability` (both `scripting:manage`); 9 assigned files 287/287.
- HBS templates: 58 in `templates/default/` + 22 in `themes/editorial/` = 80; 36 list/detail for 18 modules.
- TS dashboard: 6 files, 3,758 lines; 0 `.js` files in repo outside `node_modules/`/`target/`.
- Icons: 151 files / 151 symbols post-fix (see L-8).
- Sellable modules: **58 of 146** (worker measured 58 of 145 before `esg` landed).

## What could not be verified

- **Compilation.** No `cargo check`/`test`/`build` anywhere (VM OOM rule). CRITICAL-1 and CRITICAL-2 are strong static evidence (duplicate top-level fn; orphaned method chain — neither `cfg`-gated) but have no compiler verdict. The root `unified` binary previously reached 0 error diagnostics in 4 consecutive analyses but was OOM-killed before exit 0 every time; the fix pass then added the required `RouteInfo.auth` field and the compile tail closed the 287 literals by literal-count only.
- **Live behaviour.** No running server, no live PostgreSQL, no live requests. Middleware enforcement, revocation, role denial, CSRF exposure, tenant resolution, and every migration were verified by code reading only. No migration has ever run against a live PostgreSQL; no endpoint has been live-tested.
- **Per-route audit.** The CSRF scope conclusion rests on a repo-wide `csrf` grep, not an audit of every one of the ~2,344 mutation route registrations. Handler-level guarding of every control-plane route outside the 6 blocked prefixes was not audited.
- **C12 / C14 / C15.** No round-2 worker re-audited C12 (modularity) end-to-end; C14/C15 statuses above are the fix pass's claims, not independent re-verification.
- **HOT-file drift.** 99 `.rs` files were modified within 60 minutes of inspection; `mod.rs`, `server.rs`, 7 of the 9 RouteInfo files, `dashboard.ts`, `README.md`, and `crates/unified-scripting/src/sdk_db.rs` were all hot. Counts are snapshots; the tree was still changing when this report was written (module 146, migration 120).
- **Gateway proxy auth.** The gateway is feature-gated and not wired into `server.rs`; whether its proxy paths pass through `AuthMiddleware` was not traced.
- **Tests.** 424 test-helper header literals remain (test helpers will 401 until rewritten to insert `auth::Caller`); several `#[cfg(test)]` tests assert the old header-auth behaviour and would fail if run. The 4 chunk-spool tests, forums' 17 unit tests, icons' 6 unit tests, and minify's 8 tests (parent re-verified standalone) remain unrun in-tree.

## Worker reports

Full per-area reports (verbatim substance) are kept outside the repo tree at `~/workspace/unified-inspection-r2/workers/`:
`01-migrations.md`, `02-mcp-privacy.md`, `03-icons.md`, `04-money.md`, `05-ts-runtime.md`, `06-new-modules.md`, `07-minify.md`, `08-readme-frontend.md`, `09-tenant.md`, `10-auth.md`.

## Standing caveat

Compilation was historically never fully verified for this project. `cargo check -p unified-core` exits 0 and each `unified-db-*` crate checks clean in-VM (after the DB split); the root `unified` bin reached 0 error diagnostics but was OOM-killed before exit 0; the fix pass and compile tail then changed the tree further (verified by literal-count only). Batch 11's security fixes are code-reviewed, NOT penetration-tested. Riskiest unverified surfaces include Pingora 0.4 APIs, hand-rolled SigV4 (storage + SES), Diesel `PgNumeric`, the Dispatcher Transform machinery, and instant-acme 0.8 shapes.
