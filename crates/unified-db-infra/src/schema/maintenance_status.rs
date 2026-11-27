// Diesel schema for the maintenance_status domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Maintenance, status page, announcements, and auth (2026-11-22-000019_maintenance_statuspage_announcements_auth).

diesel::table! {
    maintenance_windows (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Text,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        affected_services -> Jsonb,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        activated_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    maintenance_history (id) {
        id -> Uuid,
        window_id -> Uuid,
        event -> Varchar,
        message -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    status_components (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        group -> Nullable<Varchar>,
        status -> Varchar,
        sort_order -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    status_daily_uptime (component_id, date) {
        component_id -> Uuid,
        date -> Varchar,
        uptime_percent -> Float8,
        minutes_down -> Int8,
        total_minutes -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    status_incidents (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Text,
        status -> Varchar,
        severity -> Varchar,
        affected_components -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
        resolved_at -> Nullable<Timestamptz>,
    }
}

diesel::table! {
    status_incident_updates (id) {
        id -> Uuid,
        incident_id -> Uuid,
        status -> Varchar,
        message -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
