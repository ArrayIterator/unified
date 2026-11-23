// Diesel schema for the approvals domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Approvals (migration 000089_approvals). `module` is a Rust keyword,
// so the struct field is `module_` (Diesel strips the underscore).
// ---------------------------------------------------------------------------

diesel::table! {
    approval_definitions (id) {
        id -> Uuid,
        #[sql_name = "module"]
        module_ -> Varchar,
        record_type -> Varchar,
        name -> Varchar,
        description -> Nullable<Text>,
        is_active -> Bool,
        timeout_hours -> Nullable<Int4>,
        escalation_action -> Varchar,
        escalate_to -> Nullable<Jsonb>,
        escalation_extension_hours -> Nullable<Int4>,
        company_id -> Nullable<Varchar>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    approval_steps (id) {
        id -> Uuid,
        definition_id -> Uuid,
        position -> Int4,
        name -> Varchar,
        mode -> Varchar,
        approvers -> Jsonb,
        is_active -> Bool,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    approval_requests (id) {
        id -> Uuid,
        definition_id -> Uuid,
        #[sql_name = "module"]
        module_ -> Varchar,
        record_type -> Varchar,
        record_id -> Varchar,
        requester_id -> Varchar,
        summary -> Varchar,
        amount_minor -> Nullable<Int8>,
        currency -> Nullable<Varchar>,
        status -> Varchar,
        current_step_index -> Int4,
        round -> Int4,
        escalation_count -> Int4,
        last_escalated_at -> Nullable<Timestamptz>,
        expires_at -> Timestamptz,
        decided_at -> Nullable<Timestamptz>,
        decided_by -> Nullable<Varchar>,
        company_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    approval_decisions (id) {
        id -> Uuid,
        request_id -> Uuid,
        step_index -> Int4,
        round -> Int4,
        approver_id -> Varchar,
        acting_for -> Nullable<Varchar>,
        slot_kind -> Varchar,
        slot_ref -> Varchar,
        decision -> Varchar,
        comment -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    approval_delegations (id) {
        id -> Uuid,
        delegator_id -> Varchar,
        delegate_id -> Varchar,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        is_active -> Bool,
        created_by -> Varchar,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    approval_escalations (id) {
        id -> Uuid,
        request_id -> Uuid,
        step_index -> Int4,
        reason -> Varchar,
        action -> Varchar,
        created_at -> Timestamptz,
    }
}
