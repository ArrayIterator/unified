// Diesel schema for the events domain of the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Events (migration 000034_events; batch 2 notes sec. 6).

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

// Sharing / auditors (migration 000068_sharing_auditor; batch 5 notes sec. c).

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
