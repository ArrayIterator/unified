# Final inspection — task 104 (code inspection), 2026-11-22

Inspector: final inspection builder. Scope: the tree after module
code-splitting (137 modules), workspace conversion (root + `crates/
unified-core` + `crates/example-hello-module` +
`crates/unified-calculator`), the f64 → i64-minor-units money
migration, and the AI capability registration pass (104/136 in-tree
modules). Nothing was pushed. No `cargo check` / `cargo build` /
`cargo test` ran (a separate agent owns compilation verification),
so every fix below is verified by read-back only.

Note: the tree is being edited concurrently by other agents
(`schema.rs` grew ~500 lines mid-inspection). All edits here use
exact-text anchors and were re-verified by read-back afterwards.

## 1. §10 verdicts (INTEGRATION_NOTES_INTEGRATION2.md §10)

| # | Item | Verdict |
|---|---|---|
| 1 | Entitlements vs licensing diverge | **FIXED.** Subscription leg added to `licensing::check_module_entitlement` (reason `"subscription"`, via the store's `subscription.module_entitlement.check` answerer) and mirrored in `entitlements::effective()` (source kind `subscription`) and `entitled_module_ids` (core modules via owner entitlement; subscription addons included). Non-sellable modules report entitled through the new derived `builtin` source so `effective()` agrees with licensing. Grace stays entitlements-only by design; `effective(ctx: None)` stays stored-rows-only, fail closed. |
| 2 | `certificates` collision (000005 vs 000079) | **FIXED.** 000079 table renamed to `elearning_certificates` (migration up/down + `table!` + `ElearningCertificate`/`NewElearningCertificate` + joinable + join group 6). |
| 3 | `pricing_tiers` collision (000041 vs 000103) | **FIXED.** 000103 table renamed to `pricing_volume_tiers` (migration up/down + `table!` + `PricingVolumeTier`/`NewPricingVolumeTier`). After the fix: 673 unique CREATEs, 0 duplicates, CREATE/DROP sets equal. |
| 4 | Modularity violations V1–V8 | **OPEN.** Spot-verified still present, e.g. provider base URLs in `src/modules/payments/dispatch.rs:68-81`. Needs data-driven refactors. |
| 5 | Tenant-scoping rollout (125/138 not-guarded) | **OPEN.** Largest remaining item; stores not partitioned by tenant anywhere. |
| 6 | Approvals sweep + web3 poll kinds, no listeners | **OPEN.** `scheduler/module.rs:28-31` still documents no consumer; approvals has no listener for `approvals.sweep_expiry_requested` (only the POST route); web3 poll is HTTP-route-only. |
| 7 | `tenant.destroy_due` arm incomplete | **OPEN.** `tenant_destroy/module.rs` listener only lists due requests; the scheduler has no `AppContext` for `process_due_destroys`. |
| 8 | Tenants data-provider for entity `tenant` missing | **OPEN.** `run_export` asks owning modules via `export.requested` (no scope); no module answers for entity `tenant`, so full-tenant exports produce 0 rows and destroy stays export-blocked. Needs an owner for tenant rows (design decision). |
| 9 | `saas_packages.package.modules_requested` no answerer | **OPEN.** `tenants/provisioning.rs:109-113` still documents it; preset side ready, consumer call not. |
| 10 | Projects `sharing.allow_check` missing | **OPEN.** `auditors/state.rs:96` still documents it; project links fail closed. |
| 11 | Whole-store CertStoreSwap needs listener rebuild | **OPEN.** Restart works today; documented in code. |
| 12 | `build_fee_invoice` without pricing validation | **ALREADY FIXED (verified).** `billing/school_fees.rs:310-376` uses `charge.company_id` and calls `validate_invoice_with_pricing`; without a tenant id or a pricing answer the invoice is held as draft with `Err`. |
| 13 | `elearning.enrollment.purchase_requested` creates no order | **OPEN by design.** `elearning/listeners.rs:85-95` documents it: no store listener creates an order; checkout with the course id is the manual path. Needs a course→product link owned by elearning/store. |
| 14 | Store coupon redemption re-check cannot run | **OPEN (partly resolved).** Store money is integer now (0 `f64` in `store.rs`); `coupon_discount` is still local (`store/pricing.rs:338,383`) and the store coupon id still differs from the pricing coupon id. |
| 15 | Subscription renewals (`renew_one`) not gated | **OPEN.** `payments/handlers_subscriptions.rs:57` charges stored tokens with no `HttpRequest`/tenant context by construction (background renewal). |
| 16 | `settings.merge` vcs-only allowlist | **ALREADY FIXED (verified).** `settings/handlers.rs:20-44` accepts `vcs.*` + `auth.*` + `cloudflare`, with tests; refusals are logged, not silent. Merged values bypass `config.validate` by documented decision (trusted internal flags). |
| 17 | Calendar on the control plane (403 by design) | **OPEN.** Needs a policy decision; `calendar/helpers.rs:41-42` documents it. |
| 18 | No `academic.enrollment.lookup` answerer | **OPEN.** No matches tree-wide. |
| 19 | School-fee schedules keyed by academic student id | **OPEN.** `billing/school_fees.rs:529-548` still falls back to `student_id` when academic does not echo `student_user_id`/`user_id`. |
| 20 | Manufacturing reservation quirk | **FIXED.** Reservation status and `stock_unknown` are now per-product (`levels.contains_key`), and the stock-check rows report per-component `stock_status`. Previously any answer flipped every component to `reserved`. |
| 21 | Exams/library + guarded modules: no entitlement check | **OPEN.** By design until grant rows exist; a blocking gate would 403 all traffic today (`entitlements.check_requested` returns entitled=false with no rows). |
| 22 | In-memory stores authoritative almost everywhere | **OPEN.** Diesel persistence still to land where the notes say so. |
| 23 | Billing stores f64 money | **ALREADY FIXED (verified).** 0 `f64` occurrences in `billing.rs`, `store.rs`, `pos.rs`. |
| 24 | Pre-reference backfill (000107 never ran) | **OPEN.** Needs a live PostgreSQL run. |
| 25 | In-memory notes (`saas_packages`, `holidays`) | **OPEN.** Same situation as exams/library (guard rejects unresolved tenants, maps not partitioned). |
| 26 | Stale `down.sql` header note / 4 `up.sql` narratives | **FIXED.** All four narratives rewritten to the post-renumber numbers (000072, 000098, 000099, 000100). |
| 27 | Known un-added audit names | **PARTLY FIXED.** `invoice.created`, `order.paid`, `store.order.completed` added to `AUDITED_EVENTS` (all three verified emitted; payloads carry ids/amounts, never PAN). `document.*`, most `web3.*`, full `quality.*`, `entitlement.*` still un-added — needs an event inventory first. |
| 28 | `create_subscription` has no `require_customer` check | **ALREADY FIXED (verified).** `payments/handlers_subscriptions.rs:269-290` requires non-empty `customer_id` + `require_customer` ownership before any charge. |
| 29 | Chrono-tz not in Cargo.toml | **OPEN.** Calendar honestly 422s on IANA names; adding the dependency cannot be verified without a cargo run. |
| 30 | Holiday sync runs in the HTTP handler | **PARTLY RESOLVED.** Scheduled syncs run in spawned tasks (`holidays/hooks.rs:222,264,334`); manual sync stays inline by documented decision (`handlers_configs.rs`, avoids a double run via the `holidays.sync_requested` listener). |

## 2. Fresh sweep findings

- **CRITICAL — client-controlled identity in two modules.**
  The projects module takes caller identity from query params:
  `viewer_of` (`src/modules/projects/state.rs:208-220`) reads
  `viewer_id` / `viewer_role` from `PaginationParams` filters, and
  `require_capability` / `require_project_access`
  (`state.rs:232-268`) enforce against those values; the module
  contains zero `HttpRequest` uses, so any client can pass
  `viewer_role=admin` (and any `viewer_id`) in the URL. The
  network-tools module does the same via `tool_actor`
  (`src/modules/network_tools/helpers.rs:209-216`), e.g.
  `dns_handler` gates on the query-supplied role. NOT fixed: the
  repair (plumb header identity into every handler) is a
  module-wide rewrite, not a small defect.
- **CRITICAL — analytics module entirely unauthenticated.**
  Zero `HttpRequest` uses in `src/modules/analytics/`; all 27
  handlers (reads and writes, e.g. `POST /sales`,
  `POST /metrics`, `POST /health`) run with no caller check.
  NOT fixed: module-wide rewrite.
- **HIGH — unauthenticated writes needing a permission design.**
  Billing `GET/PUT /settings` (`handlers_invoices.rs`, no
  `HttpRequest`) read/mutate billing settings with no caller
  check, and no billing admin permission exists yet to gate on.
  Store catalog create/update/delete
  (`store/catalog.rs`) likewise run with no caller check.
  NOT fixed: needs the permission model decided first.
- **Cleared by verification (heuristic false positives):**
  app-sync push/pull validate HMAC device tokens
  (`handlers.rs:196,290`); risk reads identity from request
  headers via `viewer_of(&req)`; surveys `submit_response` is
  token-based with expiry; elearning certificate verify is
  public by design.
- No live secrets in code (only a fake `AAAA` EC key in
  `web3/tests.rs:89`); `danger_accept_invalid_certs`: 0 hits;
  `todo!` / `unimplemented!` / `unreachable!`: 0 in code (one
  prose comment mentions `unreachable!()`; the test-helper
  `unreachable!()` in timetracker was replaced with a denying
  no-op arm). Forbidden reference-project name: 0 real hits
  (`OrgChartNode` only). Indonesian words: 0 in code/comments
  (only the legitimate native language name "Bahasa Indonesia"
  in the i18n catalogue, and one test fixture renamed to English).
  Raw SQL outside Diesel: only `mcp/context.rs`, where the SQL
  text is assembled from allowlist constants with bound
  parameters and the role is validated by `validate_role_name`
  at configuration time (`mcp/handlers.rs:133`).

## 3. Structural verification (final tree)

- Modules: 136 `pub mod` in `src/modules/mod.rs`, 137
  `::register(registry);` calls (136 + `unified_calculator`);
  every shim resolves `register` (glob or named re-export from
  `module.rs` / `routes.rs` / `stores.rs` / `state.rs`).
  AI capabilities: 104/136 modules declare `AI_CAPABILITIES`
  (content/curriculum register theirs from `module.rs::init`).
- Workspace greps in `src/`: `crate::core::` / `crate::db::` /
  `crate::http::tls` = 0/0/0.
- Migrations: `crates/unified-core/migrations/` holds
  000001–000108, gapless, no duplicates. CREATE: 673 stmts /
  673 unique / 0 duplicates; DROP sets equal (modulo three
  comment-text false positives). `table!` blocks: 677
  (673 migration tables + 2 migration-less + 2 renamed);
  model structs: 1350 (2 per new table).
- Money: billing/store/pos contain 0 `f64`. Tax + procurement
  dropped 4-decimal precision to cents, half away from zero,
  documented where it matters: `tax/computation.rs:33-34,49-50`
  ("money-to-4-decimals rounding is gone by design: sub-cent
  precision is dropped at the boundary, half away from zero")
  and `procurement/helpers.rs:174-186` (per-line rounding,
  rounded exactly once). Quantities stay `f64` (4-decimal).
  Calculator crate f64 is deliberate (computation inputs/outputs).
- REVIEW.md adapted logic spot-checked and present: session
  `saturating_add` (`auth/session.rs:113`); flash per-kind
  eviction + `spawn_periodic_cleanup` (`auth/flash.rs:7-10`);
  TLS `auto_created` map + `certificate_names` + `remove()`
  touching only dynamic/auto-created (`tls.rs:58,395,432-436`);
  ACME 7–90 day clamp + `ChallengeTokens` Drop guard
  (`http/acme.rs:54-60,195`); hook priorities/once/
  re-entrancy guard (`hooks.rs:303-336`); serde/Diesel error
  classification (`error.rs:109,132`).

## 4. Fixes applied (this inspection)

1. Item 1 — `src/modules/licensing/validation.rs` (subscription
   leg + `has_subscription_entitlement` + tests),
   `src/modules/licensing.rs` (re-export),
   `src/modules/entitlements/constants.rs` (`SOURCE_BUILTIN`,
   deliberately outside `SOURCE_KINDS`),
   `src/modules/entitlements.rs` (re-export),
   `src/modules/entitlements/state.rs` (`effective()` mirror,
   `entitled_module_ids` core+subscription legs + tests).
2. Items 2/3 — migrations 000079/000103 (up/down),
   `crates/unified-core/src/db/schema.rs` (header note, 2
   `table!` blocks, 1 joinable, join group 6),
   `crates/unified-core/src/db/models.rs` (4 structs + section
   comments).
3. Item 20 — `src/modules/manufacturing/handlers_work_orders.rs`
   (per-product `known` in reserve + stock-check rows + test).
4. Item 26 — four `up.sql` narratives rewritten.
5. Item 27 — three financial events added to `AUDITED_EVENTS`.
6. Trivia — `company/store.rs` test fixture renamed to English;
   `timetracker/module.rs` test-helper `unreachable!()` replaced.
7. `TODO.md` — §10 list updated (items 1, 2/3, 12, 16, 20, 26,
   27, 28, 30 marked fixed/verified; auth sweep findings added).

## 5. Tests added (not run — no toolchain in this VM)

- `licensing/validation.rs`: 5 tests (subscription helper
  true/false/no-answerer; gate grants via subscription; gate
  denies with neither license nor subscription).
- `entitlements/state.rs`: 7 tests (non-sellable via `builtin`,
  ctx=None stays locked, core via owner entitlement, addon via
  subscription, addon locked without either, `entitled_module_ids`
  core/free/subscription membership).
- `manufacturing/handlers_work_orders.rs`: 1 test
  (per-product `known`/`unknown` stock status with a
  single-product answer).

## 6. Could not verify

- Compilation of any edit (no cargo/rustc in this VM; a
  separate agent owns `cargo check`). In particular the new
  `table!`/struct pairs, the `SOURCE_BUILTIN` re-export chain,
  and the new tests need a compile run before any push.
- No migration ever ran against a live PostgreSQL; no endpoint
  was live-tested.
- The auth-sweep counts (224 routed handlers without
  `HttpRequest`, 274 with a request but no auth marker) are
  heuristic upper bounds with known false positives
  (extractors, token checks, public-by-design endpoints);
  only the items in §2 were manually verified.
