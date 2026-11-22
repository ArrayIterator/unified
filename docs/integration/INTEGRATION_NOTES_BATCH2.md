# Integration notes — Batch 2 (modules-as-products, AI allowlists, TimeTracker, Storage Manager, Events, Todos)

Written by the batch-2 coordinator. This file is for the INTEGRATOR only:
apply the snippets below to the shared files, in the order given. Do NOT
commit this file as documentation for users; fold the README/TODO lines
into README.md / TODO.md and then delete this file (or keep it under
docs/ if the project wants a paper trail — either way, the snippets here
are the source of truth for this batch, not a substitute for reading the
module files).

## Batch scope

| Item | Builder | Files | Lines | Migration |
|---|---|---|---|---|
| 1. Module-as-product rule | B1 | `src/core/modules.rs`, `src/modules/licensing.rs`, `src/modules/products.rs`, `src/modules/meetings.rs`, `src/modules/mcp.rs` | +227 / +133 / +119 / +10 / +10 | none |
| 2. AI allowlists | B2 | `src/modules/ai.rs` | +2,617 | `000036_ai_allowlists` |
| 3. TimeTracker | B3 | `src/modules/timetracker.rs` | 6,072 new | `000032_timetracker` |
| 4. Storage Manager | B4 | `src/modules/storage_manager.rs`, `src/core/storage.rs` | 4,313 new / +707 | `000033_storage_manager` |
| 5. Events Manager | B5 | `src/modules/events.rs` | 4,411 new | `000034_events` |
| 6. Todos | B6 | `src/modules/todos.rs` | 3,611 new | `000035_todos` |

Batch-2 totals verified by the coordinator by reading back: 4 new module
files (18,407 lines) + 5 edited files (+3,823 lines across B1/B2/B4 deltas)
= **22,230 Rust lines**; **38 new tables** across migrations 000032–000036;
**201 new routes** (B1: 1 products endpoint, B2: 21, B3: 68, B4: 41, B5: 37,
B6: 33).
Greps over every batch file: 0 forbidden reference-project names,
0 `todo!`/`unimplemented!` macros, 0 Indonesian words (word-boundary).
`cargo check` has NEVER run anywhere (no toolchain, crates.io blocked) —
compilation is NOT verified for any of this. No migration in this batch
has ever been executed against a live PostgreSQL.

Tree state when batch 2 finished (includes parallel batches 3/4, which
share this staging dir): 281 files, 140 `.rs` files, 247,512 Rust lines.
Batch 2's own files are the ones in the table above; do not attribute the
whole-tree totals to this batch.

## Migration order (no collisions — verified)

```
000032_timetracker
000033_storage_manager
000034_events
000035_todos
000036_ai_allowlists
```

Batch 3 started at 000037 and batch 4 at 000041+, so 000036 is unique
(the feared 000036 collision did NOT happen). One numbering oddity
outside this batch, for the integrator's awareness: batch 3 also created
`2026-11-22-000100_calculator`, out of sequence — not this batch's to
renumber, but the migration runner must sort by number, not by name.

## 1. Module registration (`src/modules/mod.rs`)

The PM coordinator has already registered `projects` (72 `pub mod` lines
at batch-2 completion). Add the four new modules in alphabetical order:

```rust
pub mod events;
pub mod storage_manager;
pub mod timetracker;
pub mod todos;
```

And in `register_all` (next to the neighbouring registrations):

```rust
events::register(registry);
storage_manager::register(registry);
timetracker::register(registry);
todos::register(registry);
```

B3's note: put `timetracker::register(registry);` after
`projects::register(registry);`. B5: `events::register(registry);` in the
commerce group. B6: `todos::register(registry);` after
`tasks::register(registry);`. B4: `storage_manager::register(registry);`
after `storage::register(registry);`. Any consistent placement is fine.

`ai` needs NO mod.rs change (already registered). `core/modules.rs`,
`licensing.rs`, `products.rs`, `meetings.rs`, `mcp.rs`, `core/storage.rs`
need no further registration.

## 2. Product-flag trait methods — move inherent methods into `impl Module`

B1 already added the trait methods to `src/core/modules.rs` (lines 339,
347 — verified present):

```rust
fn available_as_product(&self) -> bool { false }
fn product_category(&self) -> Option<&'static str> { None }
```

Four new modules were written BEFORE those trait methods landed, so each
implements them as inherent methods. Move each pair into that module's
`impl Module` block and DELETE the inherent block (same return values):

- `src/modules/timetracker.rs` (~line 5825): `true` / `Some("productivity")`
- `src/modules/storage_manager.rs` (~line 4142): `true` / `Some("infrastructure")`
- `src/modules/events.rs` (~line 2078): `true` / `Some("commerce")`
- `src/modules/todos.rs` (~line 3443): `false` / `None`

`meetings.rs` and `mcp.rs` already have the overrides inside `impl Module`
(B1): meetings → `true` / `Some("communication")`, mcp → `true` /
`Some("ai")`.

ADD the overrides to `src/modules/projects.rs` (parallel PM round — the
integrator owns that file now):

```rust
// projects.rs — productivity
fn available_as_product(&self) -> bool { true }
fn product_category(&self) -> Option<&'static str> { Some("productivity") }
```

## 3. Startup wiring + enable-flow gating (B1 open items)

- Call `ModuleRegistry::validate_product_declarations()` at startup (or
  from the extensions requirements endpoint). It reports
  `invalid_product_declaration` issues (sellable with no/unknown category,
  non-sellable declaring a category). It is NOT called anywhere yet.
- The entitlement gate is NOT wired into the Extensions Manager enable
  flow: `enable_with_dependencies` / `enable_extension` in `extensions.rs`
  carry no company id and no caller identity. The enable path must first
  resolve the acting company and call
  `crate::modules::licensing::check_module_entitlement(ctx, company_id, module_id)`
  before enabling. Rule (implemented in `check_module_entitlement`, in
  order): (1) unknown module → 404, no event; (2) not sellable → OK
  silently; (3) sellable CORE module → implicit owner entitlement, emits
  `module.entitlement.granted` (`reason: "owner_entitlement"`); (4)
  sellable non-core → requires an active, unexpired license whose
  `product` equals the module id or `module:<id>`, else 403 +
  `module.entitlement.denied` (`reason: "no_active_entitlement"`).
- End-to-end purchase → license → enable flow is not yet proven: the
  store must sell a module product whose issued license `product` is the
  module id (or `module:<id>`) for a non-core sellable module to be
  enableable by a customer.
- `GET /api/v1/products/module-offerings` (B1, in products.rs) is intact
  — verified present after batch 3/4 also edited products.rs
  (products.rs grew 4,925 → 5,162 lines from other batches; re-verify the
  endpoint on the final tree anyway).

## 4. Schema + models — `000032_timetracker` (14 tables)

`src/db/schema.rs` — append (types per project convention: `Numeric` for
money, `Timestamptz`, `Uuid`, `Text`/`Varchar`, `Bool`,
`Array<Uuid>`/`Array<Text>`/`Array<Int8>`):

```rust
diesel::table! {
    timetracker_timers (id) {
        id -> Uuid, user_id -> Text, project_id -> Nullable<Uuid>, task_id -> Nullable<Uuid>,
        description -> Nullable<Text>, billable -> Bool, status -> Varchar,
        started_at -> Timestamptz, paused_total_secs -> Int8, paused_at -> Nullable<Timestamptz>,
        idle_total_secs -> Int8, stopped_at -> Nullable<Timestamptz>,
        entry_id -> Nullable<Uuid>, created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_entries (id) {
        id -> Uuid, user_id -> Text, project_id -> Nullable<Uuid>, task_id -> Nullable<Uuid>,
        timer_id -> Nullable<Uuid>, description -> Text, started_at -> Timestamptz,
        ended_at -> Timestamptz, duration_secs -> Int8, billable -> Bool, approval -> Varchar,
        approved_by -> Nullable<Text>, approved_at -> Nullable<Timestamptz>,
        rejection_note -> Nullable<Text>, reason -> Nullable<Text>,
        created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_activity (id) {
        id -> Uuid, user_id -> Text, timer_id -> Nullable<Uuid>, project_id -> Nullable<Uuid>,
        task_id -> Nullable<Uuid>, activity_score -> Int4, idle_secs -> Int8,
        interval_start -> Timestamptz, interval_end -> Timestamptz,
        triggered_auto_pause -> Bool, created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_app_usage (id) {
        id -> Uuid, user_id -> Text, application_name -> Text, domain -> Nullable<Text>,
        window_title_hash -> Nullable<Text>, category -> Varchar, duration_secs -> Int8,
        interval_start -> Timestamptz, interval_end -> Timestamptz,
        created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_screenshots (id) {
        id -> Uuid, user_id -> Text, timer_id -> Nullable<Uuid>, project_id -> Nullable<Uuid>,
        task_id -> Nullable<Uuid>, storage_target -> Text, storage_key -> Text,
        content_sha256 -> Text, content_type -> Varchar, size_bytes -> Int8,
        blurred -> Bool, captured_at -> Timestamptz, created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_rates (id) {
        id -> Uuid, user_id -> Text, currency -> Varchar, rate -> Numeric,
        effective_from -> Timestamptz, effective_to -> Nullable<Timestamptz>,
        created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_timesheets (id) {
        id -> Uuid, user_id -> Text, period_start -> Timestamptz, period_end -> Timestamptz,
        status -> Varchar, entry_ids -> Array<Uuid>, submitted_at -> Nullable<Timestamptz>,
        reviewed_by -> Nullable<Text>, reviewed_at -> Nullable<Timestamptz>,
        review_note -> Nullable<Text>, created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_settings (id) {
        id -> Uuid, scope -> Text, idle_threshold_secs -> Int8, retention_days -> Int8,
        overtime_hours_per_week -> Numeric, overtime_multiplier -> Numeric,
        low_activity_threshold -> Int8, storage_target -> Text, default_currency -> Varchar,
        auto_approve_timer_entries -> Bool, created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_user_settings (id) {
        id -> Uuid, user_id -> Text, screenshots_enabled -> Bool, blur_screenshots -> Bool,
        idle_threshold_secs -> Nullable<Int8>, created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_teams (id) {
        id -> Uuid, company_id -> Uuid, name -> Text, description -> Nullable<Text>,
        manager_id -> Nullable<Text>, created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_team_members (id) {
        id -> Uuid, team_id -> Uuid, user_id -> Text, role -> Varchar,
        joined_at -> Timestamptz, created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_staff_settings (id) {
        id -> Uuid, user_id -> Text, company_id -> Uuid, role -> Varchar,
        manager_id -> Nullable<Text>, screenshots_enabled -> Nullable<Bool>,
        blur_screenshots -> Nullable<Bool>, idle_threshold_secs -> Nullable<Int8>,
        activity_level -> Nullable<Varchar>, timezone -> Nullable<Text>,
        working_hours_start -> Nullable<Text>, working_hours_end -> Nullable<Text>,
        created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_staff_invites (id) {
        id -> Uuid, company_id -> Uuid, email -> Text, role -> Varchar,
        team_id -> Nullable<Uuid>, invited_by -> Text, token_hash -> Text, status -> Varchar,
        expires_at -> Timestamptz, accepted_at -> Nullable<Timestamptz>,
        accepted_user_id -> Nullable<Text>, created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
diesel::table! {
    timetracker_buyer_settings (id) {
        id -> Uuid, company_id -> Uuid, screenshots_enabled -> Nullable<Bool>,
        screenshot_interval_secs -> Nullable<Int8>, activity_level -> Nullable<Varchar>,
        idle_timeout_secs -> Nullable<Int8>, blur_screenshots -> Nullable<Bool>,
        allowed_apps -> Array<Text>, blocked_apps -> Array<Text>,
        allowed_websites -> Array<Text>, blocked_websites -> Array<Text>,
        default_hourly_rate -> Nullable<Numeric>, timezone -> Nullable<Text>,
        working_hours_start -> Nullable<Text>, working_hours_end -> Nullable<Text>,
        working_days -> Array<Int8>, retention_days -> Nullable<Int8>,
        created_at -> Timestamptz, updated_at -> Timestamptz,
    }
}
```

Add all 14 tables to `allow_tables_to_appear_in_same_query!`, plus
`diesel::joinable!` lines: `timetracker_timers -> timetracker_entries
(entry_id)`, `timetracker_teams -> companies (company_id)`,
`timetracker_staff_settings -> companies (company_id)`,
`timetracker_staff_invites -> companies (company_id)`,
`timetracker_buyer_settings -> companies (company_id)`,
`timetracker_team_members -> timetracker_teams (team_id)`,
`timetracker_staff_invites -> timetracker_teams (team_id)`.

`src/db/models.rs` — mirror the in-module structs (`TimeTrackerTimer`,
`TimeEntry`, `ActivitySample`, `AppUsageRecord`, `ScreenshotRecord`,
`HourlyRate`, `Timesheet`, `TrackerSettings`, `UserTrackerSettings`,
`Team`, `TeamMember`, `StaffSettings`, `StaffInvite`, `BuyerSettings` —
same field names/types, `Uuid`/`DateTime<Utc>`/`String`/`bool`/`i64`/
`Vec<Uuid>`/`Vec<String>`/`Vec<i64>` as in the module). Money fields
(`rate`, `default_hourly_rate`, `overtime_hours_per_week`,
`overtime_multiplier`) use `diesel::data_types::PgNumeric` and, per the
models.rs money convention, those structs skip the serde derives (no
`Serialize`/`Deserialize` on `HourlyRate`, `TrackerSettings`,
`BuyerSettings` persistence structs; the API layer formats money
explicitly). `New*` structs carry every column except `created_at`/
`updated_at` and except columns the DB fills with a DEFAULT.

## 5. Schema + models — `000033_storage_manager` (7 tables)

`src/db/schema.rs` — full `diesel::table!` blocks (column types from the
migration; expanded from the builder's condensed blocks):

```rust
diesel::table! {
    storage_buckets (id) {
        id -> Uuid,
        name -> Varchar,
        provider_kind -> Varchar,
        endpoint -> Varchar,
        region -> Varchar,
        container -> Varchar,
        path -> Varchar,
        sftp_host -> Varchar,
        sftp_port -> Int4,
        sftp_username -> Varchar,
        sftp_auth_kind -> Varchar,
        sftp_keyfile -> Varchar,
        allowed_hosts -> Array<Text>,
        credentials_encrypted -> Nullable<Text>,
        key_fingerprint -> Nullable<Varchar>,
        encryption_at_rest -> Bool,
        enabled -> Bool,
        company_id -> Nullable<Varchar>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_bindings (id) {
        id -> Uuid,
        module_id -> Varchar,
        bucket_id -> Uuid,
        key_prefix -> Varchar,
        company_id -> Nullable<Varchar>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_quotas (id) {
        id -> Uuid,
        scope -> Varchar,
        scope_id -> Varchar,
        bytes_limit -> Nullable<Int8>,
        objects_limit -> Nullable<Int8>,
        soft_bytes_limit -> Nullable<Int8>,
        soft_objects_limit -> Nullable<Int8>,
        used_bytes -> Int8,
        used_objects -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_usage_daily (id) {
        id -> Uuid,
        date -> Date,
        bucket_id -> Uuid,
        company_id -> Nullable<Varchar>,
        user_id -> Varchar,
        module_id -> Varchar,
        bytes_total -> Int8,
        objects_total -> Int8,
        bytes_delta -> Int8,
        objects_delta -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_lifecycle_rules (id) {
        id -> Uuid,
        bucket_id -> Uuid,
        retention_days -> Int8,
        auto_delete -> Bool,
        archive_transition -> Bool,
        archive_prefix -> Varchar,
        enabled -> Bool,
        last_run_at -> Nullable<Timestamptz>,
        last_run_summary -> Nullable<Text>,
        last_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_signed_url_revocations (id) {
        id -> Uuid,
        bucket_id -> Uuid,
        key -> Varchar,
        issued_by -> Varchar,
        expires_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
        revoke_reason -> Nullable<Varchar>,
        provider_url_issued -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    storage_health_checks (id) {
        id -> Uuid,
        bucket_id -> Uuid,
        ok -> Bool,
        latency_ms -> Int8,
        detail -> Varchar,
        checked_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

`src/db/models.rs` — structs mirroring the module structs field-for-field
(`StorageBucket`, `StorageBinding`, `StorageQuota`, `UsageDaily`,
`LifecycleRule`, `SignedUrlRecord`, `HealthCheck` in
`src/modules/storage_manager.rs`; copy the derive /
`#[diesel(table_name = …)]` block from the AI structs, e.g.
`AiProviderConnection`, and mark `credentials_encrypted`
`#[serde(skip_serializing)]`). No money columns in this migration
(quotas are BIGINT bytes/counts).

## 6. Schema + models — `000034_events` (7 tables)

`src/db/schema.rs` (following the `meeting_rooms` `diesel::table!` shape):

```rust
diesel::table! {
    events (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        slug -> Varchar,
        category -> Array<Text>,
        tags -> Array<Text>,
        venue_name -> Nullable<Varchar>,
        venue_address -> Nullable<Varchar>,
        venue_latitude -> Nullable<Float8>,
        venue_longitude -> Nullable<Float8>,
        venue_capacity -> Nullable<Int8>,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        timezone -> Varchar,
        visibility -> Varchar,
        status -> Varchar,
        organizer_id -> Varchar,
        company_id -> Nullable<Varchar>,
        cover_storage_target -> Nullable<Varchar>,
        cover_storage_key -> Nullable<Varchar>,
        recurrence -> Nullable<Jsonb>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    event_occurrences (id) {
        id -> Uuid,
        event_id -> Uuid,
        sequence -> Int4,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        status -> Varchar,
        venue_name -> Nullable<Varchar>,
        venue_address -> Nullable<Varchar>,
        venue_latitude -> Nullable<Float8>,
        venue_longitude -> Nullable<Float8>,
        venue_capacity -> Nullable<Int8>,
        overridden -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    event_ticket_types (id) {
        id -> Uuid,
        event_id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        price -> Numeric,
        currency -> Varchar,
        quantity -> Int8,
        sold -> Int8,
        sales_start -> Nullable<Timestamptz>,
        sales_end -> Nullable<Timestamptz>,
        per_order_limit -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    event_registrations (id) {
        id -> Uuid,
        event_id -> Uuid,
        occurrence_id -> Nullable<Uuid>,
        ticket_type_id -> Uuid,
        user_id -> Varchar,
        quantity -> Int8,
        total_amount -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        order_id -> Nullable<Varchar>,
        payment_reference -> Nullable<Varchar>,
        payment_provider -> Nullable<Varchar>,
        check_in_code -> Varchar,
        checked_in_at -> Nullable<Timestamptz>,
        cancelled_at -> Nullable<Timestamptz>,
        refund_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    event_attendees (id) {
        id -> Uuid,
        event_id -> Uuid,
        registration_id -> Uuid,
        name -> Varchar,
        email -> Varchar,
        custom_answers -> Jsonb,
        checked_in_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    event_waitlist (id) {
        id -> Uuid,
        event_id -> Uuid,
        ticket_type_id -> Uuid,
        user_id -> Varchar,
        quantity -> Int8,
        position -> Int8,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::table! {
    event_reminders (id) {
        id -> Uuid,
        event_id -> Uuid,
        offset_minutes -> Int8,
        occurrence_id -> Nullable<Uuid>,
        title_template -> Nullable<Varchar>,
        body_template -> Nullable<Text>,
        status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        sent_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
diesel::joinable!(event_occurrences -> events (event_id));
diesel::joinable!(event_ticket_types -> events (event_id));
diesel::joinable!(event_registrations -> events (event_id));
diesel::joinable!(event_registrations -> event_ticket_types (ticket_type_id));
diesel::joinable!(event_attendees -> event_registrations (registration_id));
diesel::joinable!(event_waitlist -> events (event_id));
diesel::joinable!(event_reminders -> events (event_id));
diesel::allow_tables_to_appear_in_same_query!(
    events, event_occurrences, event_ticket_types, event_registrations,
    event_attendees, event_waitlist, event_reminders,
);
```

Also add the 7 tables to the schema's table list alongside the other
modules' tables.

`src/db/models.rs` (money as `diesel::data_types::PgNumeric`, per the
models.rs note — money structs skip serde derives like the store models;
`New*` structs carry every column except `created_at`/`updated_at` and
DB-defaulted columns):

```rust
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = events)]
pub struct EventRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub category: Vec<String>,
    pub tags: Vec<String>,
    pub venue_name: Option<String>,
    pub venue_address: Option<String>,
    pub venue_latitude: Option<f64>,
    pub venue_longitude: Option<f64>,
    pub venue_capacity: Option<i64>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub timezone: String,
    pub visibility: String,
    pub status: String,
    pub organizer_id: String,
    pub company_id: Option<String>,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
    pub recurrence: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = event_occurrences)]
pub struct EventOccurrenceRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub sequence: i32,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub venue_name: Option<String>,
    pub venue_address: Option<String>,
    pub venue_latitude: Option<f64>,
    pub venue_longitude: Option<f64>,
    pub venue_capacity: Option<i64>,
    pub overridden: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = event_ticket_types)]
pub struct EventTicketTypeRow { // money: PgNumeric, no serde derive
    pub id: Uuid,
    pub event_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub price: PgNumeric,
    pub currency: String,
    pub quantity: i64,
    pub sold: i64,
    pub sales_start: Option<DateTime<Utc>>,
    pub sales_end: Option<DateTime<Utc>>,
    pub per_order_limit: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = event_registrations)]
pub struct EventRegistrationRow { // money: PgNumeric, no serde derive
    pub id: Uuid,
    pub event_id: Uuid,
    pub occurrence_id: Option<Uuid>,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub quantity: i64,
    pub total_amount: PgNumeric,
    pub currency: String,
    pub status: String,
    pub order_id: Option<String>,
    pub payment_reference: Option<String>,
    pub payment_provider: Option<String>,
    pub check_in_code: String,
    pub checked_in_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub refund_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = event_attendees)]
pub struct EventAttendeeRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub registration_id: Uuid,
    pub name: String,
    pub email: String,
    pub custom_answers: Value,
    pub checked_in_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = event_waitlist)]
pub struct EventWaitlistRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub ticket_type_id: Uuid,
    pub user_id: String,
    pub quantity: i64,
    pub position: i64,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = event_reminders)]
pub struct EventReminderRow {
    pub id: Uuid,
    pub event_id: Uuid,
    pub offset_minutes: i64,
    pub occurrence_id: Option<Uuid>,
    pub title_template: Option<String>,
    pub body_template: Option<String>,
    pub status: String,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

## 7. Schema + models — `000035_todos` (6 tables)

`src/db/schema.rs` — append (following the existing `diesel::table!`
style; `tags`/`subtasks`/`attachments` are `Jsonb`):

```rust
diesel::table! {
    todo_lists (id) {
        id -> Uuid,
        owner_id -> Varchar,
        name -> Varchar,
        color -> Varchar,
        icon -> Varchar,
        archived -> Bool,
        sort_order -> Integer,
        is_default -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todos (id) {
        id -> Uuid,
        list_id -> Uuid,
        owner_id -> Varchar,
        title -> Varchar,
        notes -> Nullable<Text>,
        due_at -> Nullable<Timestamptz>,
        timezone -> Varchar,
        priority -> Varchar,
        status -> Varchar,
        tags -> Jsonb,
        subtasks -> Jsonb,
        attachments -> Jsonb,
        recurrence_frequency -> Nullable<Varchar>,
        recurrence_interval -> Integer,
        recurrence_count -> Nullable<Integer>,
        recurrence_until -> Nullable<Timestamptz>,
        recurrence_remaining -> Nullable<Integer>,
        recurrence_group_id -> Nullable<Uuid>,
        position -> Integer,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_subtasks (id) {
        id -> Uuid,
        todo_id -> Uuid,
        title -> Varchar,
        done -> Bool,
        position -> Integer,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_shares (id) {
        id -> Uuid,
        list_id -> Uuid,
        owner_id -> Varchar,
        shared_with_user_id -> Varchar,
        permission -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_completion_log (id) {
        id -> Uuid,
        todo_id -> Uuid,
        user_id -> Varchar,
        action -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    todo_reminders (id) {
        id -> Uuid,
        todo_id -> Uuid,
        owner_id -> Varchar,
        offset_minutes -> BigInt,
        remind_at -> Timestamptz,
        due_at -> Timestamptz,
        status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        sent_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

Also update the schema header comment's migration range from
`000001_core`–`000031_projects` to include `000035_todos`.

`src/db/models.rs` — append, in the existing style (`Queryable,
Selectable, Serialize` read struct + `Insertable, Deserialize` `New*`
struct carrying every column except `created_at`/`updated_at` and
DB-defaulted columns; PK ids are carried):

```rust
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_lists)]
pub struct TodoListRow {
    pub id: Uuid,
    pub owner_id: String,
    pub name: String,
    pub color: String,
    pub icon: String,
    pub archived: bool,
    pub sort_order: i32,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_lists)]
pub struct NewTodoListRow {
    pub id: Uuid,
    pub owner_id: String,
    pub name: String,
    pub color: String,
    pub icon: String,
    pub sort_order: i32,
    pub is_default: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todos)]
pub struct TodoRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub title: String,
    pub notes: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub priority: String,
    pub status: String,
    pub tags: Value,
    pub subtasks: Value,
    pub attachments: Value,
    pub recurrence_frequency: Option<String>,
    pub recurrence_interval: i32,
    pub recurrence_count: Option<i32>,
    pub recurrence_until: Option<DateTime<Utc>>,
    pub recurrence_remaining: Option<i32>,
    pub recurrence_group_id: Option<Uuid>,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todos)]
pub struct NewTodoRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub title: String,
    pub notes: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub priority: String,
    pub status: String,
    pub tags: Value,
    pub subtasks: Value,
    pub attachments: Value,
    pub recurrence_frequency: Option<String>,
    pub recurrence_interval: i32,
    pub recurrence_count: Option<i32>,
    pub recurrence_until: Option<DateTime<Utc>>,
    pub recurrence_remaining: Option<i32>,
    pub recurrence_group_id: Option<Uuid>,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_subtasks)]
pub struct TodoSubtaskRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub title: String,
    pub done: bool,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_subtasks)]
pub struct NewTodoSubtaskRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub title: String,
    pub done: bool,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_shares)]
pub struct TodoShareRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub shared_with_user_id: String,
    pub permission: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_shares)]
pub struct NewTodoShareRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub shared_with_user_id: String,
    pub permission: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_completion_log)]
pub struct TodoCompletionLogRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub user_id: String,
    pub action: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_completion_log)]
pub struct NewTodoCompletionLogRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub user_id: String,
    pub action: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_reminders)]
pub struct TodoReminderRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub owner_id: String,
    pub offset_minutes: i64,
    pub remind_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub status: String,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_reminders)]
pub struct NewTodoReminderRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub owner_id: String,
    pub offset_minutes: i64,
    pub remind_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub status: String,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
}
```

Also update the models.rs header comment's migration range (currently
000001–000031) to include 000035.

## 8. Schema + models — `000036_ai_allowlists` (4 tables + ALTERs)

`src/db/schema.rs` — add after the 000030_ai tables (and add the four
table names to the `allow_tables_to_appear_in_same_query!` list; add the
`joinable!` lines after the existing AI ones):

```rust
diesel::table! {
    ai_company_policies (id) {
        id -> Uuid,
        company_id -> Uuid,
        allowed_models -> Nullable<Jsonb>,
        allowed_features -> Nullable<Jsonb>,
        monthly_token_cap -> Nullable<Int8>,
        monthly_cost_cap_usd -> Nullable<Numeric>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_user_policies (id) {
        id -> Uuid,
        user_id -> Uuid,
        company_id -> Uuid,
        allowed_models -> Nullable<Jsonb>,
        allowed_features -> Nullable<Jsonb>,
        monthly_token_cap -> Nullable<Int8>,
        monthly_cost_cap_usd -> Nullable<Numeric>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_personal_keys (id) {
        id -> Uuid,
        user_id -> Uuid,
        provider_kind -> Varchar,
        base_url -> Nullable<Varchar>,
        model -> Nullable<Varchar>,
        api_key_encrypted -> Text,
        key_fingerprint -> Varchar,
        monthly_token_cap -> Nullable<Int8>,
        monthly_cost_cap_usd -> Nullable<Numeric>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    ai_user_mcp_servers (id) {
        id -> Uuid,
        user_id -> Uuid,
        name -> Varchar,
        endpoint_url -> Varchar,
        auth_token_encrypted -> Nullable<Text>,
        token_fingerprint -> Nullable<Varchar>,
        enabled -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

In the existing `ai_usage_records` `table!` block, add:

```rust
        user_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        denial_reason -> Nullable<Varchar>,
        used_personal_key -> Bool,
```

`joinable!` additions:

```rust
diesel::joinable!(ai_company_policies -> companies (company_id));
diesel::joinable!(ai_user_policies -> users (user_id));
diesel::joinable!(ai_user_policies -> companies (company_id));
diesel::joinable!(ai_personal_keys -> users (user_id));
diesel::joinable!(ai_user_mcp_servers -> users (user_id));
diesel::joinable!(ai_usage_records -> users (user_id));
diesel::joinable!(ai_usage_records -> companies (company_id));
```

`src/db/models.rs` — add after the 000030_ai models. Money fields use
`PgNumeric` (per the existing models.rs money note: those structs derive
`Queryable`/`Selectable` only, no `Serialize`/`Deserialize`):

```rust
// AI access allowlists (2026-11-22-000036_ai_allowlists).

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = ai_company_policies)]
pub struct AiCompanyPolicy {
    pub id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_company_policies)]
pub struct NewAiCompanyPolicy {
    pub id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = ai_user_policies)]
pub struct AiUserPolicy {
    pub id: Uuid,
    pub user_id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_user_policies)]
pub struct NewAiUserPolicy {
    pub id: Uuid,
    pub user_id: Uuid,
    pub company_id: Uuid,
    pub allowed_models: Option<Value>,
    pub allowed_features: Option<Value>,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = ai_personal_keys)]
pub struct AiPersonalKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider_kind: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    #[serde(skip_serializing)]
    pub api_key_encrypted: String,
    pub key_fingerprint: String,
    pub monthly_token_cap: Option<i64>,
    pub monthly_cost_cap_usd: Option<PgNumeric>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_personal_keys)]
pub struct NewAiPersonalKey {
    pub id: Uuid,
    pub user_id: Uuid,
    pub provider_kind: String,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key_encrypted: String,
    pub key_fingerprint: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = ai_user_mcp_servers)]
pub struct AiUserMcpServer {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub endpoint_url: String,
    #[serde(skip_serializing)]
    pub auth_token_encrypted: Option<String>,
    pub token_fingerprint: Option<String>,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = ai_user_mcp_servers)]
pub struct NewAiUserMcpServer {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub endpoint_url: String,
    pub auth_token_encrypted: Option<String>,
    pub token_fingerprint: Option<String>,
}
```

In the existing `AiUsageRecord` (models.rs) add `pub user_id:
Option<Uuid>`, `pub company_id: Option<Uuid>`, `pub denial_reason:
Option<String>`, `pub used_personal_key: bool`; in `NewAiUsageRecord`
add the same four fields (`used_personal_key: bool`).

## 9. Scheduler arms (`src/modules/scheduler.rs` — integrator only)

Add all four kinds to `KNOWN_KINDS` and arm each dispatch (adapt to the
match arm's exact shape; the snippets below are the builders' words):

```rust
// KNOWN_KINDS additions:
"timetracker.screenshots.purge",
"storage.lifecycle.run",
"events.reminders.send",
"todos.reminders.send",
```

TimeTracker purge:

```rust
"timetracker.screenshots.purge" => {
    state.emit("timetracker.screenshots.purge_requested", payload.clone());
    Ok(json!({"dispatched": true, "event": "timetracker.screenshots.purge_requested"}))
}
```

Storage lifecycle (next to the `"backup.run"` arm):

```rust
"storage.lifecycle.run" => {
    ctx.hooks.emit("storage.lifecycle.run_requested", payload);
}
```

Events reminders:

```rust
// 1. Add to KNOWN_KINDS: "events.reminders.send"
// 2. In on_scheduler_enqueue's event->kind mapping, add:
"events.reminders.send_requested" => "events.reminders.send".to_string(),
// 3. In the job dispatch, add:
"events.reminders.send" => {
    ctx.hooks.emit("events.reminders.send_requested", payload.clone());
}
```

Todos reminders:

```rust
// In KNOWN_KINDS, add: "todos.reminders.send"
// In dispatch(), add:
"todos.reminders.send" => {
    state.emit("todos.reminders.send_requested", payload.clone());
    Ok(json!({ "dispatched": true, "event": "todos.reminders.send_requested" }))
}
// In on_scheduler_enqueue()'s event -> kind mapping, add:
"todos.reminders.send_requested" => "todos.reminders.send".to_string(),
```

Until the arms land, all four features record `declined` honestly and
offer a manual run endpoint (`POST /screenshots/purge`,
`POST /lifecycle/run`, `POST /{id}/reminders/{reminder_id}/send`); the
storage lifecycle `run_requested` handler already executes on its own via
a spawned worker, so its arm only needs to emit the event.

## 10. Audit events (`src/modules/audit.rs` `AUDITED_EVENTS`)

Add, following the existing `ai.*` pattern:

```
ai.access.denied
ai.personal_key.created
ai.personal_key.rotated
ai.personal_key.deleted
ai.access.policy_changed
timetracker.timer.started
timetracker.timer.stopped
timetracker.timer.paused
timetracker.timer.resumed
timetracker.timer.auto_paused
timetracker.time.logged
timetracker.entry.updated
timetracker.entry.deleted
timetracker.entry.approved
timetracker.entry.rejected
timetracker.timesheet.created
timetracker.timesheet.submitted
timetracker.timesheet.approved
timetracker.timesheet.rejected
timetracker.staff.added
timetracker.staff.updated
timetracker.staff.removed
timetracker.staff.invite.created
timetracker.staff.invite.accepted
timetracker.staff.invite.revoked
timetracker.team.created
timetracker.team.deleted
timetracker.team.member.added
timetracker.team.member.removed
timetracker.settings.updated
```

(Events + Todos emit their own `*.created/updated/...` events through the
same bus; add their names to `AUDITED_EVENTS` too if the audit module
whitelists by name — check the list when wiring.)

## 11. Billing listener — `timetracker.time.logged`

`billing.rs` currently listens only to `order.created`,
`payment.captured`, `payment.refunded`. Add a `timetracker.time.logged`
handler using this payload (emitted by the TimeTracker on every timer
stop and approved manual entry):

```json
{"entry_id","user_id","timer_id","project_id","task_id","description",
 "started_at","ended_at","duration_secs","hours","billable","approval",
 "currency","hourly_rate","amount","source"}
```

`source` is `"timer"` or `"manual"`; `hours = duration_secs/3600`;
`amount = hours × hourly_rate` (rounded 4dp; 0 when non-billable).
Non-billable entries still emit with `"billable": false`. Billable hours
feed invoicing; do NOT import `project.time.logged` into billing as well
(the TimeTracker acks it only — importing it would double-count).

## 12. Hook answerers still missing (integrator / other modules)

- `timetracker.company.membership.check` (`{"company_id","user_id"}` →
  `{"member": true}`) and `timetracker.team.check`
  (`{"manager_id","user_id"}` → `{"manages": true}`): defined and
  fail-closed (unanswered = denied). The company/team side must implement
  them, or all cross-user and staff scoping in the TimeTracker stays
  denied. `company.exists` already exists.
- `user.company_requested` (`{"user_id"}` → `{"company_id"}`): the AI
  module asks this hook to resolve a caller's company. The users module
  does not answer it yet; until it does, callers without the
  `X-Company-Id` header resolve to no company (platform-admin treatment
  for policy CRUD; denial with `ai_company_unknown` for AI calls once
  enforcement is on).
- `todos.ai.draft_requested` (`{"user_id","role","kind":"summarize"|
  "plan-day","todo_ids","context_text","requested_at"}`): no handler
  answers it yet, so `/api/v1/todos/ai/summarize` and `/ai/plan-day`
  correctly answer 503 until the AI module registers the handler
  documented in the todos module header. The todos module answers
  `todos.context_requested` (own todos only) for the AI side to consume.
- `project.task.*` listeners in the TimeTracker were written against the
  audit-listed event names only (`project.created`, `project.task.created`,
  `project.task.completed`, `project.time.logged`) because
  `src/modules/projects.rs` did not exist when B3 wrote them. `projects.rs`
  has since landed (408 KB) — VERIFY the event names/payloads against the
  real projects module; nothing in the TimeTracker imports or calls it.

## 13. API Manager rules + roles capabilities

- Events public routes need admin-created `ApiAccessRule` rows
  (`public_access: true`) for the 5 public paths, or they stay denied by
  default (the API Manager gate is default-deny; there is no reusable
  per-request public-access helper to call from a handler):
  `GET /api/v1/events/public`, `GET /api/v1/events/public/{slug}`,
  `GET /api/v1/events/public/{slug}/ics`,
  `GET /api/v1/events/public/calendar.ics`,
  `GET /api/v1/events/public/calendar.json`.
- Capabilities that must exist for `role.check` to grant them
  (fail-closed otherwise): `events.view`, `events.create`,
  `events.manage`, `events.checkin`, `timetracker:view_team_screenshots`,
  `timetracker:manage_buyer_settings`, `todos:read`, `todos:write`,
  `todos:share`, `products:read` (new, added by B1 for the
  module-offerings endpoint).

## 14. README.md / TODO.md lines (integrator to add)

README (module list):

```markdown
- Module-as-product rule: every module declares `available_as_product` and `product_category` (closed set: productivity, communication, commerce, infrastructure, ai). Sellable modules are listed at `GET /api/v1/products/module-offerings`; enabling a sellable non-core module requires a license entitlement (see `licensing::check_module_entitlement`).
- AI access allowlists: company/user policies, personal API keys and personal MCP servers gate every completion and draft via `authorize_ai_call`; see `src/modules/ai.rs` and migration `2026-11-22-000036_ai_allowlists`.
- TimeTracker — Time Doctor-style time tracking (timers, manual entries, approvals, activity/app-usage monitoring, screenshots with consent and retention, rates, payroll with overtime, timesheets, reports, team monitoring, per-company staff/teams, per-buyer settings). Sellable product, category "productivity". The desktop/mobile agent app is a planned future product; the server agent API under `/api/v1/timetracker/agent/*` is what it will call.
- Storage Manager (`storage_manager`): bucket registry, per-module bindings with a module → company-default → global-default fallback chain, quotas (soft warn + hard 409), file browser, usage stats, lifecycle rules, revocable signed URLs, and health checks on top of the local, S3, R2, Google Drive, and SFTP providers. Available as a product (`infrastructure`).
- SFTP is a new storage provider kind (`sftp`): system OpenSSH `sftp` client in batch mode, keyfile or password auth (password via `sshpass` + `SSHPASS` env only), allowlisted hosts only.
- Events Manager — events with venues, recurring series, ticket types, registrations, waitlists, attendee check-in, reminders, and iCalendar/JSON calendar feeds. Paid registrations confirm on `payment.captured`; refunds are requested via `events.registration.refund_requested` and processed by billing/store.
- Todos (`src/modules/todos.rs`, migration `2026-11-22-000035_todos`): personal per-user todos — lists with a default Inbox, quick capture, due dates with reminders, priorities, tags, one-level subtasks, storage-key attachments, read/edit list sharing (ownership never transfers), recurring todos, completion history with computed streaks, and AI draft summaries/day plans. Personal utility, not a product (`available_as_product() -> false`). Strictly separate from the shared/assigned tasks module. In-memory DashMap stores are authoritative; the six tables (`todo_lists`, `todos`, `todo_subtasks`, `todo_shares`, `todo_completion_log`, `todo_reminders`) are the persistence target.
```

TODO (append to the batch-2 section):

```markdown
- Batch 2 integration: register timetracker/storage_manager/events/todos in mod.rs; add schema/models for migrations 000032–000036; move the four inherent available_as_product/product_category pairs into impl Module; add projects.rs product overrides; wire validate_product_declarations at startup; gate the Extensions Manager enable flow on check_module_entitlement; add scheduler arms (timetracker.screenshots.purge, storage.lifecycle.run, events.reminders.send, todos.reminders.send); add ai.* + timetracker.* to AUDITED_EVENTS; wire billing to timetracker.time.logged; implement timetracker.company.membership.check + timetracker.team.check + user.company_requested answerers + the todos.ai.draft_requested AI-side handler; create ApiAccessRule rows for the events public paths; run cargo check; execute migrations 000032–000036 against a live PostgreSQL and then wire the read paths (all batch-2 stores are in-memory authoritative; migrations are persistence targets).
- TimeTracker: desktop/mobile agent app is FUTURE (not built); the server agent API is the contract it will use. Per-buyer settings + staff management are in the module (migration 000032).
- Storage Manager: S3/R2 server-side encryption is not sent by the provider (see the module header matrix); SFTP needs the OpenSSH sftp binary (+ sshpass for password auth); signed-URL revocation blocks hub tokens only, not provider presigned URLs.
- AI allowlists: legacy open mode — while BOTH policy stores are empty, authorize_ai_call allows any ai:use caller (existing deployments keep working); creating the first policy switches enforcement to strict default-deny. Create the company policy BEFORE the first user policy or that company's users lock out immediately. If strict-always is wanted, delete the !enforcement_on early return in authorize_ai_call. Personal MCP servers are grounding references only (no live MCP fetch/tool call is performed).
- Events: roles need events.view/create/manage/checkin; scheduler arm + ApiAccessRule rows pending (see §9/§13).
- Todos: editing recurrence_count on an existing series does not recompute recurrence_remaining (minor, needs a decision, not a guess).
```

## 15. Desktop / mobile TimeTracker app — FUTURE, NOT BUILT

Recorded as required by the parent: no desktop or mobile agent app was
built in this round. The server-side agent API under
`/api/v1/timetracker/agent/*` (API-key authenticated, scoped
`timetracker:write`/`timetracker:read`, owner = key owner) is the contract
the future app will call. The module header, the `GET /api/v1/timetracker`
status response (`"desktop_agent": "not_built"`), and the module
description all say so explicitly. Do not present the TimeTracker as
having a desktop/mobile app until that product is built.

## 16. OPEN list (everything unfinished or unverified — be honest)

1. `cargo check` NEVER ran anywhere (no toolchain, crates.io blocked).
   Highest-risk spots named by builders: `tokio::task::spawn_blocking`
   in `core/storage.rs`; `Option::is_none_or` (MSRV 1.85); struct-update
   syntax in `StorageTargetConfig`; hand-built `json!` maps in storage
   stats; the 2,617-line ai.rs extension (one mid-task slip already
   caught and fixed by B2); borrow hazards in the 6,072-line
   timetracker.rs (reviewed by hand only).
2. No migration in this batch has ever been executed against a live
   PostgreSQL. Run 000032–000036 on a scratch DB first.
3. No live test of any new endpoint (no running server in this VM):
   module-offerings, AI access/personal-key/personal-MCP, timetracker
   agent API, storage-manager bindings/quotas, events public routes +
   payment listener, todos AI drafts.
4. Actix static-vs-dynamic route precedence for `/public` and
   `/calendar.ics` vs `/{id}` in events.rs is unverified at runtime
   (public/static routes are registered first, but confirm live).
5. `GET /usage` and `GET /usage/{id}` in ai.rs remain capability-gated
   but not ownership-scoped (pre-existing); the new `by-user`/`by-company`
   endpoints are the scoped ones.
6. Cost-cap checks in ai.rs are skipped when the model has no known price
   (`estimate_cost_usd` returns `None`).
7. TimeTracker payroll `total_pay` is a plain cross-currency sum (noted
   in the module); staff/team emits use string literals while `events`
   also defines constants for them (identical values — unify later).
8. Todos recurrence edge: editing `recurrence_count` on an existing
   series does not recompute `recurrence_remaining`.
9. Storage lifecycle archive moves are download+upload+delete through the
   provider contract (no native rename/copy in the trait), capped at
   100 MiB; SFTP has no overall transfer timeout (ConnectTimeout=10
   bounds connect only); `tokio::spawn` in the lifecycle hook handler
   requires a live async runtime at emit time.
10. A shared core crypto helper would be better than the second copy of
    the `v1.` HMAC-keystream construction in storage_manager.rs — left as
    integrator work (noted in the module header).
