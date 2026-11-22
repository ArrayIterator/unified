# Integration notes — Batch 5 (Unified)

Coordinator: batch 5. Nothing here is pushed. The integrator owns
`src/modules/mod.rs`, `src/core/mod.rs`, `src/db/schema.rs`,
`src/db/models.rs`, `README.md`, `TODO.md`, scheduler `KNOWN_KINDS` /
dispatch arms, `AUDITED_EVENTS`, role capability seeds, and
`ApiAccessRule` rows. This file lists the EXACT snippets to apply.

## Files delivered by batch 5

| File | Lines | Routes | Migration |
|---|---|---|---|
| `src/core/sharing.rs` (new) | 1,039 | — (shared machinery) | `000067_sharing_auditor` |
| `src/modules/auditors.rs` (new) | 2,276 | 19 | `000067_sharing_auditor` |
| `src/modules/properties.rs` (new) | 4,704 | 37 | `000060_properties` |
| `src/modules/logistics.rs` (new) | 5,533 | 55 | `000061_logistics` |
| `src/modules/supervision.rs` (new) | 4,199 | 44 | `000062_supervision` |
| `src/modules/ai.rs` (extended, +1,089) | 6,084 → 7,173 | +5 gating routes | `000063_ai_gating` |

Total new Rust: 17,751 lines (plus 1,089 added to `ai.rs`). New routes:
155. New tables: 30 (5 sharing/auditor + 4 properties + 11 logistics +
10 supervision) plus ALTERs on the AI policy tables and one new
`ai_global_settings` table.

## Migration order (renumber history — read this first)

Batch 5 was assigned 000059+. Two collisions with parallel rounds were
found and fixed by the batch-5 coordinator (the other rounds' numbers
were left untouched):

- `000059_sharing_auditor` collided with batch 4's `000059_workflows`
  → renumbered to `000066_sharing_auditor`, then `000066_web3`
  (another round) landed → renumbered again to
  **`000067_sharing_auditor`**. Final number: **000067**.
- The batch-5 sequence in run order is therefore:
  `000059_workflows` (batch 4) → `000060_properties` →
  `000061_logistics` → `000062_supervision` → `000063_ai_gating` →
  `000064_documents` (other round) → `000065_privacy` (other round) →
  `000066_web3` (other round) → `000067_sharing_auditor`.
- Verified 2026-11-22: no duplicate migration numbers remain
  (66 migration dirs). The sharing/auditor migration files, `sharing.rs`,
  and `auditors.rs` contain zero stale `000059`/`000066` references.

## Coordinator verification (independent, by reading back)

- Forbidden reference-project name in batch-5 files: **0**.
- Indonesian words (word-boundary grep, ~40 words incl. kavling/denah/
  tanah/proyek/laporan/pengawasan/logistik/blok/jual/harga): **0**.
- `todo!` / `unimplemented!` in batch-5 files (incl. whole `ai.rs`): **0**.
- Delimiter balance in all 5 new files: 0/0/0.
- Route counts verified 1:1 (`.route(` calls == `RouteInfo {`
  literals): properties 37, logistics 55, supervision 44, auditors 19.
- `ai.rs` sha after B5's edit: `716a66630c407172…` — unchanged since
  B5 finished; `evaluate_ai_gate` (line 3362), `authorize_ai_call`
  (2751), `require_allowed_user` (3154), `AiGlobalSettings` all present.
- Shared files untouched by batch 5: `mod.rs`, `core/mod.rs`,
  `schema.rs`, `models.rs`, `README.md`, `TODO.md` contain zero
  references to the batch-5 modules.
- `cargo check` NEVER ran anywhere (no toolchain, crates.io blocked) —
  compilation is unverified for all five items. Migrations never ran
  against live PostgreSQL. In-memory DashMap stores are authoritative
  everywhere (same caveat as all sibling modules).

## (a) `src/modules/mod.rs`

```rust
pub mod auditors;   // place after `pub mod audit;`, before `pub mod auth_manager;`
pub mod logistics; // alphabetical, after `load_balancer`, before `logs`
pub mod properties; // alphabetical, after `planner`, before `products`
pub mod supervision; // alphabetical list
```

In `register_all`, foundation group after `audit::register(registry);`:

```rust
    audit::register(registry);
    auditors::register(registry);
```

Commerce/Operations group:

```rust
    products::register(registry);
    properties::register(registry);
```

```rust
    logistics::register(registry); // Operations group, e.g. after geo::register(registry);
```

After the projects line (currently line 127):

```rust
    projects::register(registry);
    supervision::register(registry);
```

## (b) `src/core/mod.rs`

```rust
pub mod sharing;   // place after `pub mod routes;`, before `pub mod storage;`
```

## (c) `src/db/schema.rs` — append these `diesel::table!` blocks

### Sharing / auditors (`000067_sharing_auditor`)

```rust
diesel::table! {
    public_links (id) {
        id -> Uuid,
        token_hash -> Varchar,
        scope_kind -> Varchar,
        scope_id -> Uuid,
        scope_view -> Varchar,
        created_by -> Varchar,
        expires_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    public_link_access_log (id) {
        id -> Uuid,
        link_id -> Nullable<Uuid>,
        accessed_at -> Timestamptz,
        ip_hash -> Nullable<Varchar>,
        user_agent_hash -> Nullable<Varchar>,
        allowed -> Bool,
        reason -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    auditor_accounts (id) {
        id -> Uuid,
        user_id -> Varchar,
        label -> Varchar,
        email -> Varchar,
        is_auditor -> Bool,
        auditor_of -> Varchar,
        status -> Varchar,
        expires_at -> Timestamptz,
        password_hash -> Varchar,
        revoked_at -> Nullable<Timestamptz>,
        revoked_by -> Nullable<Varchar>,
        revoke_reason -> Nullable<Text>,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    auditor_scopes (id) {
        id -> Uuid,
        auditor_id -> Uuid,
        module -> Varchar,
        resource_kind -> Varchar,
        resource_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        allow_export -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    auditor_sessions (id) {
        id -> Uuid,
        auditor_id -> Uuid,
        user_id -> Varchar,
        started_at -> Timestamptz,
        last_seen_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
        revoked_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

### Properties (`000060_properties`; money `NUMERIC(19,4)`, geometry `Jsonb`)

```rust
diesel::table! {
    properties (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        slug -> Varchar,
        latitude -> Nullable<Float8>,
        longitude -> Nullable<Float8>,
        address -> Nullable<Varchar>,
        total_area_sqm -> Nullable<Numeric>,
        company_id -> Nullable<Uuid>,
        status -> Varchar,
        cover_storage_target -> Nullable<Varchar>,
        cover_storage_key -> Nullable<Varchar>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    property_plans (id) {
        id -> Uuid,
        property_id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        geometry -> Nullable<Jsonb>,
        geometry_hash -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    property_blocks (id) {
        id -> Uuid,
        plan_id -> Uuid,
        property_id -> Uuid,
        title -> Varchar,
        notes -> Nullable<Text>,
        geometry -> Jsonb,
        geometry_hash -> Varchar,
        area_sqm -> Nullable<Numeric>,
        length_m -> Nullable<Numeric>,
        price -> Numeric,
        currency -> Varchar,
        status -> Varchar,
        assigned_customer_id -> Nullable<Text>,
        click_behavior -> Varchar,
        click_link_kind -> Nullable<Varchar>,
        click_link_target -> Nullable<Text>,
        order_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    property_block_history (id) {
        id -> Uuid,
        block_id -> Uuid,
        property_id -> Uuid,
        plan_id -> Uuid,
        from_status -> Varchar,
        to_status -> Varchar,
        actor_id -> Text,
        reason -> Nullable<Text>,
        order_id -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::joinable!(property_plans -> properties (property_id));
diesel::joinable!(property_blocks -> property_plans (plan_id));
diesel::joinable!(property_blocks -> properties (property_id));
diesel::joinable!(property_block_history -> property_blocks (block_id));
diesel::joinable!(property_block_history -> properties (property_id));
diesel::joinable!(property_block_history -> property_plans (plan_id));
```

### Logistics (`000061_logistics`)

```rust
diesel::table! {
    logistics_drivers (id) {
        id -> Uuid,
        user_id -> Text,
        license_number -> Varchar,
        phone -> Nullable<Varchar>,
        status -> Varchar,
        current_vehicle_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_vehicles (id) {
        id -> Uuid,
        plate -> Varchar,
        vehicle_type -> Varchar,
        capacity_kg -> Double,
        status -> Varchar,
        assigned_driver_id -> Nullable<Uuid>,
        company_id -> Nullable<Uuid>,
        maintenance -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_warehouses (id) {
        id -> Uuid,
        name -> Varchar,
        address -> Varchar,
        capacity -> Int8,
        manager_user_id -> Nullable<Text>,
        default_low_stock_threshold -> Int8,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_shipments (id) {
        id -> Uuid,
        tracking_number -> Varchar,
        tracking_token_hash -> Varchar,
        order_id -> Nullable<Varchar>,
        order_link_status -> Varchar,
        company_id -> Nullable<Uuid>,
        status -> Varchar,
        origin_address -> Varchar,
        origin_latitude -> Nullable<Double>,
        origin_longitude -> Nullable<Double>,
        destination_address -> Varchar,
        destination_latitude -> Nullable<Double>,
        destination_longitude -> Nullable<Double>,
        weight_kg -> Double,
        length_cm -> Double,
        width_cm -> Double,
        height_cm -> Double,
        zone_origin -> Varchar,
        zone_destination -> Varchar,
        service_level -> Varchar,
        driver_id -> Nullable<Uuid>,
        vehicle_id -> Nullable<Uuid>,
        warehouse_id -> Nullable<Uuid>,
        scheduled_at -> Nullable<Timestamptz>,
        window_start -> Nullable<Timestamptz>,
        window_end -> Nullable<Timestamptz>,
        estimated_delivery_at -> Nullable<Timestamptz>,
        shipping_cost -> Nullable<Numeric>,
        currency -> Varchar,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_shipment_events (id) {
        id -> Uuid,
        shipment_id -> Uuid,
        kind -> Varchar,
        from_status -> Nullable<Varchar>,
        to_status -> Nullable<Varchar>,
        actor -> Text,
        location -> Nullable<Varchar>,
        note -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_stock_movements (id) {
        id -> Uuid,
        warehouse_id -> Uuid,
        asset_id -> Uuid,
        direction -> Varchar,
        quantity -> Int8,
        quantity_after -> Int8,
        reason -> Varchar,
        shipment_id -> Nullable<Uuid>,
        actor -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_routes (id) {
        id -> Uuid,
        name -> Varchar,
        driver_id -> Nullable<Uuid>,
        vehicle_id -> Nullable<Uuid>,
        route_date -> Timestamptz,
        start_latitude -> Nullable<Double>,
        start_longitude -> Nullable<Double>,
        status -> Varchar,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_route_stops (id) {
        id -> Uuid,
        route_id -> Uuid,
        shipment_id -> Nullable<Uuid>,
        sequence -> Int8,
        address -> Varchar,
        latitude -> Nullable<Double>,
        longitude -> Nullable<Double>,
        status -> Varchar,
        note -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_proofs (id) {
        id -> Uuid,
        shipment_id -> Uuid,
        storage_target -> Varchar,
        storage_key -> Varchar,
        content_sha256 -> Varchar,
        signature_storage_target -> Nullable<Varchar>,
        signature_storage_key -> Nullable<Varchar>,
        recipient_name -> Varchar,
        delivered_at -> Timestamptz,
        actor -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_exceptions (id) {
        id -> Uuid,
        shipment_id -> Uuid,
        exception_type -> Varchar,
        description -> Text,
        status -> Varchar,
        previous_status -> Varchar,
        resolution -> Nullable<Text>,
        actor -> Text,
        resolved_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    logistics_rate_tables (id) {
        id -> Uuid,
        name -> Varchar,
        origin_zone -> Nullable<Varchar>,
        destination_zone -> Nullable<Varchar>,
        base_price -> Numeric,
        per_kg -> Numeric,
        per_km -> Numeric,
        currency -> Varchar,
        active -> Bool,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

Joinables:

```rust
diesel::joinable!(logistics_shipments -> logistics_drivers (driver_id));
diesel::joinable!(logistics_shipments -> logistics_vehicles (vehicle_id));
diesel::joinable!(logistics_shipments -> logistics_warehouses (warehouse_id));
diesel::joinable!(logistics_shipment_events -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_stock_movements -> logistics_warehouses (warehouse_id));
diesel::joinable!(logistics_stock_movements -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_routes -> logistics_drivers (driver_id));
diesel::joinable!(logistics_routes -> logistics_vehicles (vehicle_id));
diesel::joinable!(logistics_route_stops -> logistics_routes (route_id));
diesel::joinable!(logistics_route_stops -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_proofs -> logistics_shipments (shipment_id));
diesel::joinable!(logistics_exceptions -> logistics_shipments (shipment_id));
```

### Supervision (`000062_supervision`; percents `Numeric`, dates `Date`, SHA-256 `Varchar`)

```rust
diesel::table! {
    supervision_assignments (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        user_id -> Text,
        role -> Text,
        status -> Text,
        starts_at -> Timestamptz,
        ends_at -> Nullable<Timestamptz>,
        notes -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_inspection_templates (id) {
        id -> Uuid,
        project_id -> Nullable<Text>,
        company_id -> Nullable<Uuid>,
        title -> Text,
        description -> Nullable<Text>,
        category -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_template_items (id) {
        id -> Uuid,
        template_id -> Uuid,
        title -> Text,
        description -> Nullable<Text>,
        position -> Int4,
        required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_inspections (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        template_id -> Nullable<Uuid>,
        title -> Text,
        location -> Nullable<Text>,
        inspector_id -> Text,
        status -> Text,
        scheduled_for -> Timestamptz,
        conducted_at -> Nullable<Timestamptz>,
        verdict -> Nullable<Text>,
        summary -> Nullable<Text>,
        signed_by -> Nullable<Text>,
        signed_at -> Nullable<Timestamptz>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_inspection_items (id) {
        id -> Uuid,
        inspection_id -> Uuid,
        template_item_id -> Nullable<Uuid>,
        title -> Text,
        result -> Text,
        notes -> Nullable<Text>,
        follow_up_requested -> Bool,
        follow_up_task_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_progress_reports (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        milestone_id -> Nullable<Text>,
        title -> Text,
        period_start -> Date,
        period_end -> Date,
        reported_percent_complete -> Numeric,
        planned_percent_complete -> Nullable<Numeric>,
        project_percent_complete -> Nullable<Numeric>,
        source -> Text,
        status -> Text,
        notes -> Nullable<Text>,
        reported_by -> Text,
        reviewed_by -> Nullable<Text>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_defects (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        inspection_id -> Nullable<Uuid>,
        task_id -> Nullable<Text>,
        title -> Text,
        description -> Nullable<Text>,
        location -> Nullable<Text>,
        severity -> Text,
        status -> Text,
        reported_by -> Text,
        assigned_to -> Nullable<Text>,
        follow_up_requested -> Bool,
        follow_up_task_id -> Nullable<Text>,
        resolved_at -> Nullable<Timestamptz>,
        verified_by -> Nullable<Text>,
        verified_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_approvals (id) {
        id -> Uuid,
        target_kind -> Text,
        target_id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        approver_id -> Text,
        decision -> Text,
        comment -> Nullable<Text>,
        decided_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_daily_logs (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        log_date -> Date,
        supervisor_id -> Text,
        weather -> Nullable<Text>,
        workforce_count -> Int8,
        work_done -> Nullable<Text>,
        materials_received -> Nullable<Text>,
        incidents_on_site -> Nullable<Text>,
        signed_by -> Nullable<Text>,
        signed_at -> Nullable<Timestamptz>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_photos (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        subject_kind -> Text,
        subject_id -> Uuid,
        storage_target -> Text,
        storage_key -> Text,
        content_sha256 -> Text,
        content_type -> Text,
        size_bytes -> Int8,
        caption -> Nullable<Text>,
        uploaded_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
```

Joinables (deliberately NO join on `project_id`/`task_id`/`milestone_id` —
opaque TEXT, no FK):

```rust
diesel::joinable!(supervision_template_items -> supervision_inspection_templates (template_id));
diesel::joinable!(supervision_inspections -> supervision_inspection_templates (template_id));
diesel::joinable!(supervision_inspection_items -> supervision_inspections (inspection_id));
diesel::joinable!(supervision_defects -> supervision_inspections (inspection_id));
```

Add all 30 tables above to the `allow_tables_to_appear_in_same_query!` list.

### AI gating (`000063_ai_gating`)

The migration is ALTERs on `ai_company_policies` / `ai_user_policies`
plus one new table. Add to `schema.rs`:

```rust
diesel::table! {
    ai_global_settings (key) {
        key -> Varchar,
        enabled -> Bool,
        updated_at -> Timestamptz,
    }
}
```

And add the `ai_enabled` (`Bool`) + `disabled_features`
(`Nullable<Jsonb>`) columns to the existing `ai_company_policies` and
`ai_user_policies` table definitions.

## (d) `src/db/models.rs` — append

### Sharing / auditors (following the existing `Queryable` + `New*` `Insertable` pattern; `status`/`reason`/`scheduler_status` stored as `String`)

```rust
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = public_links)]
pub struct PublicLinkRow {
    pub id: Uuid,
    pub token_hash: String,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub scope_view: String,
    pub created_by: String,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = public_links)]
pub struct NewPublicLinkRow {
    pub id: Uuid,
    pub token_hash: String,
    pub scope_kind: String,
    pub scope_id: Uuid,
    pub scope_view: String,
    pub created_by: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = public_link_access_log)]
pub struct PublicLinkAccessLogRow {
    pub id: Uuid,
    pub link_id: Option<Uuid>,
    pub accessed_at: DateTime<Utc>,
    pub ip_hash: Option<String>,
    pub user_agent_hash: Option<String>,
    pub allowed: bool,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = public_link_access_log)]
pub struct NewPublicLinkAccessLogRow {
    pub id: Uuid,
    pub link_id: Option<Uuid>,
    pub ip_hash: Option<String>,
    pub user_agent_hash: Option<String>,
    pub allowed: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auditor_accounts)]
pub struct AuditorAccountRow {
    pub id: Uuid,
    pub user_id: String,
    pub label: String,
    pub email: String,
    pub is_auditor: bool,
    pub auditor_of: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub password_hash: String,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<String>,
    pub revoke_reason: Option<String>,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auditor_accounts)]
pub struct NewAuditorAccountRow {
    pub id: Uuid,
    pub user_id: String,
    pub label: String,
    pub email: String,
    pub auditor_of: String,
    pub expires_at: DateTime<Utc>,
    pub password_hash: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auditor_scopes)]
pub struct AuditorScopeRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub module: String,
    pub resource_kind: String,
    pub resource_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub allow_export: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auditor_scopes)]
pub struct NewAuditorScopeRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub module: String,
    pub resource_kind: String,
    pub resource_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub allow_export: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auditor_sessions)]
pub struct AuditorSessionRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub user_id: String,
    pub started_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revoked_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auditor_sessions)]
pub struct NewAuditorSessionRow {
    pub id: Uuid,
    pub auditor_id: Uuid,
    pub user_id: String,
}
```

### Properties (following the existing `Role`/`GeoOverride` patterns; `Value` is `serde_json::Value`, `PgNumeric` is `diesel::data_types::PgNumeric`)

```rust
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = properties)]
pub struct PropertyRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub slug: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub address: Option<String>,
    pub total_area_sqm: Option<PgNumeric>,
    pub company_id: Option<Uuid>,
    pub status: String,
    pub cover_storage_target: Option<String>,
    pub cover_storage_key: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = property_plans)]
pub struct PropertyPlanRow {
    pub id: Uuid,
    pub property_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub geometry: Option<Value>,
    pub geometry_hash: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = property_blocks)]
pub struct PropertyBlockRow {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub property_id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub geometry: Value,
    pub geometry_hash: String,
    pub area_sqm: Option<PgNumeric>,
    pub length_m: Option<PgNumeric>,
    pub price: PgNumeric,
    pub currency: String,
    pub status: String,
    pub assigned_customer_id: Option<String>,
    pub click_behavior: String,
    pub click_link_kind: Option<String>,
    pub click_link_target: Option<String>,
    pub order_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = property_block_history)]
pub struct PropertyBlockHistoryRow {
    pub id: Uuid,
    pub block_id: Uuid,
    pub property_id: Uuid,
    pub plan_id: Uuid,
    pub from_status: String,
    pub to_status: String,
    pub actor_id: String,
    pub reason: Option<String>,
    pub order_id: Option<String>,
    pub created_at: DateTime<Utc>,
}
```

(`Insertable` counterparts follow the same fields minus
`id`/`created_at`/`updated_at` defaults; the module's in-memory store
is authoritative today, so these rows are persistence targets only.)

### Logistics (money uses `PgNumeric`; models carrying money skip serde derives, per the file's own note; `tracking_token_hash` carries `#[serde(skip_serializing)]` on the read struct)

```rust
#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = logistics_drivers)]
pub struct LogisticsDriver {
    pub id: Uuid,
    pub user_id: String,
    pub license_number: String,
    pub phone: Option<String>,
    pub status: String,
    pub current_vehicle_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = logistics_vehicles)]
pub struct LogisticsVehicle {
    pub id: Uuid,
    pub plate: String,
    pub vehicle_type: String,
    pub capacity_kg: f64,
    pub status: String,
    pub assigned_driver_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub maintenance: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = logistics_warehouses)]
pub struct LogisticsWarehouse {
    pub id: Uuid,
    pub name: String,
    pub address: String,
    pub capacity: i64,
    pub manager_user_id: Option<String>,
    pub default_low_stock_threshold: i64,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = logistics_shipments)]
pub struct LogisticsShipment {
    pub id: Uuid,
    pub tracking_number: String,
    #[serde(skip_serializing)]
    pub tracking_token_hash: String,
    pub order_id: Option<String>,
    pub order_link_status: String,
    pub company_id: Option<Uuid>,
    pub status: String,
    pub origin_address: String,
    pub origin_latitude: Option<f64>,
    pub origin_longitude: Option<f64>,
    pub destination_address: String,
    pub destination_latitude: Option<f64>,
    pub destination_longitude: Option<f64>,
    pub weight_kg: f64,
    pub length_cm: f64,
    pub width_cm: f64,
    pub height_cm: f64,
    pub zone_origin: String,
    pub zone_destination: String,
    pub service_level: String,
    pub driver_id: Option<Uuid>,
    pub vehicle_id: Option<Uuid>,
    pub warehouse_id: Option<Uuid>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub window_start: Option<DateTime<Utc>>,
    pub window_end: Option<DateTime<Utc>>,
    pub estimated_delivery_at: Option<DateTime<Utc>>,
    pub shipping_cost: Option<PgNumeric>,
    pub currency: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = logistics_shipment_events)]
pub struct LogisticsShipmentEvent {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub kind: String,
    pub from_status: Option<String>,
    pub to_status: Option<String>,
    pub actor: String,
    pub location: Option<String>,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = logistics_stock_movements)]
pub struct LogisticsStockMovement {
    pub id: Uuid,
    pub warehouse_id: Uuid,
    pub asset_id: Uuid,
    pub direction: String,
    pub quantity: i64,
    pub quantity_after: i64,
    pub reason: String,
    pub shipment_id: Option<Uuid>,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = logistics_routes)]
pub struct LogisticsRoute {
    pub id: Uuid,
    pub name: String,
    pub driver_id: Option<Uuid>,
    pub vehicle_id: Option<Uuid>,
    pub route_date: DateTime<Utc>,
    pub start_latitude: Option<f64>,
    pub start_longitude: Option<f64>,
    pub status: String,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = logistics_route_stops)]
pub struct LogisticsRouteStop {
    pub id: Uuid,
    pub route_id: Uuid,
    pub shipment_id: Option<Uuid>,
    pub sequence: i64,
    pub address: String,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub status: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = logistics_proofs)]
pub struct LogisticsProof {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub signature_storage_target: Option<String>,
    pub signature_storage_key: Option<String>,
    pub recipient_name: String,
    pub delivered_at: DateTime<Utc>,
    pub actor: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = logistics_exceptions)]
pub struct LogisticsException {
    pub id: Uuid,
    pub shipment_id: Uuid,
    pub exception_type: String,
    pub description: String,
    pub status: String,
    pub previous_status: String,
    pub resolution: Option<String>,
    pub actor: String,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = logistics_rate_tables)]
pub struct LogisticsRateTable {
    pub id: Uuid,
    pub name: String,
    pub origin_zone: Option<String>,
    pub destination_zone: Option<String>,
    pub base_price: PgNumeric,
    pub per_kg: PgNumeric,
    pub per_km: PgNumeric,
    pub currency: String,
    pub active: bool,
    pub company_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

`New*` insertables follow the file's existing convention (every column
except `created_at`/`updated_at` and except columns the database fills
with a DEFAULT; primary-key columns carried, callers supply ids), one
per table named `NewLogisticsShipment`, `NewLogisticsShipmentEvent`,
`NewLogisticsVehicle`, `NewLogisticsDriver`, `NewLogisticsWarehouse`,
`NewLogisticsStockMovement`, `NewLogisticsRoute`, `NewLogisticsRouteStop`,
`NewLogisticsProof`, `NewLogisticsException`, `NewLogisticsRateTable`.

### Supervision (one `Queryable`/`Insertable` pair per table, following the existing file conventions; `New*` carries every column except `created_at`/`updated_at` and DB-defaulted columns; enums are plain `String` in models)

```rust
// SupervisionAssignment / NewSupervisionAssignment
//   id: Uuid, project_id: String, company_id: Option<Uuid>, user_id: String,
//   role: String, status: String, starts_at: DateTime<Utc>, ends_at: Option<DateTime<Utc>>,
//   notes: Option<String>, created_by: String, created_at/updated_at: DateTime<Utc>
//   (New*: same minus created_at/updated_at)
// SupervisionInspectionTemplate / NewSupervisionInspectionTemplate
//   id, project_id: Option<String>, company_id: Option<Uuid>, title: String,
//   description: Option<String>, category: String, created_by: String, created_at/updated_at
// SupervisionTemplateItem / NewSupervisionTemplateItem
//   id, template_id: Uuid, title: String, description: Option<String>, position: i32,
//   required: bool, created_at/updated_at
// SupervisionInspection / NewSupervisionInspection
//   id, project_id: String, company_id: Option<Uuid>, template_id: Option<Uuid>,
//   title: String, location: Option<String>, inspector_id: String, status: String,
//   scheduled_for: DateTime<Utc>, conducted_at: Option<DateTime<Utc>>,
//   verdict: Option<String>, summary: Option<String>, signed_by: Option<String>,
//   signed_at: Option<DateTime<Utc>>, created_by: String, created_at/updated_at
// SupervisionInspectionItem / NewSupervisionInspectionItem
//   id, inspection_id: Uuid, template_item_id: Option<Uuid>, title: String, result: String,
//   notes: Option<String>, follow_up_requested: bool, follow_up_task_id: Option<String>,
//   created_at/updated_at
// SupervisionProgressReport / NewSupervisionProgressReport
//   id, project_id: String, company_id: Option<Uuid>, milestone_id: Option<String>,
//   title: String, period_start: NaiveDate, period_end: NaiveDate,
//   reported_percent_complete: PgNumeric, planned_percent_complete: Option<PgNumeric>,
//   project_percent_complete: Option<PgNumeric>, source: String, status: String,
//   notes: Option<String>, reported_by: String, reviewed_by: Option<String>,
//   reviewed_at: Option<DateTime<Utc>>, created_at/updated_at
//   (PgNumeric has no serde impl — these two structs skip the serde derives, like the store models)
// SupervisionDefect / NewSupervisionDefect
//   id, project_id: String, company_id: Option<Uuid>, inspection_id: Option<Uuid>,
//   task_id: Option<String>, title: String, description: Option<String>,
//   location: Option<String>, severity: String, status: String, reported_by: String,
//   assigned_to: Option<String>, follow_up_requested: bool,
//   follow_up_task_id: Option<String>, resolved_at: Option<DateTime<Utc>>,
//   verified_by: Option<String>, verified_at: Option<DateTime<Utc>>, created_at/updated_at
// SupervisionApproval / NewSupervisionApproval
//   id, target_kind: String, target_id: Uuid, project_id: String,
//   company_id: Option<Uuid>, approver_id: String, decision: String,
//   comment: Option<String>, decided_at: DateTime<Utc>, created_at: DateTime<Utc>
//   (New*: same minus created_at; decided_at carried, caller supplies it)
// SupervisionDailyLog / NewSupervisionDailyLog
//   id, project_id: String, company_id: Option<Uuid>, log_date: NaiveDate,
//   supervisor_id: String, weather: Option<String>, workforce_count: i64,
//   work_done: Option<String>, materials_received: Option<String>,
//   incidents_on_site: Option<String>, signed_by: Option<String>,
//   signed_at: Option<DateTime<Utc>>, created_by: String, created_at/updated_at
// SupervisionPhoto / NewSupervisionPhoto
//   id, project_id: String, company_id: Option<Uuid>, subject_kind: String,
//   subject_id: Uuid, storage_target: String, storage_key: String,
//   content_sha256: String, content_type: String, size_bytes: i64,
//   caption: Option<String>, uploaded_by: String, created_at/updated_at
```

### AI gating

Add an `AiGlobalSettingsRow` (`key: String`, `enabled: bool`,
`updated_at: DateTime<Utc>`) for `ai_global_settings`, and add the
`ai_enabled: bool` + `disabled_features: Option<serde_json::Value>`
fields to the existing `AiCompanyPolicy` / `AiUserPolicy` model structs
(matching migration `000063_ai_gating`).

## (e) Scheduler — `src/modules/scheduler.rs`

Add to `KNOWN_KINDS` (next to `"ticket.sla.check"`):

```rust
    "auditors.expire",
    "logistics.maintenance.remind",
```

Dispatch arms (next to the other arms):

```rust
        "auditors.expire" => {
            // The Auditors module listens to
            // `auditors.expire_requested` and expires the named
            // account when due (or sweeps all due accounts when no
            // `auditor_id` is given). The payload is forwarded
            // unchanged.
            state.emit("auditors.expire_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "auditors.expire_requested" }))
        }
```

```rust
        "logistics.maintenance.remind" => {
            // The Logistics module listens to
            // `logistics.maintenance.remind_requested` and marks the
            // reminder due / notifies the assigned driver.
            state.emit("logistics.maintenance.remind_requested", payload.clone());
            Ok(json!({ "dispatched": true, "event": "logistics.maintenance.remind_requested" }))
        }
```

Add to the `on_scheduler_enqueue` event→kind mapping (next to
`"ticket.sla.check_requested"`):

```rust
                    "auditors.expire_requested" => "auditors.expire".to_string(),
                    "logistics.maintenance.remind_requested" => "logistics.maintenance.remind".to_string(),
```

## (f) Config section — `src/core/config.rs`

Add the import and field (next to the `security` field):

```rust
use crate::core::sharing::SharingSettings;
```

```rust
    /// Public link sharing and auditor account settings. See
    /// `crate::core::sharing::SharingSettings`.
    pub sharing: SharingSettings,
```

In `impl Default for AppConfig`, add `sharing: SharingSettings::default(),`
and in `AppConfig::validate`, add the sharing validation (its messages
already carry the `sharing.` prefix). YAML shape:

```yaml
sharing:
  public_links_enabled: true
  auditors_enabled: true
  default_link_ttl_hours: 72
  max_link_ttl_hours: 720
  default_auditor_ttl_hours: 168
  max_auditor_ttl_hours: 720
```

Note: the auditors module currently runs on
`SharingSettings::default()`; once this field exists,
`AuditorsModule::init` should load it from `ctx.config` into
`state.settings` (one line the integrator owns, since `AppConfig` has
no such field today).

## (g) `AUDITED_EVENTS` — `src/modules/audit.rs`, add

```rust
"auditors.created", "auditors.revoked", "auditors.expired",
"auditors.action", "auditors.session.started", "auditors.session.revoked",
"sharing.link.created", "sharing.link.revoked",
"sharing.link.accessed", "sharing.link.access_denied",
"ai.gating.changed",
"supervision.assignment.created", "supervision.inspection.created",
"supervision.inspection.completed", "supervision.progress_report.created",
"supervision.defect.created", "supervision.approval.recorded",
"supervision.daily_log.created",
"logistics.shipment.created", "logistics.shipment.delivered",
"logistics.exception.created", "logistics.proof.created",
"properties.block.reserved", "properties.block.sold",
"properties.block.purchase_requested",
```

(Keep the file's existing grouping; the exact list per module is in
each builder's report — the names above are the minimum the audit
module must know.)

## (h) Role capabilities to seed (roles module)

- Auditors/sharing: `auditors.view`, `auditors.create`,
  `auditors.manage`, `auditors.revoke`, `sharing.view`,
  `sharing.create`, `sharing.revoke` (admin role holds all; auditors
  hold no roles — never grant these to auditor accounts).
- Supervision: `supervision.view`, `supervision.create`,
  `supervision.manage`, `supervision.approve`.
- Logistics: `logistics.view`, `logistics.create`, `logistics.manage`,
  `logistics.driver`, `logistics.quote`.
- Properties: `properties.view`, `properties.create`, `properties.manage`.
- AI gating endpoints reuse the existing `ai:manage_providers`
  capability (deliberate — a new `ai:manage_gating` permission would
  deny everyone until roles grant it).

## (i) API Manager — `ApiAccessRule` rows (`public_access: true`)

- `GET /api/v1/sharing/public/{token}` (without it the public route is
  denied by default-deny, exactly like the events public routes).
- `GET /api/v1/logistics/track/*`.
- `GET /api/v1/properties/public/*` (all four public routes).

## (j) Cross-module contracts the integrator must wire

### Projects module — four query handlers for supervision (none exists today; verified in `projects.rs`)

Pattern to follow: the existing `on_billable_hours_requested` in
`projects.rs` (reads `ctx.data`, answers via
`ctx.add_result(json!({...}))`).

1. `project.get_requested` `{"project_id"}` → result
   `{"exists": true, "project": {"id", "name", "status", "archived"}}`
   or `{"exists": false}`.
2. `project.progress_requested` `{"project_id", "milestone_id" | null}`
   → result `{"percent_complete": <0..100>, "milestone_id": …,
   "milestone_reached": <bool>}` — derived by the projects module from
   its own tasks/milestones (it has no stored percent on `Project`;
   e.g. completed vs total tasks).
3. `project.task.create_requested` `{"project_id", "title",
   "description", "source": "supervision", "inspection_id" | null,
   "defect_id" | null, "requested_by"}` → result
   `{"created": true, "task_id": "<id>"}` or
   `{"created": false, "error": "…"}`.
4. `project.task.get_requested` `{"task_id"}` → result
   `{"exists": true, "task": {…}}` or `{"exists": false}`.

Until these exist, supervision fails closed on project lookups (by
design) and follow-ups record as unanswered.

### Store module — properties purchase listener (`store.rs`)

In `store.rs register_hooks`, add a listener for
`properties.block.purchase_requested`. It must create a store order
for the block (single line item: the block, priced at the payload's
`price` / `currency`, buyer = payload `buyer_id`, company = the
property's company when known) and then emit:

```json
properties.order.created {"purchase_id", "order_id", "block_id", "property_id", "buyer_id", "total", "currency"}
```

so the properties module can link the order id to the purchase record.
The store's existing `payment.captured` /
`bank_transfer.verification.created` / `bank_transfer.rejected`
emitters need NO change: the properties listeners already match on
the order id they carry. Do NOT mark the block sold in the store —
the properties `payment.captured` listener owns that transition.

### Store module — `store.order.lookup` hook query (for logistics)

`store.order.lookup` `{"order_id"}` → `{"found": bool}`. Until the
store answers, logistics order links record
`order_link_status: "unverified"` (by design, never treated as linked).

### Calculator / geo — `calculator.distance_requested` (for logistics)

`calculator.distance_requested` `{origin_latitude, origin_longitude,
destination_latitude, destination_longitude}` →
`{"distance_km", "method"}`. Until answered, logistics quotes without
a caller-supplied `distance_km` fail with 422 (no distance is ever
guessed).

### Products / billing — `ai.product_includes_ai` hook query (for AI gating)

- Event: `ai.product_includes_ai` (`EVENT_AI_PRODUCT_INCLUDES_AI`,
  `ai.rs` line 168). Request payload:
  `{ "product_id": "<uuid>", "user_id": "<uuid>" | null,
  "company_id": "<uuid>" | null }`.
- Answering handler sets the answer on the hook context:
  `ctx.set("includes_ai", json!(true))` (same pattern as
  `user.company_requested` / `role.check`).
- Response: `includes_ai: true` = product includes AI (allow);
  `false` = product excludes AI (deny, `ai_product_excludes_ai`); **no
  answer = fail closed** (deny, `ai_product_unknown`).
- No product context on the call (no `X-Product-Id` header) = product
  layer defaults to **allow**, recorded in the decision as `"No
  product context: product layer defaults to allow"`. This is the only
  fail-open case, and only when the caller supplies no product at all.

### `sharing.allow_check` hook contract (for owning modules — exact)

- Event name: `sharing.allow_check`
  (`crate::core::sharing::sharing_events::ALLOW_CHECK`). Payload in:
  `{"kind": "<resource kind>", "id": "<resource UUID string>",
  "view": "<view>", "actor": "<X-User-Id of the link creator>",
  "allowed": false}`.
- The owning module registers one handler for this event and answers
  **only for kinds it owns**; other kinds must be ignored (leave
  `"allowed"` untouched).
- Handler steps: (1) load the record by `id`; if it does not exist,
  leave `"allowed": false`. (2) Set `"allowed": true` **only if** the
  record carries `allow_public_link: true` **AND** the `actor` may
  share that record (record owner, or a caller holding a manage
  capability on the owning module). (3) Never set `"allowed": true`
  unconditionally; never answer for another module's kind.
- No handler answering = denied (fail closed). The auditors module
  enforces the answer server-side at link creation (`POST
  /api/v1/sharing/links` returns 403 when false); hiding the share
  button in the UI is not a substitute.
- Registration prerequisite: the owning module must first register its
  kind and views in `ShareRegistry` (e.g. in its `init`:
  `registry.register("project", &["board", "summary"], "Project
  board/summary views")` — the registry lives in `AuditorsState`;
  until a registry handle is exposed, register via the hook bus or ask
  the integrator to wire `ShareRegistry` into `AppContext`). Until
  both steps are done, link creation for that kind fails closed by
  design.

### Auditor scope enforcement (for adopting modules — exact place)

Scope evaluation in `core::sharing::AuditorScope` is only the
decision; enforcement must happen in the query, never after fetch:

- **List endpoints**: before running the query, call
  `auditors.scope.check` (or `scope.allows_kind`); then add a Diesel
  predicate from `covered_record_ids(module, kind)` →
  `.filter(record_id.eq_any(ids))`, or when it returns `None`
  (wholesale/company entry), from `covered_company_ids(module, kind)`
  → `.filter(company_id.eq_any(companies))`. If both are empty →
  return an empty page (never a 403 that leaks existence, and never
  an unfiltered list).
- **Detail endpoints**: call `auditors.scope.check` with the record's
  `(module, kind, resource_id, company_id)` **before** loading the
  record (or load by id inside the scope-filtered query); on deny
  return 403 with the generic message.
- **Exports**: same as detail but with `action: "export"` — denied
  unless a scope entry for that exact record/company grants
  `allow_export: true`.
- **Mutations/settings/API keys**: never reachable by auditors —
  `assert_auditor_action_allowed` rejects them before any scope
  lookup; modules that add their own write endpoints must also reject
  callers whose `X-User-Id` matches an auditor account (via
  `auditors.scope.check` with a non-read action, which always denies).

### AI gating — combined evaluation order (exact, `evaluate_ai_gate`, `ai.rs` line 3362)

1. settings kill-switch `ai.global_enabled` → deny `ai_globally_disabled`
2. global per-feature switch → deny `ai_feature_disabled`
3. product layer via `ai.product_includes_ai` hook → deny
   `ai_product_excludes_ai` / `ai_product_unknown`
4. company allowlist (`enabled`) + gate (`ai_enabled`) → deny
   `ai_not_allowed_for_company` / `ai_company_unknown` /
   `ai_disabled_for_company`
5. user allowlist (`enabled`) + gate (`ai_enabled`) → deny
   `ai_not_allowed_for_user` / `ai_user_unknown` / `ai_disabled_for_user`
6. feature toggles: allowlist `allowed_features` lists, then
   company/user `disabled_features` off-switches → deny
   `ai_feature_not_allowed` / `ai_feature_disabled_for_company` /
   `ai_feature_disabled_for_user`

Then `authorize_ai_call` still runs its existing steps 1–7 (models,
caps, personal-key quota) — the gate covers on/off only. In legacy
open mode (no policy anywhere) company/user layers allow (nothing was
switched off) while settings + product layers still apply. Effective
access = SETTINGS **AND** PRODUCT **AND** COMPANY **AND** USER **AND**
FEATURE. The gate is called FIRST inside `authorize_ai_call` (line
2751, insertion at line 2805) and re-checked on every call — no
decision is cached anywhere. Feature names are canonical use-case
names (`complete`, `support_reply`, `chat_reply`, `content`, `email`,
`kb_answer`, `test`) with friendly aliases: `support`/`drafting` →
`support_reply`, `chat` → `chat_reply`, `insights` → `complete`
(insight generation runs through generic completion — see OPEN list).

## (k) README / TODO lines

README (modules table):

```markdown
- **Public Sharing & Auditor Access** (`auditors`, `/api/v1/auditors` + `/api/v1/sharing`, 19 routes) — signed, expiring, revocable, single-scope public links (server-side allow-flag enforcement) and time-limited read-only auditor accounts with scoped data access, full audit logging, and one-click revoke. Core security infrastructure, not sold separately.
- **Property Manager** (`properties`, `/api/v1/properties`, 37 routes) — land parcels and housing developments with 2D site plans, blocks/plots for sale, pluggable geocoder location search, SVG plans and print pages. Sellable as a product (category `commerce`). Buy flow hands checkout to the store module and marks blocks sold on `payment.captured`. True 3D visualization is NOT built.
- **Logistics** (`logistics`, `/api/v1/logistics`, 55 routes) — shipments with a checked state machine and timeline, fleet vehicles and drivers, warehouses with stock movements and low-stock alerts, delivery routes with a nearest-neighbour reorder heuristic, delivery scheduling, proof of delivery, delivery exceptions, rate-table shipping quotes, and token-gated public tracking. Sellable as a product (category `commerce`).
- **Project Supervision** (`supervision`, `/api/v1/supervision`, 44 routes) — supervisor assignments, inspection templates and checklists, conducted inspections with verdicts and signatures, progress-report snapshots with an approval workflow, defect log with photo evidence, approval chains with a full audit trail, and daily site logs. Sellable as a product (category `productivity`). Projects, tasks, and milestones stay owned by Project Management and are linked by id through hook queries only (`project.get_requested`, `project.progress_requested`, `project.task.create_requested`, `project.task.get_requested` — the projects module must answer these; until then supervision fails closed on project lookups).
- **Layered AI gating** (AI module section) — a global kill-switch (`ai.global_enabled`), a purchased-product layer (`ai.product_includes_ai` hook), and company/user on-off switches with per-feature toggles, composed in `evaluate_ai_gate` and enforced first on every AI call; admin endpoints under `/api/v1/ai/gating`.
```

TODO (append):

```markdown
- Batch 5 integration: register auditors/properties/logistics/supervision in mod.rs, add the sharing/auditor/property/logistics/supervision/AI-gating tables to schema.rs + models.rs (snippets in INTEGRATION_NOTES_BATCH5.md), add scheduler kinds `auditors.expire` + `logistics.maintenance.remind`, wire the `sharing:` config section, add the AUDITED_EVENTS names, seed the new role capabilities, create the three ApiAccessRule rows. Migrations 000060–000063 + 000067 have never run against live PostgreSQL; cargo check has never run on any batch-5 file.
- Supervision: the projects module must still register the four query handlers (`project.get_requested`, `project.progress_requested`, `project.task.create_requested`, `project.task.get_requested`).
- Properties: implement the store-side `properties.block.purchase_requested` listener (create order + emit `properties.order.created`) — without it, purchases reserve a block but no order is ever created. Scheduler sweep to release stale reservations (purchase in `requested` with no order after a configurable window). Persistence table for `BlockPurchase` (purchase -> order linkage is in-memory only today; a restart loses it). True 3D site-plan visualization (future item — 2D SVG only today).
- Logistics: store module should answer the `store.order.lookup` hook query; Calculator (or geo) module should answer `calculator.distance_requested`; add `logistics.maintenance.remind` to `KNOWN_KINDS` and arm the dispatch.
- AI gating follow-ups: (1) add `ai_global_settings` + the `ai_enabled`/`disabled_features` columns to `src/db/schema.rs` and `src/db/models.rs` (migration 000063 is the persistence target; in-memory stores are authoritative); (2) products/billing module must answer `ai.product_includes_ai` (no answer currently fails closed when `X-Product-Id` is present); (3) add `ai.gating.changed` to `AUDITED_EVENTS` in `audit.rs`; (4) split a dedicated insights use case out of `complete` if insights needs its own toggle.
- Sharing/auditors: no owning module has adopted sharing yet — no kind is registered in `ShareRegistry` and no module answers `sharing.allow_check`, so link creation fails closed for every resource until modules adopt the hook contract. `auth.login` payload shape is unverified (the session-recording listener reads defensively).
```

## OPEN list (consolidated, explicit)

1. **`cargo check` never run** (no toolchain, crates.io blocked) —
   compilation is NOT verified for any batch-5 file. Known
   compile-risk spots: `decide_inspection`/`decide_progress_report`
   in supervision are `async fn` with no `.await` inside (warning
   only); `ReportStatus` formatted with `{:?}` in one error string;
   B1 fixed two type-level risks by hand (an `access_attempt`
   signature mismatch, a missing `MIN_AUDITOR_TTL_HOURS` import).
2. **Migrations never executed** against any PostgreSQL database.
   In-memory stores are authoritative until then.
3. **No live test** of any endpoint, gate decision, or migration (no
   running server/DB in this VM).
4. **Supervision is not usable end-to-end until the projects module
   answers its four query hooks** (fails closed by design).
5. **Properties buy flow dead-ends without the store-side listener**
   (see (j)); stale reservations have no sweep yet; `BlockPurchase`
   has no DB table (in-memory only).
6. **Logistics**: `store.order.lookup` and
   `calculator.distance_requested` have no answerers;
   `logistics.maintenance.remind` not in scheduler `KNOWN_KINDS`.
7. **AI gating**: no `ai.product_includes_ai` answerer yet (calls
   carrying `X-Product-Id` are denied with `ai_product_unknown`);
   `ai.gating.changed` not in `AUDITED_EVENTS`; `insights` is an
   alias for `complete`, so switching `insights` off switches off all
   generic completions until a dedicated insights use case exists;
   gating endpoints reuse `ai:manage_providers` rather than a new
   `ai:manage_gating` permission; `PATCH /gating/company|user` 404s
   when no policy exists instead of creating one.
8. **Sharing/auditors**: no module has adopted sharing yet (link
   creation fails closed for every resource); `auth.login` payload
   shape unverified; `auditors.expire` scheduler kind not wired yet;
   `ApiAccessRule` row for the public route not created; 7 new role
   capabilities not seeded; `auditors.*`/`sharing.*` events not in
   `AUDITED_EVENTS`; `sharing:` config section not wired into
   `AppConfig` (module runs on defaults until then).
9. **3D site-plan visualization is NOT built** (2D SVG only — recorded
   as a future item, never pretended).
10. **Route optimisation in logistics is a nearest-neighbour
    heuristic, not an optimiser** (documented as such; refuses with
    422 when stops lack coordinates).
11. **Logistics minor redundancy (not a bug)**: `resolve_exception`
    with `resume_delivery:false` sets the exception status to
    `"returned"` twice (harmless).
12. **Properties geocoder**: live Nominatim call never tested; search
    returns at most 5 results; reverse geocode returns a single
    candidate or none — by design. Area/length are site-plan
    approximations (equirectangular + shoelace), not survey-grade.
