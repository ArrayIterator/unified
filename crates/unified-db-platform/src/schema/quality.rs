// Diesel schema for the quality domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Import / export hardening (migration 000083_import_export_hardening).
// The `import_jobs` / `export_jobs` ALTERs from the same migration
// are applied to the existing `table!` blocks above.
// ---------------------------------------------------------------------------

diesel::table! {
    mapping_templates (id) {
        id -> Uuid,
        name -> Varchar,
        entity -> Varchar,
        mapping -> Jsonb,
        actor -> Varchar,
        company_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

// ---------------------------------------------------------------------------
// Quality management (migration 000087_quality).
// ---------------------------------------------------------------------------

diesel::table! {
    quality_points (id) {
        id -> Uuid,
        name -> Text,
        description -> Text,
        scope -> Text,
        target_ref -> Nullable<Text>,
        target_kind -> Nullable<Text>,
        check_kind -> Text,
        on_fail_policy -> Text,
        alert_severity -> Text,
        active -> Bool,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quality_criteria (id) {
        id -> Uuid,
        point_id -> Uuid,
        name -> Text,
        kind -> Text,
        unit -> Nullable<Text>,
        min_value -> Nullable<Float8>,
        max_value -> Nullable<Float8>,
        expected_bool -> Nullable<Bool>,
        expected_text -> Nullable<Text>,
        failure_cause -> Nullable<Text>,
        weight -> Float8,
        required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quality_inspections (id) {
        id -> Uuid,
        point_id -> Uuid,
        target_ref -> Text,
        target_kind -> Text,
        inspector_id -> Nullable<Text>,
        status -> Text,
        result -> Nullable<Text>,
        measurements -> Jsonb,
        failure_causes -> Array<Text>,
        notes -> Nullable<Text>,
        evidence_refs -> Array<Text>,
        inspected_at -> Nullable<Timestamptz>,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quality_alerts (id) {
        id -> Uuid,
        inspection_id -> Uuid,
        point_id -> Uuid,
        target_ref -> Text,
        target_kind -> Text,
        severity -> Text,
        status -> Text,
        message -> Text,
        blocked -> Bool,
        acknowledged_by -> Nullable<Text>,
        resolved_by -> Nullable<Text>,
        resolved_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quality_capas (id) {
        id -> Uuid,
        inspection_id -> Uuid,
        alert_id -> Nullable<Uuid>,
        title -> Text,
        description -> Text,
        root_cause -> Nullable<Text>,
        owner_id -> Text,
        due_date -> Nullable<Timestamptz>,
        state -> Text,
        verification_notes -> Nullable<Text>,
        verified_by -> Nullable<Text>,
        verified_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
