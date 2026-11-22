# Integration notes — Batch 3 B5: subscription lifecycle

Written by the batch-3 B5 builder (subscription-based selling). This file is
for the INTEGRATOR only: apply the snippets below to the shared files, in the
order given. Do NOT commit this file as documentation for users; fold the
README/TODO lines into README.md / TODO.md and then delete this file (or keep
it under docs/ if the project wants a paper trail — either way, the snippets
here are the source of truth for this batch, not a substitute for reading the
module files).

## Batch scope

| Item | Builder | Files | Lines | Migration |
|---|---|---|---|---|
| Subscription lifecycle (B5) | B5 | `src/modules/products.rs`, `src/modules/store.rs`, `src/modules/billing.rs` | products 4925 → 5162 (+237); store 4334 → 5504 (+1170); billing 2213 → 3341 (+1128) | `2026-11-22-000040_subscriptions` |

Migration `000040` adds 3 tables: `subscriptions`, `subscription_events`,
`dunning_attempts`. No other migration number is used by this batch. Note:
`000041` already exists twice in staging (`000041_pricing_marketplace` and
`000041_workflows`) — that collision belongs to the sibling batches, not this
one; `000040` was verified free before creation.

Greps over every batch file (three `.rs` files + both migration files + this
note): 0 forbidden reference-project names, 0 `todo!` / `unimplemented!` /
`panic!` / `unreachable!` macros, 0 Indonesian words (word-boundary list).
`cargo check` has NEVER run anywhere (no toolchain in this VM, crates.io
blocked) — compilation is NOT verified for any of this. Delimiter balance
verified with a corrected Rust-aware tokenizer: `{`/`(`/`[` all 0 in all
three files. See "Verification status" at the end.

## Design in one paragraph

The store owns the authoritative subscription record (`Subscription` in
`StoreState.subscriptions`, created when a subscription order becomes paid).
Billing keeps a mirror (`SubscriptionLifecycle` in `BillingState`) synced from
`subscription.*` events and never writes store records directly. Lifecycle
decisions flow store-ward through one hook, `subscription.lifecycle.apply`
(actions `renew` / `mark_past_due` / `end_trial` / `cancel`); the store
applies the action and re-emits the public `subscription.*` event, which
billing mirrors from. Money in the in-memory store is `f64` major units (as
elsewhere in store/billing); money in SQL is `NUMERIC(19,4)`. Product/plan
data stays in the existing product tables; the migration holds only the
subscription record, its audit trail, and dunning attempts.

Period lengths (shared convention, matching `license_expiry_for` in
store.rs): weekly 7 days, monthly 30 days, yearly 365 days. A trialing
subscription's `current_period_end` is `trial_end + one period`.

## 1. `src/db/schema.rs` — table definitions

Add these three `diesel::table!` blocks (place them near the other store /
billing tables). Types follow the existing conventions (`Numeric` for money,
`Varchar`, `Int4`, `Bool`, `Timestamptz`, `Uuid`, `Nullable<>`, `Jsonb`).
`subscription_events.subscription_id` is deliberately a plain `Uuid` with NO
foreign key (the audit trail must survive the record it describes);
`dunning_attempts.subscription_id` is a real FK `ON DELETE CASCADE`.

```rust
diesel::table! {
    subscriptions (id) {
        id -> Uuid,
        user_id -> Varchar,
        company_id -> Nullable<Uuid>,
        product_id -> Varchar,
        tier_id -> Nullable<Varchar>,
        order_id -> Varchar,
        module_id -> Nullable<Varchar>,
        status -> Varchar,
        billing_period -> Varchar,
        price -> Numeric,
        currency -> Varchar,
        trial_days -> Int4,
        trial_end -> Nullable<Timestamptz>,
        current_period_start -> Timestamptz,
        current_period_end -> Timestamptz,
        proration_policy -> Varchar,
        grace_period_days -> Int4,
        dunning_max_retries -> Int4,
        dunning_retry_interval_days -> Int4,
        cancel_at_period_end -> Bool,
        cancelled_at -> Nullable<Timestamptz>,
        pending_product_id -> Nullable<Varchar>,
        pending_tier_id -> Nullable<Varchar>,
        pending_price -> Nullable<Numeric>,
        pending_billing_period -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    subscription_events (id) {
        id -> Uuid,
        subscription_id -> Uuid,
        event -> Varchar,
        actor -> Nullable<Varchar>,
        reason -> Nullable<Varchar>,
        payload -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    dunning_attempts (id) {
        id -> Uuid,
        subscription_id -> Uuid,
        attempt_number -> Int4,
        reason -> Varchar,
        amount_minor -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

Add to the `allow_tables_to_appear_in_same_query!` list:

```rust
    subscriptions,
    subscription_events,
    dunning_attempts,
```

Add the one joinable (dunning attempts belong to a subscription;
`subscription_events` has no FK by design, so it gets no `joinable!`):

```rust
diesel::joinable!(dunning_attempts -> subscriptions (subscription_id));
```

## 2. `src/db/models.rs` — struct definitions

Money uses `diesel::data_types::PgNumeric` (already imported in models.rs).
Per the models.rs header note, structs carrying `PgNumeric` skip the serde
derives (`PgNumeric` has no serde implementation). `SubscriptionEvent` and
`DunningAttempt` carry no money, so they keep the serde derives.

Deliberate deviation from the usual `New*` rule, documented here so the
integrator does not "fix" it away: models.rs says `New*` structs omit
columns the database fills with a DEFAULT. `NewSubscription` below INCLUDES
the defaulted business fields (`status`, `billing_period`, `price`,
`currency`, `trial_days`, `proration_policy`, `grace_period_days`,
`dunning_max_retries`, `dunning_retry_interval_days`,
`cancel_at_period_end`) because the writer must persist the values the
store computed at activation — the SQL defaults (`price 0`, `trial_days 0`,
`prorate`) would be wrong data, not harmless placeholders. `created_at` /
`updated_at` are still omitted (DB `now()` default), and `id` is carried
(callers supply ids, as everywhere else).

```rust
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = subscriptions)]
pub struct Subscription {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub product_id: String,
    pub tier_id: Option<String>,
    pub order_id: String,
    pub module_id: Option<String>,
    pub status: String,
    pub billing_period: String,
    pub price: PgNumeric,
    pub currency: String,
    pub trial_days: i32,
    pub trial_end: Option<DateTime<Utc>>,
    pub current_period_start: DateTime<Utc>,
    pub current_period_end: DateTime<Utc>,
    pub proration_policy: String,
    pub grace_period_days: i32,
    pub dunning_max_retries: i32,
    pub dunning_retry_interval_days: i32,
    pub cancel_at_period_end: bool,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub pending_product_id: Option<String>,
    pub pending_tier_id: Option<String>,
    pub pending_price: Option<PgNumeric>,
    pub pending_billing_period: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = subscriptions)]
pub struct NewSubscription {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<Uuid>,
    pub product_id: String,
    pub tier_id: Option<String>,
    pub order_id: String,
    pub module_id: Option<String>,
    pub status: String,
    pub billing_period: String,
    pub price: PgNumeric,
    pub currency: String,
    pub trial_days: i32,
    pub trial_end: Option<DateTime<Utc>>,
    pub current_period_start: DateTime<Utc>,
    pub current_period_end: DateTime<Utc>,
    pub proration_policy: String,
    pub grace_period_days: i32,
    pub dunning_max_retries: i32,
    pub dunning_retry_interval_days: i32,
    pub cancel_at_period_end: bool,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub pending_product_id: Option<String>,
    pub pending_tier_id: Option<String>,
    pub pending_price: Option<PgNumeric>,
    pub pending_billing_period: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = subscription_events)]
pub struct SubscriptionEvent {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub reason: Option<String>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = subscription_events)]
pub struct NewSubscriptionEvent {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub event: String,
    pub actor: Option<String>,
    pub reason: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = dunning_attempts)]
pub struct DunningAttempt {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub attempt_number: i32,
    pub reason: String,
    pub amount_minor: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = dunning_attempts)]
pub struct NewDunningAttempt {
    pub id: Uuid,
    pub subscription_id: Uuid,
    pub attempt_number: i32,
    pub reason: String,
    pub amount_minor: Option<i64>,
}
```

Conversion point (integrator's to write, not invented here): the in-memory
`Subscription.price` is `f64` major units; the durable form is
`NUMERIC(19,4)` (`PgNumeric`, scale 4). The persistence writer must convert
`f64` major units to a scale-4 `PgNumeric` (round to cents first, as
`round_money` does in store.rs) and back. Do not store the `f64` as a
floating-point column — there is no `DOUBLE PRECISION` column in migration
000040, on purpose.

## 3. `README.md` lines

Extend item 20 (Store / Marketplace) — append to its paragraph:

```markdown
    Subscription orders create a subscription on payment (`trialing` with a
    trial, otherwise `active`); plan changes mid-cycle prorate per the
    product policy (`prorate` / `no_prorate` / `credit_only`), and
    cancellation is immediate or at period end. An active subscription
    covering a sellable module counts as that module's entitlement.
```

Extend item 23 (Billing / Invoicing) — append to its paragraph:

```markdown
    The subscription lifecycle engine drives renewals (renewal invoice
    before period end, charge requested via `subscription.renewal_due`),
    renewal reminders, proration invoices on upgrade/downgrade, dunning
    with per-product retry settings and a recorded attempt history, grace
    periods (`past_due`, then `cancelled` when grace expires), and
    trial-end handling, via `POST /api/v1/billing/subscriptions/scan` and
    the `billing.subscriptions.scan` hook.
```

Header count line (line ~10: "31 Diesel migrations covering 179 tables"):
do NOT just add +1 / +3 — parallel batches added migrations concurrently, so
recount migrations and tables from the final tree and update the numbers to
match. This batch's delta is +1 migration, +3 tables.

## 4. `TODO.md` lines

Add under a new section (or the batch-3 section, if one exists):

```markdown
- Batch 3 B5 — subscription lifecycle (integrator): wire
  `src/db/schema.rs` + `src/db/models.rs` from
  INTEGRATION_NOTES_BATCH3_B5.md §1–2; add the licensing hook call from §5
  (subscription entitlements) and the payments `subscription.renewal_due`
  listener from §6; arm a recurring scheduler job that emits
  `billing.subscriptions.scan` (the scan is safe to run frequently — it
  issues one renewal invoice and one reminder per period); fold the
  README lines from §3 into README.md. The in-memory DashMap store is
  still authoritative; the `subscriptions` / `subscription_events` /
  `dunning_attempts` tables are the durable form, not yet the read path.
- Batch 3 B5 — persistence writer (future): write store subscriptions to
  the `subscriptions` table on `subscription.created` / `renewed` /
  `cancelled` (and proration/dunning rows to `subscription_events` /
  `dunning_attempts`); convert `f64` major units to scale-4 `PgNumeric`
  (see §2 conversion point). No writer exists yet — the tables are
  created by migration 000040 but nothing inserts into them.
- Batch 3 B5 — scheduler arm for the subscription scan: no recurring job
  emits `billing.subscriptions.scan` yet; until one exists, the lifecycle
  only runs when `POST /api/v1/billing/subscriptions/scan` is called or
  `billing.subscriptions.scan` is emitted (billing also runs the scan on
  `cleanup.requested`).
```

## 5. Licensing convergence — subscription entitlements

Read first: `src/modules/licensing.rs`, `check_module_entitlement`
(~lines 1083–1135). Exact rule today, in order: (1) unknown module → 404;
(2) non-sellable → no entitlement needed; (3) sellable core → implicit owner
entitlement (`reason: "owner_entitlement"`); (4) sellable non-core → active
license whose `product` equals the module id or `module:<id>`
(`has_module_entitlement`, `reason: "license"`); otherwise emit
`module.entitlement.denied` (`reason: "no_active_entitlement"`) and return
403.

The store already answers the subscription side. `StoreState` holds
`Subscription.module_id` (the sellable module a product sells access to,
mirrored from the product catalogue by `on_product_listed`), and
`on_subscription_module_entitlement_check` (store.rs ~3924) answers the
hook `subscription.module_entitlement.check` with
`{entitled: bool, module_id, company_id, subscription_id?, source:
"subscription", reason?: "no_active_subscription"}`. The access rule is
`subscription_grants_access` (store.rs ~920): trialing/active inside the
current period; past-due inside the grace period; cancelled-at-period-end
inside the current period; immediate cancellation or past grace grants
nothing. The store also emits `subscription.entitlement_changed` on grant /
revoke (creation, immediate cancellation, refund cancellation). Licensing
logic is NOT duplicated anywhere in store.rs — the store only answers the
question.

Insert this block in `check_module_entitlement`, AFTER the
`has_module_entitlement` success branch and BEFORE the deny emit. It follows
the existing hook-result pattern (`ctx.hooks.emit(...)` returns a
`HookContext`; results are read from `.results`, as in
`ProductsState::check_entitlement` in products.rs ~1276–1290). It fails
closed: an unanswered hook (store module not registered) means no
subscription entitlement, and the deny below still runs.

```rust
    if ctx.hooks.has("subscription.module_entitlement.check") {
        let context = ctx.hooks.emit(
            "subscription.module_entitlement.check",
            serde_json::json!({
                "company_id": company_id,
                "module_id": module_id,
            }),
        );
        let entitled = context.results.iter().any(|result| {
            result
                .get("entitled")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        });
        if entitled {
            ctx.hooks.emit(
                MODULE_ENTITLEMENT_GRANTED_EVENT,
                serde_json::json!({
                    "id": module_id,
                    "company_id": company_id,
                    "reason": "subscription",
                }),
            );
            return Ok(());
        }
    }
```

Also update the doc comment above `check_module_entitlement` (the "Exact
rule, in order" list): insert a step 5 — "Sellable non-core module without a
license: an active subscription covering the module counts as an
entitlement (`reason: "subscription"`, answered by the store module through
`subscription.module_entitlement.check`)" — and renumber the deny step to 6.
`on_store_entitlement_check` (store.rs ~2114) already matches active
subscriptions by `product_id` as well (`source: "subscription"`), so
product-level entitlement checks via `store.entitlement.check` see
subscriptions without any licensing change.

## 6. Payments convergence — renewal charges

Read first: `src/modules/payments.rs` (gateway subscriptions) and
`scheduler.rs` (job kinds). The payments module owns gateway subscriptions
(`GatewaySubscription`, table `gateway_subscriptions`, events
`payment.subscription.created/cancelled/renewed`, `payment.captured` /
`payment.failed`). Its `renew_one` charges by ITS OWN gateway subscription
id, and its `payment.captured` renewal payload carries `subscription_id` =
gateway id. The commercial subscription id from this batch is a DIFFERENT
id — do not mix them.

What billing does: `request_renewal_charge` (billing.rs ~929) emits
`subscription.renewal_due` with this payload:

```json
{
  "subscription_id": "<commercial subscription id>",
  "order_id": "<original order id>",
  "invoice_id": "<renewal invoice id>",
  "user_id": "<buyer>",
  "amount": <price per period, major units>,
  "currency": "USD",
  "retry": false
}
```

The payments module must add a listener for `subscription.renewal_due`
that maps `order_id` to its gateway subscription (the order is the link
between the two records) and charges through its existing renew path.
Charge confirmation comes back through `payment.captured`, which billing
already handles: `maybe_renew_subscription` (billing.rs ~955) advances the
period ONLY when the paid invoice is the recorded current renewal invoice
(the initial order invoice and proration invoices never advance the period;
the new period starts at the old period end, so a late payment never
shortens the next period). Dunning failures come through `payment.failed`
(billing's `on_payment_failed`, ~2953, falls back from `subscription_id` to
an `order_id` lookup, because payments' failure payload carries its own
gateway id).

Do NOT use the scheduler kind `payment.subscription.renew` for commercial
subscriptions: its payload `subscription_id` names the payments module's
own gateway subscription, so enqueueing a commercial id would fail there.
That kind is for gateway renewals only. The subscription scan is driven by
the `billing.subscriptions.scan` hook and `POST
/api/v1/billing/subscriptions/scan` (see TODO lines in §4 for the scheduler
arm).

## 7. What was built — edit summary

### `src/modules/products.rs` (4925 → 5162 lines)

| Change | Location |
|---|---|
| Module header comment: subscription plan fields + endpoint documented | ~lines 1–17 |
| Constants `PRORATION_POLICIES`, `MAX_DUNNING_RETRIES`, `MAX_GRACE_PERIOD_DAYS`, `MAX_DUNNING_RETRY_INTERVAL_DAYS` | lines 83–93 |
| `Product` gains `proration_policy` (default `"prorate"`), `grace_period_days` (default 0), `dunning_max_retries` (default 3), `dunning_retry_interval_days` (default 3) | lines ~310–340 |
| Default fns `default_proration_policy` / `default_dunning_max_retries` / `default_dunning_retry_interval_days` | lines 454–466 |
| New `SubscriptionPlanView` struct | lines 434–~453 |
| `CreateProduct` / `UpdateProduct` gain the four optional plan fields | (create/update input structs) |
| Validators `validate_proration_policy` + `resolve_subscription_settings(pricing_model, proration_policy, grace, max_retries, interval)` | lines 1620–1677 |
| `create_product` / `update_product` wire the fields; non-subscription products reject non-default plan fields; changing away from subscription resets stored plan fields to defaults | lines 1950–~2100, 2098–~2260 |
| New endpoint `get_subscription_plan`: `GET /api/v1/products/{id}/subscription-plan` | lines 2616–2657; route line 5092; `route_info` line 5145 |

Validation bounds: trial ≥ 0 (≤ 3650 in SQL), grace 0–90 days, dunning max
retries 0–10, retry interval 1–30 days, proration in
`prorate` / `no_prorate` / `credit_only`.

### `src/modules/store.rs` (4334 → 5504 lines)

| Change | Location |
|---|---|
| Module header comment + description updated (subscriptions) | header; `route_info` |
| `Product` gains plan fields + `module_id` (sellable module the product sells access to); same fields on `CreateProduct` / `UpdateProduct`; `validate_subscription_settings` | lines ~67–141, 794–~820 |
| `on_product_listed` mirrors plan fields + `module_id` from the products catalogue payload (invalid proration falls back to `"prorate"`) | lines 1863–~1950 |
| `OrderItem` gains subscription snapshot fields (`trial_days`, `proration_policy`, `grace_period_days`, `dunning_max_retries`, `dunning_retry_interval_days`, `module_id`) resolved at checkout from product/tier | ~lines 360–390 |
| New `Subscription` struct (statuses `trialing`/`active`/`past_due`/`cancelled`; `cancel_at_period_end`; `pending_*` plan-change fields; `trial_end`; period start/end; plan snapshot; `module_id`; `company_id`; `order_id`) | lines 441–501 |
| `ChangePlanRequest`, `CancelSubscriptionRequest` (`cancel_at_period_end` defaults true) | lines ~502–530 |
| `StoreState.subscriptions: DashMap<String, Subscription>` | lines 708, 724 |
| Helpers `period_days`, `period_end`, `subscription_grants_access`, `proration_amounts` | lines 902–~1000 |
| `valid_billing_period` extended to monthly/yearly/weekly; `license_expiry_for` gains weekly (7 days) | lines 794, 883 |
| Checkout enforces: a subscription order must reference exactly one subscription product, quantity 1, billing period present (422/400 otherwise) | `checkout` ~3243; checks ~2266, 2671, 2774 |
| `activate_subscription_for_paid_order` (idempotent by `order_id`; trialing iff trial days > 0; recurring price = renewal price else price paid; emits `subscription.created` + `subscription.entitlement_changed`) | lines 3590–3668 |
| `cancel_subscriptions_for_order` (immediate, for refunds) | lines 3669–3714 |
| `on_subscription_lifecycle_apply` (actions `renew` / `mark_past_due` / `end_trial` / `cancel`; renew applies a pending plan change; guards against cancelled / cancel-at-period-end / bad dates) | lines 3715–3923 |
| `on_subscription_module_entitlement_check` (answers `subscription.module_entitlement.check`) | lines 3924–3977 |
| HTTP handlers `list_subscriptions` (filters status/user_id/product_id/module_id/company_id), `get_subscription` (owner check), `change_subscription_plan`, `cancel_subscription` | lines 3978, 4025, 4054, 4199 |
| Activation wired into all three paid paths: manual `pay_order` (subscription id in response), `update_order` `"paid"` branch, `on_payment_captured` (subscription id in hook result) | lines 4385, ~1457, ~4300 |
| Refunds cancel subscriptions: `on_payment_refunded`, `refund_order` (count in payload/response) | lines 1608, 4501 |
| `on_store_entitlement_check` also matches active subscriptions by `product_id` (`source: "subscription"`) | lines 2114–~2200 |
| Hooks registered: `subscription.lifecycle.apply`, `subscription.module_entitlement.check` | lines ~5356–5370 |
| Routes under `/api/v1/store/subscriptions` + `route_info` entries | lines 5462–5470, 5407–5410 |

Proration (`proration_amounts`): credit = unused share of old price;
charge = new price for the remaining share of the new period (both rounded
to cents); net = charge − credit, may be negative. `prorate` applies
immediately (new period starts now); `no_prorate` stores a pending plan,
applies at next period end, zero amounts; `credit_only` credits unused time
now, new price from next period. `change_subscription_plan` emits
`subscription.upgraded` / `subscription.downgraded` with proration lines +
`effective_at`; direction by new price ≥ old price.

### `src/modules/billing.rs` (2213 → 3341 lines)

| Change | Location |
|---|---|
| Module header comment extended (subscription lifecycle section) + description updated | header |
| `Invoice` gains `subscription_id: Option<String>` (serde default); both `Invoice` constructions updated (`invoice_from_order_value` → `None`, `create_invoice` → `None`) | line 122; lines 462, 1483 |
| Constants `RENEWAL_INVOICE_LEAD_DAYS = 2`, `RENEWAL_REMINDER_LEAD_DAYS = 3` | line 207 |
| New `SubscriptionLifecycle` mirror + lifecycle bookkeeping (`renewal_invoice_id`, `renewal_invoice_for_period_end`, `renewal_reminder_sent_for`, `dunning_attempts`, `last_failure_at`, `next_retry_at`) | lines 220–275 |
| New `DunningAttempt` (1-based attempt number, reason, optional `amount_minor`) | lines 276–295 |
| `BillingState` gains `subscription_lifecycles` and `dunning_attempts` DashMaps | lines 400–402, 417–418 |
| `BillingState` methods: `period_days`, `sync_subscription_lifecycle` (commercial fields overwritten from store payload; per-period bookkeeping preserved, cleared on period change), `build_renewal_invoice`, `build_proration_invoice`, `request_notification` (`notification.requested` direct pattern), `request_renewal_charge`, `maybe_renew_subscription`, `run_subscription_scan` | lines 638–~1200 |
| `invoice_for_payment_event` prefers the open subscription-linked invoice (by `subscription_id`, then by `order_id` preferring subscription-linked unpaid) so renewal payments land on the renewal invoice, not the already-paid original | lines 2445–2552 |
| `on_payment_captured` calls `maybe_renew_subscription` after a fully-paid invoice; `pay_invoice` also calls it (manual renewal payment advances the period) | lines 2553, 1764 |
| Hook handlers `on_subscription_created`, `on_subscription_synced` (renewed/trial_ended/upgraded/downgraded/past_due/cancelled; builds proration invoice on upgraded/downgraded when credit/charge non-zero), `on_payment_failed`, `on_subscriptions_scan` (`billing.subscriptions.scan`), `on_cleanup_requested` extended to also run the subscription scan | lines 2859, 2879, 2953, 2839, 2826 |
| HTTP handlers `list_subscriptions`, `get_subscription`, `list_dunning_attempts`, `run_subscription_scan_endpoint` | lines 3076, 3117, 3129, 3157 |
| Hook registrations in `register_hooks` | lines 3213–3275 |
| Routes (`GET /subscriptions`, `POST /subscriptions/scan` before `/{id}`, `GET /subscriptions/{id}`, `GET /subscriptions/{id}/dunning`) + `route_info` entries | lines 3297–3300, 3328–3331 |

Scan behaviour (`run_subscription_scan`, ~996–1200): trial ends → apply
`end_trial`; period-end cancellation for `cancel_at_period_end` →
`cancel`; past-due → grace expiry cancels (`grace_period_expired`), else a
due dunning retry is requested; period end with no successful renewal
payment → past-due (`renewal_payment_not_received`); renewal reminder when
the period ends within 3 days, once per period, via `notification.requested`
+ `subscription.renewal_reminder`; renewal invoice when the period ends
within 2 days, once per period, `issued`, due at period end, linked to the
subscription + original order, followed by a `subscription.renewal_due`
charge request. Proration invoice: charge line + negative credit line,
untaxed; net ≤ 0 → recorded as `paid` with nothing due and an explanatory
note. Dunning (`on_payment_failed`): records a `DunningAttempt`, increments
the mirror counter, sets `next_retry_at = now + retry interval`, emits
`mark_past_due` apply + `notification.requested` on EVERY failure and a
final-failure notification when attempts are exhausted; cancellation waits
for grace expiry in the scan.

### Migration `2026-11-22-000040_subscriptions`

- `up.sql` (165 lines): `subscriptions` (UUID PK, `user_id` VARCHAR,
  `company_id` UUID no FK, `product_id`/`tier_id`/`order_id` VARCHAR plain
  references, `module_id`, status CHECK `trialing/active/past_due/cancelled`,
  `billing_period` CHECK weekly/monthly/yearly, `price NUMERIC(19,4)` CHECK
  ≥ 0, currency 3-char, trial 0–3650, `trial_end`, period start/end with
  `CHECK (current_period_end > current_period_start)`, proration CHECK,
  grace 0–90, dunning max 0–10, retry interval 1–30, `cancel_at_period_end`,
  `cancelled_at`, `pending_*` incl. `pending_price NUMERIC(19,4)`, indexes
  on user/company/product/order/module/status/period end),
  `subscription_events` (append-only audit; `subscription_id` UUID plain,
  NO FK on purpose so audit survives the record; event CHECK
  `created/trial_ended/renewed/upgraded/downgraded/payment_failed/past_due/cancelled/entitlement_changed`;
  `payload JSONB`; indexes), `dunning_attempts` (`subscription_id` FK `ON
  DELETE CASCADE`, `attempt_number >= 1`, reason, `amount_minor BIGINT`;
  indexes). No `DOUBLE PRECISION` column.
- `down.sql` (6 lines): drops in reverse order — `dunning_attempts`,
  `subscription_events`, `subscriptions`.

## 8. Events

Emitted by this batch:

| Event | Emitter | Payload (main fields) |
|---|---|---|
| `subscription.created` | store (`activate_subscription_for_paid_order`) | subscription record (id, user/company, product/tier/order, module, status, period, plan snapshot) |
| `subscription.renewed` | store (`on_subscription_lifecycle_apply`, `renew`) | subscription id, new period start/end |
| `subscription.trial_ended` | store (apply `end_trial`) | subscription id |
| `subscription.upgraded` / `subscription.downgraded` | store (`change_subscription_plan`) | subscription id, old/new product + price, proration lines (credit/charge/net), `effective_at` |
| `subscription.payment_failed` | billing (`on_payment_failed`) | subscription id, attempt number, reason |
| `subscription.cancelled` | store (apply `cancel`, `cancel_subscription`, refund) | subscription id, reason |
| `subscription.entitlement_changed` | store (grant/revoke for module products) | subscription id, module id, entitled |
| `subscription.past_due` | store (apply `mark_past_due`) | subscription id |
| `subscription.renewal_reminder` | billing (scan) | subscription id, period end |
| `subscription.renewal_due` | billing (`request_renewal_charge`) | subscription id, order id, invoice id, user id, amount, currency, retry |
| `subscription.proration_invoiced` | billing (`on_subscription_synced`, proration invoice) | subscription id, invoice id, credit/charge/net |
| `subscription.lifecycle.apply` | billing (scan, `maybe_renew_subscription`, dunning) | subscription id, action, period dates / reason |
| `billing.subscriptions.scan_completed` | billing (scan) | summary counts + details |
| `invoice.created` | billing (renewal + proration invoices; existing event, reused) | invoice record |
| `notification.requested` | billing (reminders, dunning) | user id, title, body, kind |

Consumed by this batch: `payment.captured`, `payment.failed`,
`subscription.created` / `renewed` / `trial_ended` / `upgraded` /
`downgraded` / `past_due` / `cancelled` (billing mirror sync),
`cleanup.requested` (billing runs the scan), `billing.subscriptions.scan`,
`subscription.lifecycle.apply` (store side),
`subscription.module_entitlement.check` (store side).

Event-name correction (sibling builder, verified against source 2026-11-22):
billing emits `invoice.created`, NOT `billing.invoice_created`; the store
emits `order.paid` and `store.order.completed`, NOT `store.order_paid`.
This batch has no `tax.rs` and uses only the real names — verified by grep
over products.rs / store.rs / billing.rs: 0 occurrences of
`billing.invoice_created`, `billing.order_created`, or `store.order_paid`;
`invoice.created` confirmed at billing.rs lines 1011, 1071, 1847;
`order.paid` at store.rs lines 1580, 4338, 4423; `store.order.completed` at
store.rs lines 4341, 4471. Nothing was renamed.

## 9. Verification status (builder's own checks)

- Re-read each file fully before editing; after edits, re-read every
  touched region to confirm no concurrent coordinator change was
  clobbered. No unexpected concurrent changes were observed in the three
  files.
- Greps (0 hits each): Indonesian words (word-boundary list) over all
  three `.rs` files + both migration files + this note; forbidden
  reference-project name over the same set; `todo!` / `unimplemented!` /
  `panic!` / `unreachable!` over the three `.rs` files.
- Delimiter balance: an early tokenizer run disagreed with itself
  (crude regex-stripper vs Rust-aware scanner). Root cause found and
  fixed in the checker, not the code: the scanner did `i += m.end()` on
  char-literal matches, where `m.end()` is absolute — it skipped code
  after every `'.'` / `'0'` literal. With the corrected tokenizer
  (`i = m.end()`), all three files balance exactly: `{` 0, `(` 0, `[` 0.
  One REAL imbalance was found by read-back during verification (the big
  `BillingState` impl insertion had dropped the closing `}` of
  `impl BillingState`) and fixed; the region now reads fn-close, impl-close
  before the `// Helpers` section.
- Read-back verified: products constants (~83–93), validators (~1620–1677),
  `parse_money_amount` (~161–211), `get_subscription_plan` (~2616–2657);
  store `Subscription` struct (~441–501), helpers (~902–990),
  `on_subscription_lifecycle_apply` start (~3715–3800),
  `on_subscription_module_entitlement_check` (~3924–3977), routes +
  `route_info` (~5407–5470); billing types (~207–295), scan start
  (~996–1076), hook handlers (~2826–3000), `request_notification` /
  `request_renewal_charge` / `maybe_renew_subscription` (~907–995),
  `register_hooks` (~3213–3275), routes + `route_info` (~3297–3331).
- NOT verified: `cargo check` / `rustfmt` / `cargo test` — no toolchain
  exists in this VM (`which cargo rustfmt` finds nothing; crates.io is
  blocked). No live runtime test (no running server / database in this
  environment). The `update_order` `"paid"` branch activation wiring and
  the three paid paths were verified by reading the call sites, not by
  execution.

## 10. OPEN (for the integrator / parent)

1. `cargo check` never run — no toolchain. Compilation is unverified for
   all three files (delimiter balance + read-back only).
2. `src/db/schema.rs` + `src/db/models.rs` + `README.md` + `TODO.md` wiring
   from §1–4 is integrator work (this builder is forbidden to touch those
   files).
3. Licensing hook call from §5 — one insert in `check_module_entitlement`
   (licensing.rs, after the license check, before deny) + doc-comment
   renumbering. Without it, subscription entitlements are answered by the
   store but never consulted by licensing.
4. Payments `subscription.renewal_due` listener from §6 — without it,
   renewal invoices are issued but never charged. Do not enqueue the
   commercial id into scheduler kind `payment.subscription.renew`.
5. Scheduler arm for `billing.subscriptions.scan` (see §4 TODO lines) —
   until one exists, the lifecycle only runs on manual scan / cleanup.
6. Persistence writer for the `subscriptions` / `subscription_events` /
   `dunning_attempts` tables (see §4) — tables exist, nothing inserts yet;
   the in-memory DashMap store is authoritative until then.
7. Migration `000041` exists twice in staging (sibling batches) — the
   integrator must resolve that collision; this batch's `000040` is
   unaffected.
8. Live verification (paid subscription order → subscription → renewal
   invoice → charge → period advance; dunning → past-due → grace expiry →
   cancel; proration invoice on upgrade/downgrade) has not been run
   anywhere — needs a running server + database.
