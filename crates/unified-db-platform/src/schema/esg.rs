// Diesel schema for the ESG domain of the unified-db-platform bounded context.
//
// Tables are defined by migration 2026-11-22-000119_esg. They had
// no Diesel bindings before C11 (task 271): the ESG module kept
// its live data in memory and the tables were persistence targets
// only. The columns below are byte-identical to that migration.
// No foreign keys: ESG rows reference other modules by plain
// ids only (see the migration header), so no `joinable!`
// declarations belong here; the hydrate path in
// `src/modules/esg/state.rs` loads each table on its own.

// ---------------------------------------------------------------------------
// ESG (migration 000119_esg).
// ---------------------------------------------------------------------------

diesel::table! {
    esg_frameworks (id) {
        id -> Uuid,
        tenant_id -> Text,
        name -> Text,
        description -> Nullable<Text>,
        version -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    esg_metrics (id) {
        id -> Uuid,
        tenant_id -> Text,
        name -> Text,
        description -> Nullable<Text>,
        category -> Text,
        unit -> Text,
        data_type -> Text,
        frequency -> Text,
        framework_ids -> Jsonb,
        active -> Bool,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    esg_periods (id) {
        id -> Uuid,
        tenant_id -> Text,
        code -> Text,
        name -> Nullable<Text>,
        starts_on -> Date,
        ends_on -> Date,
        status -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    esg_entries (id) {
        id -> Uuid,
        tenant_id -> Text,
        metric_id -> Uuid,
        period_id -> Uuid,
        scope_type -> Text,
        scope_id -> Text,
        value_text -> Text,
        source_note -> Nullable<Text>,
        document_ids -> Jsonb,
        entered_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    esg_entry_history (id) {
        id -> Uuid,
        tenant_id -> Text,
        entry_id -> Uuid,
        metric_id -> Uuid,
        period_id -> Uuid,
        value_text -> Text,
        source_note -> Nullable<Text>,
        entered_by -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    esg_targets (id) {
        id -> Uuid,
        tenant_id -> Text,
        metric_id -> Uuid,
        period_id -> Uuid,
        scope_type -> Text,
        scope_id -> Text,
        target_value -> Text,
        direction -> Text,
        deadline -> Date,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    esg_audit (id) {
        id -> Uuid,
        tenant_id -> Text,
        entity_type -> Text,
        entity_id -> Text,
        action -> Text,
        actor -> Text,
        before -> Nullable<Jsonb>,
        after -> Nullable<Jsonb>,
        created_at -> Timestamptz,
    }
}
