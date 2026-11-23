// Diesel schema for the entitlements domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Entitlements (migration 000098_entitlements; tenant-schema tables);
// batch 15 B1 notes §4. No `joinable!` entries (no FKs, per notes).
// ---------------------------------------------------------------------------

diesel::table! {
    entitlements (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        company_id -> Varchar,
        source_kind -> Varchar,
        source_id -> Varchar,
        source_ref -> Nullable<Varchar>,
        module_id -> Varchar,
        status -> Varchar,
        granted_at -> Timestamptz,
        expires_at -> Nullable<Timestamptz>,
        granted_by -> Nullable<Varchar>,
        reason -> Nullable<Varchar>,
        grace_started_at -> Nullable<Timestamptz>,
        grace_ends_at -> Nullable<Timestamptz>,
        lapsed_at -> Nullable<Timestamptz>,
        lapse_reason -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_history (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        entitlement_id -> Nullable<Uuid>,
        company_id -> Varchar,
        module_id -> Varchar,
        action -> Varchar,
        actor_id -> Nullable<Varchar>,
        detail -> Jsonb,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_denied_attempts (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        company_id -> Varchar,
        module_id -> Varchar,
        actor_id -> Nullable<Varchar>,
        reason -> Varchar,
        error_code -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_package_changes (id) {
        id -> Uuid,
        tenant_id -> Varchar,
        company_id -> Varchar,
        source_kind -> Varchar,
        source_id -> Varchar,
        new_module_ids -> Jsonb,
        newly_entitled -> Jsonb,
        kept -> Jsonb,
        kept_via_other_source -> Jsonb,
        requires_confirmation -> Jsonb,
        disabled_module_ids -> Jsonb,
        status -> Varchar,
        requested_by -> Nullable<Varchar>,
        decided_by -> Nullable<Varchar>,
        decided_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_settings (id) {
        id -> Varchar,
        grace_period_days -> Int8,
        read_only_grace_enabled -> Bool,
        default_deny -> Bool,
        updated_by -> Nullable<Varchar>,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    entitlement_module_activations (company_id, module_id) {
        tenant_id -> Varchar,
        company_id -> Varchar,
        module_id -> Varchar,
        enabled -> Bool,
        enabled_at -> Timestamptz,
        enabled_by -> Nullable<Varchar>,
        source_kind -> Nullable<Varchar>,
        source_id -> Nullable<Varchar>,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    reference_id_settings (tenant_id) {
        tenant_id -> Uuid,
        default_prefix -> Varchar,
        product_prefixes -> Jsonb,
        max_total_length -> Int4,
    }
}
