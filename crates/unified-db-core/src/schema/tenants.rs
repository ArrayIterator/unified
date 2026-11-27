// Diesel schema for the tenants domain of the unified-db-core bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Tenants (migration 000100_tenant_isolation); snippets from the
// header doc comment of `src/modules/tenants.rs`, verified against
// the migration. `tenant_module_grants` has no created_at/updated_at.

diesel::table! {
    tenants (id) {
        id -> Uuid,
        slug -> Varchar,
        display_name -> Varchar,
        status -> Varchar,
        owner_user_id -> Uuid,
        package_id -> Nullable<Uuid>,
        preset_id -> Nullable<Uuid>,
        plan_id -> Nullable<Varchar>,
        subdomain -> Nullable<Varchar>,
        subdomain_pending -> Bool,
        schema_name -> Varchar,
        suspended_at -> Nullable<Timestamptz>,
        suspended_reason -> Nullable<Text>,
        destroyed_at -> Nullable<Timestamptz>,
        export_reference -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tenant_provisioning_steps (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        step -> Varchar,
        status -> Varchar,
        attempt -> Int4,
        error -> Nullable<Text>,
        detail -> Jsonb,
        started_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    tenant_module_grants (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        module_id -> Varchar,
        granted_via -> Varchar,
        granted_via_id -> Nullable<Uuid>,
        granted_at -> Timestamptz,
        expires_at -> Nullable<Timestamptz>,
        granted_by -> Nullable<Varchar>,
        revoked_at -> Nullable<Timestamptz>,
        revoke_reason -> Nullable<Text>,
    }
}

// Tenant destroy requests (no migration yet; derived from the
// `DestroyRequest` struct in src/modules/tenant_destroy.rs).

diesel::table! {
    tenant_destroy_requests (id) {
        id -> Uuid,
        tenant_id -> Uuid,
        tenant_slug -> Varchar,
        status -> Varchar,
        requested_by -> Varchar,
        requested_at -> Timestamptz,
        reason -> Nullable<Text>,
        export_job_id -> Nullable<Uuid>,
        export_requested_at -> Nullable<Timestamptz>,
        export_completed_at -> Nullable<Timestamptz>,
        export_download_url -> Nullable<Text>,
        export_verified_via -> Nullable<Varchar>,
        export_request_claimed -> Bool,
        confirmation_attempts -> Int4,
        failed_confirmation_attempts -> Int4,
        confirmation_locked_until -> Nullable<Timestamptz>,
        confirmed_at -> Nullable<Timestamptz>,
        confirmed_by -> Nullable<Varchar>,
        second_approval_required -> Bool,
        second_approval_reason -> Nullable<Text>,
        second_approver -> Nullable<Varchar>,
        second_approved_at -> Nullable<Timestamptz>,
        grace_period_days -> Int8,
        grace_expires_at -> Nullable<Timestamptz>,
        suspended_at -> Nullable<Timestamptz>,
        destroyed_at -> Nullable<Timestamptz>,
        schema_name_snapshot -> Nullable<Varchar>,
        subdomains_revoke_requested -> Bool,
        external_cleanup -> Jsonb,
        audit_trail -> Jsonb,
        cancel_reason -> Nullable<Text>,
        cancelled_by -> Nullable<Varchar>,
        cancelled_at -> Nullable<Timestamptz>,
        last_error -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
