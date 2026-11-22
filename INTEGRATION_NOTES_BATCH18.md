# INTEGRATION NOTES — BATCH 18 (Credit Card Validation + Payment Reference IDs)

Coordinator-verified by read-back. Nothing pushed. Remote head untouched (`7cf6853c`).

Batch 18 totals (verified on disk): **2 new files, 4 extended files, 1 migration,
2 tables, 11 new routes in cards.rs (+2 reference-settings route pairs in
store/billing), ~4,700 new/changed Rust lines.** Tree at verification:
138 `.rs` files in `src/modules/`, 412,829 Rust lines under `src/modules/`,
106 migration dirs.

## 1. What was built

| Item | File | Lines | Routes | Migration | Tables |
|---|---|---|---|---|---|
| Card validation | `src/modules/cards.rs` (new) | 2,535 | 11 | `000099_cards` | 2 |
| Reference-id generator | `src/core/reference_id.rs` (new) + `src/core/mod.rs` | 699 | — | — | — |
| Store wiring | `src/modules/store.rs` (+154) | 8,579 | +2 | — | — |
| Billing wiring | `src/modules/billing.rs` (+184) | 8,395 | +2 | — | — |
| Gateway wiring | `src/modules/payments.rs` (+~1,117 vs pre-batch 10,021) | 11,138 | 0 new (1 pending, S7) | — | — |
| Settings emit fix | `src/modules/settings.rs` (+29) | 396 | — | — | — |
| Webhook events | `src/modules/webhooks.rs` (+9) | 1,318 | — | — | — |

### 1a. Card validation (B1)
- Luhn checksum (`luhn_checksum` / `luhn_valid`), normalisation (strip
  spaces/dashes, reject any other non-digit), masking helper (first 6 +
  last 4) used in every response/log/hook payload. **Never stores a PAN or
  CVV** — only admin custom rules + daily validation counts are stored.
- Brand detection from IIN tables adapted from GatePay-CreditCard
  (MIT, IIN data from Wikipedia). **Repo fetch SUCCEEDED for Elo +
  HiperCard**: Elo verified against the actual factory source — 71
  six-digit prefixes (504175, 506699–506778 subset, 509000–509882 subset,
  636297, 636368; lengths [16]); HiperCard verified exact match
  (384100, 384140, 384160, 606282, 637095, 637568, 637599, 637609, 637612;
  [16]). All other 27 brands use the verified Wikipedia IIN table.
  29 brands total. GPN disabled by default (same as the source library).
- Matching precedence (deterministic, documented in code): longest
  matching prefix wins, then specificity tie-break (Visa Electron > Visa,
  Maestro UK > Maestro > Switch on 6759, Mastercard > Diners Club on 55,
  Discover > RuPay/Troy on bare 65 — ambiguity documented in code).
- Validation order: format → generic length (12–19) → brand →
  `brand_disabled` → `test_number_rejected` (live mode) → brand length →
  `invalid_luhn` → `invalid_expiry` (MM/YY or MM/YYYY, not past, ≤ current
  year + 20 default, configurable 1–50) → `invalid_cvv` (Amex 4, others 3).
  One generic reason code per failure; response names `matched_rule`
  (`standard:<id>` / `custom:<uuid>` / `none`). Extra code added by B1
  beyond the seven specified: `invalid_format` (illegal characters in the
  PAN) — leaks nothing; coordinator confirms keeping it (mislabeling it
  `invalid_length` would be wrong).
- Admin custom rules (tenant-scoped CRUD): name, IIN prefixes (1–8
  digits) / ranges (equal-length digit strings, start ≤ end), allowed
  lengths 12–19, CVV 3/4, optional `overrides_standard_brand` (must be a
  known standard brand; replaces that brand's lengths/CVV/Luhn when it
  matches). Evaluated after the standard tables unless overriding.
  Disabled rules never match; cross-tenant ids read as 404.
- Test-number mode: the 6 well-known test PANs recognised; rejected in
  live (default), allowed in sandbox (per-tenant setting).
- Hooks: answers `cards.validate_requested` (tenant_id required,
  fail-closed when missing; number passed in event, never stored) and
  emits `cards.validated` with masked PAN + result. Bus attached in
  `register_hooks` (verified on disk:
  `bus.register(CARDS_VALIDATE_REQUESTED, "cards", handler)`).
- Routes (11, parity verified: 11 `.route(` == 11 `RouteInfo`):
  POST `/api/v1/cards/validate`, GET `/api/v1/cards/brands`,
  GET `/api/v1/cards/brands/{id}`, GET/PUT `/api/v1/cards/settings`,
  GET/POST `/api/v1/cards/custom-rules`,
  GET/PUT/DELETE `/api/v1/cards/custom-rules/{id}`,
  GET `/api/v1/cards/counts`. All capability-checked
  (`cards:read` / `cards:validate` / `cards:manage`, fail-closed),
  tenant context via `tenancy::TenantContext::from_http_request`
  (fail-closed 401, never falls back to `public`). Module declares
  `available_as_product = false` (infrastructure).
- 7 in-module unit tests (Luhn hand-check, masking, normalisation,
  precedence, validation flow/test numbers, expiry, custom override).

### 1b. Reference-id generator (B2)
- `src/core/reference_id.rs` (699 lines, 8 unit tests) + `pub mod
  reference_id;` in `src/core/mod.rs` (verified present). Pure,
  dependency-light: only `rand` 0.9, `chrono`, `serde`, `std::time` — no
  new dependency, no regex (manual validation).
- Algorithm exactly as specified: `{PREFIX 4}-{12 hex timestamp}-{12 hex
  random}` = 30 chars, e.g. `ORDR-019d43d20eb8-6a5a7925dfb5`. `format_prefix`
  (trim, uppercase, ASCII-alnum only, truncate 4, pad `X`) — verified by
  read-back. Timestamp = ms since epoch `& 0xFFFFFFFFFFFF`, zero-padded
  lowercase hex; random = 6 CSPRNG bytes → 12 hex; 96 bits entropy.
- Functions: `format_prefix`, `generate`, `generate_with_prefix`,
  `generate_unique` / `generate_unique_with_prefix` (existence-check
  closure, retry generation once on conflict; second conflict must fail
  the insert, never overwrite), `is_valid` (exact equivalent of
  `^[A-Z0-9]{4}-[0-9a-f]{12}-[0-9a-f]{12}$`, verified by read-back),
  `parse` → `ReferenceOrderId { prefix, timestamp_ms, random_hex,
  created_at }`, `get_timestamp`, `timestamp_hex`, `timestamp_ms_now`,
  `get_time_range_pattern` → `TimeRangePattern { prefix, from_hex, to_hex,
  like_pattern }` + `from_bound()` / `to_bound()` for
  `reference >= 'ORDR-<from>' AND reference < 'ORDR-<to>' AND reference
  LIKE 'ORDR-%'`.
- `ReferenceIdSettings { default_prefix ("ORDR"), product_prefixes,
  max_total_length }` with `prefix_for_product` (id → slug → default),
  `set_product_prefix` / `remove_product_prefix`, `validate()`,
  `sanitized()` (cap clamped to 30–64). Events `reference.generated` /
  `reference.settings_changed`. Module docs carry the migration note:
  changing prefixes never breaks validation of already-issued IDs.
- Wiring: **store** (+154): `Order` gains immutable `reference`
  (`#[serde(default)]`, verified at store.rs:440); issued once in
  `checkout` (single-product → that product's prefix, mixed → default);
  internal `id` stays a UUID; reference travels in `order.created` /
  `payment.initiated`; `GET/PUT /api/v1/store/reference-settings`
  (capabilities `store.reference:read` / `store.reference:manage`).
  **Billing** (+184): `Invoice` gains `reference` (verified at
  billing.rs:144); sequential `INV-000001` document number untouched;
  all 6 construction sites wired (order reuse when valid, else fresh);
  `GET/PUT /api/v1/billing/reference-settings`. **Payments** (~+100 of
  B2's work): `ChargeInput.reference_id`; all 9 literals set it;
  `charge_reference()` resolution (threaded reference → valid `order_id`
  → fresh generate; a store UUID is never sent to a provider); all ad-hoc
  fallbacks replaced (`pay-{uuid}`, `ord-{uuid}`, `txn-{uuid}`,
  `payout-{uuid}`, `subscription-{uuid}`, `bt-{uuid}` → helper with
  `PYOT` / `SUBS` / `BTRF` / `RFND` / `TXNS` prefixes).
  **Licensing untouched** (correct — consumes `order_id` from events
  only, never creates a reference).
- Grep list (for the integrator; NOT silently rewritten): internal UUIDs
  kept deliberately (primary keys, not public references); sequential
  document numbers kept (`INV-`, `CN-`, `WO-`); `connection-test-{uuid}`
  probes; user-supplied bank-transfer `reference_number`; POS `sale.id`
  (UUID) passed as `order_id` in refund events (POS sales have no payment
  reference yet); Stripe metadata / PayPal `custom_id` / 2Checkout
  `merchantTransactionId` / custom-adapter JSON still echo raw `order_id`
  (they pick up the new format automatically once callers pass the store
  reference). **Zero remaining** `pay-` / `ord-` / `txn-` / `payout-` /
  `subscription-` / `bt-` / `bt-refund-` prefixed-UUID fallbacks in `src/`.

### 1c. Gateway wiring (B3)
- `src/modules/payments.rs` Batch 18 section lines 199–1156 (~958 lines:
  event/reason/capability constants, `CardDetailsInput`,
  `mask_pan` / `digits_only`, `format_prefix` re-export (duplicate
  implementation deleted — now a pure re-export of
  `reference_id::format_prefix`), settings definitions + effective store,
  `config.validate` + `settings.changed` handlers,
  `require_cards_capability`, `request_card_validation` gate at line 907,
  `get_cards_settings` handler at line 1135), plus `card:
  Option<CardDetailsInput>` on all 6 checkout request types and
  `PaymentIntent.payment_method_id`.
- Gate contract (reconciled with B1's landed code, NOT the assumed one):
  `cards.validate_requested` payload carries `mode` (`full` / `method` /
  `claim`), `number` (digits-only), `expiry` (`MM/YYYY`), `cvv`,
  `masked_pan`, `brand`, `cvv_present` / `cvv_length` /
  `cvv_digits_only`, `claimed_brand` / `claimed_last4`, `test_mode`,
  `gpn_enabled`, `enabled_brands`, `expiry_upper_bound_years`,
  `tenant_id`, `schema_name`, `customer_id`, `connection_id`, `provider`,
  `order_id`, `intent_id`. Opaque token alone → no gate (`Ok(None)`):
  the token came from upstream PCI-compliant tokenization. The three
  sensitive values travel on the in-process bus only — B1's
  `validate_card` is pure and never stores/logs them; the request event
  is never audited, persisted, or forwarded.
- Gate order (fail closed): no tenant context → 401/403; no handler on
  the event → 503 `cards_unavailable`; silent answer / no boolean `valid`
  → 503; `valid:false` → 422 with metadata `{reason, mode, masked_pan?,
  brand?}`; reason outside the allowlist, or B1's `missing_tenant_context`
  refusal → 503 (never a guessed reason). No `payment.failed` for a
  validation rejection (no transaction exists).
- Answer contract (B1): `result` set on the payload AND `add_result`ed;
  gate reads `result.valid` first, then the added result. Failure reason
  is `reason_codes[0]` (exactly one code per failure); brand is
  `brand_name` (fallback `brand_id`); masked PAN from the answer
  (empty string filtered out). B1's PAN range is **12–19** (not the 13–19
  in the old `looks_like_pan` guard).
- `cards.validated` emitted by B1 per answered validation (HTTP or hook):
  `{tenant_id, masked_pan, brand_id, brand_name, matched_rule, valid,
  reason_codes, is_test_number}` — never the full number, expiry, or CVV.
  The gateway does NOT emit it (single emit site).
- Event names corrected: B2's event is `reference.generated` (NOT
  `reference_id.generated`); settings change is
  `reference.settings_changed` (emitted from billing.rs:1434,
  store.rs:1344).
- **Settings fix (B3, verified on disk)**: `settings.rs` now emits
  `settings.changed` after successful PUT and DELETE (lines 311, 338) —
  B2 found the emission missing; B3 fixed it. One real would-be compile
  error caught by hand in B3's own edit (partial move of `record` — fixed
  with `.clone()`).
- **Hook registration fix (B2, verified on disk)**: both cards/reference
  settings handlers registered in `PaymentsModule::register_hooks`
  (payments.rs:11030, 11035) — they were defined but never registered.
  Cap semantics aligned to 30–64.
- **CORRECTION to B2's report**: the ~950 lines B2 attributed to "B1
  landing in payments.rs" are B3's Batch 18 section (lines 199–1156,
  marker at line 200). B1 never edited payments.rs. B2's own section is
  lines 1765–1827 ("Batch 18: payment reference ids").
- Grep sweep (B3): existing card-validation spots — `payments.rs`
  `looks_like_pan` + `validate_token` (PAN-rejection guard at the token
  boundary — KEEP: raw PANs arrive only via the new `card` field +
  event, never as a token); `create_payment_method` brand/last4/expiry
  format checks (local shape only; semantics now the validator's);
  `logistics.rs:1372 luhn_check_digit` (tracking-number check digit —
  different domain, do NOT converge); `billing.rs:2000`
  `invoice_prefix` validation (unrelated). **PCI violations: NONE
  found.** No `card_number` / `cvv` / `cvc` fields outside card handling;
  `payment_methods` table holds only `provider_token` / `brand` / `last4`
  / `exp_month` / `exp_year`; POS `card` is a tender kind label +
  authorisation-code reference; store `payment.initiated` carries
  order/provider/amount only; `billing.rs` has zero "card" matches;
  `payments.rs` has zero `log::` calls; `privacy.rs` lists `card_number`
  only as a PII-redaction marker. Two watch items (not violations):
  B1's `POST /api/v1/cards/validate` accepts raw PAN+CVV in the body by
  design (in-memory only — must never be logged/persisted); the gate's
  bus payload carries `number` / `expiry` / `cvv` in-process (audit /
  webhook exclusion verified).
- Pre-existing gap found, NOT fixed (out of scope): `settings.rs
  on_settings_merge` accepts only `vcs.*` namespaces, so the `auth.<id>`
  and `cloudflare` merges emitted by auth_manager/cloudflare are
  silently dropped (`merged: 0`). Do not widen the allowlist without an
  integrator decision (merged values bypass `config.validate`).

## 2. Modularity-rule check (standing, MUTLAK)

Second sanctioned hard-code exception (first: standard paper sizes):
card-brand IIN/BIN ranges + the Luhn checksum + per-brand CVV lengths ARE
international standards and MAY be hard-coded as standard constants —
recorded in the module docs. Everything else (which brands are enabled,
custom rules, test mode, expiry upper bound, reference-id prefixes and
default) is data/config-driven. Disclosed constants: IIN prefix/range
tables (29 brands), Luhn algorithm, CVV lengths (Amex 4, others 3),
generic PAN length bounds 12–19, test PAN list, reason-code vocabulary,
capability names, numeric validation bounds. No other hard-coded spots.

## 3. Integrator work list

### 3a. Register the module (`src/modules/mod.rs`)
```rust
pub mod cards;
```
and in the registration function, following the existing form:
```rust
cards::register(registry);
```
Verified: `mod.rs` has 0 mentions of `cards` (72 `pub mod` lines total
at verification). Until registered, the module, its hook answerer, and
its routes are not live, and the gate fails closed with
`cards_unavailable` for every gated card payment. (`reference_id` IS in
`src/core/mod.rs` — verified.)

### 3b. Diesel schema/models (`src/db/schema.rs`, `src/db/models.rs`)
Add `table!` blocks + Queryable/Insertable structs for the 2 tables in
`000099_cards` (CREATE == DROP verified):
- `card_custom_rules` (id uuid, tenant_id, name, brand_name,
  iin_prefixes/ranges JSONB, allowed_lengths JSONB, cvv_length,
  luhn_required, enabled, overrides_standard_brand nullable, timestamps)
- `card_validation_daily_counts` (tenant_id, date, matched_rule, result,
  count — no PANs)
And the durable reference-id settings table (see §3e). Migration
`000099_cards` is unique on disk (`000097_pricing` and
`000098_holidays` were taken by concurrent rounds; `000100_calculator`
still out of sequence — integrator renumbers the known collisions:
000056, 000070, 000075, 000077, 000080, 000090 + 000100).

### 3c. AUDITED_EVENTS additions (`src/modules/audit.rs`)
Insert after `"project.time.logged",` (audit subscribes only to listed
events and records `ctx.data` verbatim):
```rust
    // Card validation outcomes (cards module) — masked PAN only.
    "cards.validated",
    // Custom card rule changed (cards module — NOTE: the cards module
    // does not emit this event yet; add the emit site in cards.rs
    // create/update/delete custom-rule handlers first).
    "cards.custom_rule.changed",
    // Reference ids minted (reference-id generator).
    "reference.generated",
    // Reference-id settings changed.
    "reference.settings_changed",
```
NEVER add `"cards.validate_requested"` — its payload carries the
PAN/expiry/CVV.

### 3d. Role capabilities (roles module — all checks fail closed until defined)
- `cards:read` / `cards:validate` / `cards:manage` (constants agree in
  cards.rs `PERM_CARDS_*` and payments.rs `CARDS_READ/VALIDATE/MANAGE`).
  Admin already holds all three via `"*": ["*"]` in `roles.rs`. Seed
  snippet for a future finance role only, never for customer/editor/
  member (an open validation oracle lets anyone probe card numbers):
```rust
("Finance", "finance", "Manages card validation settings and custom rules.",
 json!({ "cards": ["read", "validate", "manage"], "payments": ["read"] }))
```
- `store.reference:read` / `store.reference:manage`,
  `billing.reference:read` / `billing.reference:manage` (B2's new
  reference-settings endpoints).

### 3e. UNIQUE-constraint snippet (from B2 — apply inside the migration that creates the persistence tables for store orders / billing invoices; do NOT run standalone)
```sql
-- Retry rule for the DB layer: on unique-violation, regenerate once
-- (generate_unique) and retry the insert; a second conflict returns an
-- error (409) and must NEVER silently overwrite the existing row.
ALTER TABLE store_orders ADD COLUMN IF NOT EXISTS reference VARCHAR(30) NOT NULL;
ALTER TABLE store_orders ADD CONSTRAINT store_orders_reference_unique UNIQUE (reference);
ALTER TABLE billing_invoices ADD COLUMN IF NOT EXISTS reference VARCHAR(30) NOT NULL;
ALTER TABLE billing_invoices ADD CONSTRAINT billing_invoices_reference_unique UNIQUE (reference);
-- Range queries rely on lexicographic == chronological order per prefix:
--   reference >= 'ORDR-<from_hex>' AND reference < 'ORDR-<to_hex>'
--   AND reference LIKE 'ORDR-%'
-- Durable form of ReferenceIdSettings, one row per tenant (tenant_id
-- always from the tenant context; no tenant context -> no row, fail closed):
CREATE TABLE IF NOT EXISTS reference_id_settings (
  tenant_id UUID NOT NULL,
  default_prefix VARCHAR(4) NOT NULL DEFAULT 'ORDR',
  product_prefixes JSONB NOT NULL DEFAULT '{}'::jsonb,
  max_total_length INT NOT NULL DEFAULT 30,
  CHECK (max_total_length BETWEEN 30 AND 64),
  PRIMARY KEY (tenant_id)
);
```

### 3f. Settings source-of-truth split (DECIDE — do not ship both)
B1 owns the runtime settings as per-tenant `CardsSettings`
(`PUT /api/v1/cards/settings`: `test_mode` live/sandbox enum,
`max_expiry_years_ahead` 1–50 default 20, `gpn_enabled`,
`disabled_standard_brands` denylist). B3's gateway namespace
(`cards_settings_definitions()` in payments.rs) uses an
`enabled_brands` allowlist and bound-max-30, plus a
`reference_id.*` namespace. The two are semantically different.
**Coordinator recommendation: B1's per-tenant settings are the single
source of truth; B3's namespace then mirrors it or is dropped.**
Also open: the settings-module `reference_id.*` namespace vs B2's
store/billing `reference-settings` endpoints (which emit
`reference.settings_changed` with the full settings value) — a bridge
listener translating that into the gateway mirror, or a decision to keep
one canonical surface, is still open. Nothing on the gateway side
currently reads the effective `reference_id.*` values — B2's
store/billing endpoints are the only live read paths for generation.

### 3g. Pending snippets from B3 (payments.rs is under live concurrent edit — do NOT apply until the file settles)
**S0 — COMPILE BLOCK (critical, coordinator-verified on disk).**
`PaymentIntent` declares `payment_method_id` (payments.rs:2551) but the
`create_intent` literal (~line 7865) does not set it (fields end at
`updated_at`, no struct-update syntax). Fix, in `create_intent` after
`payment_method_token: token,`:
```rust
            payment_method_id: input.payment_method_id,
```
**S1–S4 — gate call sites** (insert after `resolve_token(...)` / before
`adapter_for(&connection)?` in each handler; all use
`request_card_validation(&state, &req, card, method_id, claimed,
&connection, customer_id, order_id, intent_id)`). Verified: the gate
function exists (line 907) but has ZERO call sites today.
- S1 `confirm_intent`: `request_card_validation(&state, &req, request.card.as_ref(), request.payment_method_id.or(intent.payment_method_id), None, &connection, &intent.customer_id, intent.order_id.as_deref(), Some(intent.id))?;`
- S2 `create_charge`: `request_card_validation(&state, &req, input.card.as_ref(), input.payment_method_id, None, &connection, &input.customer_id, input.order_id.as_deref(), None)?;`
- S3 `pay_order`: same as S2 with `Some(order_id.as_str())` as order_id.
- S4 `pay_split_part`: first add `req: HttpRequest,` to the signature
  (after `ctx`), then `request_card_validation(&state, &req, request.card.as_ref(), request.payment_method_id, None, &connection, &customer_id, split.order_id.as_deref(), None)?;` (bank-transfer early return stays before the gate — correct, no card involved).
**S5 — `create_payment_method`:** add `state: web::Data<Arc<PaymentsState>>,` to the signature; re-add `Datelike` to the chrono import (`use chrono::{DateTime, Datelike, Utc};`); replace the `2000..=2100` year check with the settings-driven bound (`latest = Utc::now().year() + cards_expiry_upper_bound_years() as i32`; reject `< 2000` or `> latest`; past-ness stays the validator's `invalid_expiry`); after `find_connection`, call the gate with `input.card.as_ref()`, `None`, and `Some(CardMethodRef { brand: input.brand.trim().to_string(), last4: input.last4.clone(), exp_month: input.exp_month, exp_year: input.exp_year })`, then store `brand` from `outcome.brand` (validator-detected, authoritative) falling back to the claimed brand.
**S6 — `create_subscription`:** add `req: HttpRequest,` to the signature; gate after `resolve_token` with `input.card.as_ref(), input.payment_method_id, None, &connection, &input.customer_id, input.order_id.as_deref(), None` (provider `subscription_create` can charge immediately). Note: this handler has no `require_customer` check (pre-existing gap).
**S7 — route + registry entry for the already-written `get_cards_settings` handler** (verified: handler exists at payments.rs:1135, no `.route(` for it): in `configure_routes` after `.route("/providers/{kind}/settings", web::get().to(get_provider_settings))` add `.route("/cards/settings", web::get().to(get_cards_settings))`; in `route_info()` after the providers-settings entry add `RouteInfo { method: "GET", path: "/api/v1/payments/cards/settings", owner: "payments", description: "Card and reference-id settings definitions, effective values, capabilities, and the validation contract (admin-only, cards:read)" }` (auto-collected by api_manager once registered).

### 3h. README / TODO lines
- Module count: add 1 (cards). Migration count: add 1 (000099_cards).
  Table count: add 2 (+1 reference_id_settings when §3e lands).
- TODO.md: add a batch-18 section with the OPEN list from §5.

## 4. Pre-existing tree-wide blocker (NOT batch 18's — integrator must fix)
`src/modules/privacy.rs` EXISTS on disk but `pub mod privacy;` is
missing from `src/modules/mod.rs` (verified: 72 `pub mod` lines, no
`privacy`, no `cards`). B2 flagged it as a tree-wide blocker from batch
14. Batch 18 was forbidden to edit `mod.rs`; the integrator must add
both `pub mod privacy;` and `pub mod cards;` (plus their registrations)
before any `cargo check` can pass tree-wide.

## 5. OPEN list (explicit)

1. **`cargo check` / `cargo build` / `cargo test` have NEVER run anywhere**
   (no toolchain, crates.io blocked) — compilation unverified for the
   whole batch. Known would-be compile error: S0 (§3g). My crude
   brace-balance stripper gave inconclusive results on the concurrently
   edited files (false positives from lifetimes/char literals) — I do NOT
   report an imbalance; `cargo check` is the arbiter.
2. **Gate call sites S1–S4 + S5–S7 not applied** (§3g) — the gate function
   exists but is never called; until applied, checkout does NOT validate
   cards. `get_cards_settings` is unwired (S7).
3. **Method/claim modes cannot succeed yet (loudest gap).**
   Coordinator-verified on disk: B1's `on_validate_requested` requires
   `number` and answers `invalid_format` without it, so stored-method
   charges fail closed with `invalid_format` until B1's handler is
   extended to accept a number-less payload (`brand` / `last4` /
   `expiry` — the gateway already sends `mode`, `brand`,
   `claimed_brand`, `claimed_last4`, `expiry`, `masked_pan`) and run
   brand+expiry checks with the same result shape. The gate keeps
   emitting them (fail-closed beats fail-open).
4. **Settings source-of-truth split** (§3f): allowlist vs denylist,
   bound max 30 vs 50, `test_mode` bool vs live/sandbox enum; two admin
   surfaces for reference-id settings.
5. **`cards.custom_rule.changed` has no emit site** in `cards.rs` — add
   emits in create/update/delete custom-rule handlers before relying on
   the audit entry.
6. **Pre-reference backfill** — orders/invoices serialized before this
   batch carry `reference: ""` (`#[serde(default)]`); the integrator
   must backfill them (generate + persist under the UNIQUE constraint) —
   no silent default is invented at read time.
7. **Reference threading gaps** — intents (`PaymentIntent` has no
   `reference_id` field) and renewals/local-schedule subscriptions fall
   back to a fresh reference per charge; the store order reference stays
   the reconciliation key via `transaction.order_id`. MPGS
   capture/refund/void generates a fresh order reference when `order_id`
   is absent; ideally it should reuse the original charge's reference
   (not stored today).
8. **Tenant settings resolution** — `ReferenceIdSettings` is held
   per-module in memory with defaults; per-tenant resolution (loading
   the tenant's row from `reference_id_settings` at request time) and
   the durable table itself are integrator work; generation itself is
   DB-free so it cannot leak across tenants, and persistence must go
   through the caller's tenant context.
9. **Subscription renewals (`renew_one`) are not gated** — no
   `HttpRequest` / tenant context on that path; they charge
   previously-validated stored tokens. Needs tenant id on the
   subscription record to gate.
10. **No package-entitlement check in the gate** — no
    `entitlements.check_requested` answerer exists in-tree; the gate
    enforces tenant context + `require_customer` ownership only. Do not
    emit a check nobody answers and ignore the silence.
11. **No entitlement gate in cards module** — capability + tenant-context
    checks only, matching sibling modules; wiring a `cards` entitlement
    check is integrator work.
12. **In-memory stores are authoritative** (cards custom rules, reference
    settings, store/billing/payments) — the migrations are persistence
    targets, not yet read paths; DB-backed persistence is integrator
    work. Migrations never executed against live PostgreSQL.
13. **Open consistency question**: should the gateway reject when claimed
    `last4` ≠ validated PAN's last 4 on method onboarding? (B1 ignores
    `claimed_last4` today.)
14. **`settings.merge` vcs-only allowlist** (§1c) — integrator decision.
15. **B1 notes**: override clearing — updating a custom rule with
    `overrides_standard_brand: ""` clears the override; `null`/absent
    keeps the existing value (no explicit clear-by-null). Elo source
    quirk ignored deliberately: the repo's Elo `isValid` contains a dead
    range check on the 6th digit of 5069xx prefixes; only its IIN list
    was used. Tenant middleware dependency: all cards HTTP routes fail
    closed (401) when no tenant context is resolved — correct per the
    standing rule, but the routes serve nothing until tenant resolution
    is wired for the cards paths (same posture as other tenant-scoped
    modules).

## 6. Coordinator verification (by read-back, not by trusting reports)

- Line counts match every builder's report exactly: cards.rs 2,535;
  reference_id.rs 699; payments.rs 11,138; settings.rs 396 (+29);
  webhooks.rs 1,318 (+9); store.rs 8,579 (+154); billing.rs 8,395
  (+184); migration up.sql 86 lines, down.sql 9 lines.
- Greps over all batch-18 files (reliable counts, no pipes):
  **0** forbidden reference-project name hits, **0** `todo!` /
  `unimplemented!` macros, **0** Indonesian words (word-boundary scan,
  broad list incl. kartu/pembayaran/pesanan/validasi/merek/kedaluwarsa).
- Route parity: cards.rs 11 `.route(` == 11 `RouteInfo`; store/billing
  reference-settings routes present with RouteInfo entries.
- Migration CREATE == DROP (2/2); 000099 unique on disk.
- Luhn verified by my own hand computation: 4242424242424242 → 80
  valid; 4242424242424241 → 79 invalid; 378282246310005 → 60 valid;
  378282246310006 → 61 invalid.
- Elo prefix list counted by hand: 71 (1 + 39 + 29 + 2) — matches B1.
- `format_prefix` read back: trim, ASCII-alnum filter, uppercase,
  take 4, pad `X` — exact spec match. `generate` / `is_valid`
  structure read back — exact spec match.
- Hook registration verified: `bus.register(CARDS_VALIDATE_REQUESTED,
  "cards", handler)` in cards.rs `register_hooks`; settings handlers
  registered at payments.rs:11030/11035; `settings.changed` emitted at
  settings.rs:311/338; `cards.validate_requested` deliberately absent
  from webhooks `KNOWN_EVENTS`.
- S0 verified on disk (PaymentIntent struct field at 2551, literal
  missing it, no struct-update syntax). Zero `request_card_validation`
  call sites verified (only the fn definition at 907). `get_cards_settings`
  handler verified present (1135) with no route. `on_validate_requested`
  `number` requirement verified (missing → `invalid_format`).
- `pub mod privacy;` missing from `src/modules/mod.rs` verified
  (privacy.rs exists on disk; 72 `pub mod` lines, no privacy, no cards).
- **Corrections to builder reports recorded here**: (a) the ~950 lines
  B2 attributed to "B1 landing in payments.rs" are B3's Batch 18 section
  (lines 199–1156, marker at line 200); B2's own section is 1765–1827.
  (b) B1's migration is 000099, not 000097 (000097_pricing was taken).
