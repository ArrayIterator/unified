// Diesel schema for the appointments domain of the unified-db-people bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Appointments (migration 000077_appointments). `AppointmentService`
// and `Appointment` carry NUMERIC columns, so they skip ALL serde
// derives per the repo header rule.
// ---------------------------------------------------------------------------

diesel::table! {
    appointment_services (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        duration_minutes -> Int4,
        price -> Numeric,
        currency -> Bpchar,
        capacity -> Int4,
        requires_staff -> Bool,
        location_type -> Varchar,
        location -> Nullable<Varchar>,
        buffer_before_minutes -> Nullable<Int4>,
        buffer_after_minutes -> Nullable<Int4>,
        cancellation_notice_hours -> Nullable<Int4>,
        cancellation_fee_percent -> Nullable<Numeric>,
        is_active -> Bool,
        company_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    appointment_staff_availability (id) {
        id -> Uuid,
        staff_user_id -> Varchar,
        kind -> Varchar,
        weekday -> Nullable<Int4>,
        date -> Nullable<Date>,
        start_minutes -> Nullable<Int4>,
        end_minutes -> Nullable<Int4>,
        break_start_minutes -> Nullable<Int4>,
        break_end_minutes -> Nullable<Int4>,
        valid_from -> Nullable<Date>,
        valid_until -> Nullable<Date>,
        reason -> Nullable<Varchar>,
        is_active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    appointment_staff_services (id) {
        id -> Uuid,
        staff_user_id -> Varchar,
        service_id -> Uuid,
        is_active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    appointments (id) {
        id -> Uuid,
        service_id -> Uuid,
        staff_user_id -> Nullable<Varchar>,
        customer_user_id -> Nullable<Varchar>,
        customer_name -> Varchar,
        customer_email -> Varchar,
        customer_phone -> Nullable<Varchar>,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        status -> Varchar,
        source -> Varchar,
        price -> Numeric,
        currency -> Bpchar,
        cancellation_fee_charged -> Nullable<Numeric>,
        cancelled_at -> Nullable<Timestamptz>,
        cancel_reason -> Nullable<Varchar>,
        notes -> Nullable<Text>,
        confirmation_code -> Varchar,
        company_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    appointment_reminders (id) {
        id -> Uuid,
        appointment_id -> Uuid,
        offset_minutes -> Int4,
        channels -> Array<Text>,
        status -> Varchar,
        scheduler_job_id -> Nullable<Varchar>,
        scheduler_status -> Varchar,
        sent_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
