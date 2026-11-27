// Diesel schema for the compliance domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Compliance (migration 000058_compliance).

diesel::table! {
    compliance_export_requests (id) {
        id -> Uuid,
        requester_id -> Varchar,
        scope -> Array<Text>,
        status -> Varchar,
        storage_target -> Nullable<Varchar>,
        storage_key -> Nullable<Varchar>,
        download_expires_at -> Nullable<Timestamptz>,
        failure_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_deletion_requests (id) {
        id -> Uuid,
        requester_id -> Varchar,
        reason -> Text,
        status -> Varchar,
        approved_by -> Nullable<Varchar>,
        approved_at -> Nullable<Timestamptz>,
        executed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_deletion_items (id) {
        id -> Uuid,
        deletion_request_id -> Uuid,
        module_id -> Varchar,
        subject -> Text,
        status -> Varchar,
        detail -> Nullable<Text>,
        executed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_consents (id) {
        id -> Uuid,
        user_id -> Varchar,
        purpose -> Varchar,
        consent_text_version -> Varchar,
        granted -> Bool,
        recorded_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_retention_policies (id) {
        id -> Uuid,
        data_category -> Varchar,
        retain_days -> Int8,
        review_action -> Varchar,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_retention_reviews (id) {
        id -> Uuid,
        policy_id -> Uuid,
        items_flagged -> Int8,
        notes -> Nullable<Text>,
        scheduler_status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        reviewed_at -> Timestamptz,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_processing_records (id) {
        id -> Uuid,
        purpose -> Varchar,
        data_categories -> Array<Text>,
        recipients -> Array<Text>,
        retention -> Varchar,
        lawful_basis -> Varchar,
        owner -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_checklists (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        status -> Varchar,
        evidence_storage_target -> Nullable<Varchar>,
        evidence_storage_key -> Nullable<Varchar>,
        due_date -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_breach_log (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Text,
        discovered_at -> Timestamptz,
        affected_scope -> Varchar,
        status -> Varchar,
        notification_status -> Varchar,
        reported_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    compliance_breach_timeline (id) {
        id -> Uuid,
        breach_id -> Uuid,
        author_id -> Varchar,
        entry -> Text,
        recorded_at -> Timestamptz,
    }
}
