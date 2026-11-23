// Diesel schema for the incidents domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Incidents & on-call (migration 000053_incidents). The
// self-reference `on_call_schedules.overrides_entry_id` gets no
// `joinable!` (a table cannot join to itself in that form).
// ---------------------------------------------------------------------------

diesel::table! {
    incidents (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Nullable<Text>,
        severity -> Varchar,
        status -> Varchar,
        affected_service_ids -> Array<Text>,
        responders -> Array<Text>,
        current_responder -> Nullable<Varchar>,
        source -> Varchar,
        source_service_id -> Nullable<Varchar>,
        escalation_policy_id -> Nullable<Uuid>,
        rotation_id -> Nullable<Uuid>,
        created_by -> Varchar,
        acknowledged_by -> Nullable<Varchar>,
        acknowledged_at -> Nullable<Timestamptz>,
        resolved_at -> Nullable<Timestamptz>,
        closed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    incident_timeline (id) {
        id -> Uuid,
        incident_id -> Uuid,
        actor_id -> Varchar,
        kind -> Varchar,
        message -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    on_call_rotations (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        members -> Array<Text>,
        handover_interval_days -> Int8,
        handover_time -> Varchar,
        timezone -> Varchar,
        current_index -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    on_call_schedules (id) {
        id -> Uuid,
        rotation_id -> Uuid,
        member_id -> Varchar,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        kind -> Varchar,
        overrides_entry_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    escalation_policies (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Nullable<Text>,
        applies_to_severities -> Array<Text>,
        steps -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    incident_escalations (id) {
        id -> Uuid,
        incident_id -> Uuid,
        policy_id -> Uuid,
        step_index -> Int8,
        next_escalation_at -> Timestamptz,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    page_attempts (id) {
        id -> Uuid,
        incident_id -> Uuid,
        paged_user_id -> Varchar,
        channel -> Varchar,
        status -> Varchar,
        acknowledged_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    incident_postmortems (id) {
        id -> Uuid,
        incident_id -> Uuid,
        summary -> Text,
        timeline_summary -> Nullable<Text>,
        root_cause -> Nullable<Text>,
        created_by -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    postmortem_action_items (id) {
        id -> Uuid,
        postmortem_id -> Uuid,
        title -> Varchar,
        assignee_id -> Nullable<Varchar>,
        status -> Varchar,
        task_reference -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
