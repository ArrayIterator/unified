// Diesel schema for the supervision domain of the unified-db-people bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Supervision (migration 000063_supervision; batch 5 notes sec. c).

diesel::table! {
    supervision_assignments (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        user_id -> Text,
        role -> Text,
        status -> Text,
        starts_at -> Timestamptz,
        ends_at -> Nullable<Timestamptz>,
        notes -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_inspection_templates (id) {
        id -> Uuid,
        project_id -> Nullable<Text>,
        company_id -> Nullable<Uuid>,
        title -> Text,
        description -> Nullable<Text>,
        category -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_template_items (id) {
        id -> Uuid,
        template_id -> Uuid,
        title -> Text,
        description -> Nullable<Text>,
        position -> Int4,
        required -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_inspections (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        template_id -> Nullable<Uuid>,
        title -> Text,
        location -> Nullable<Text>,
        inspector_id -> Text,
        status -> Text,
        scheduled_for -> Timestamptz,
        conducted_at -> Nullable<Timestamptz>,
        verdict -> Nullable<Text>,
        summary -> Nullable<Text>,
        signed_by -> Nullable<Text>,
        signed_at -> Nullable<Timestamptz>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_inspection_items (id) {
        id -> Uuid,
        inspection_id -> Uuid,
        template_item_id -> Nullable<Uuid>,
        title -> Text,
        result -> Text,
        notes -> Nullable<Text>,
        follow_up_requested -> Bool,
        follow_up_task_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_progress_reports (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        milestone_id -> Nullable<Text>,
        title -> Text,
        period_start -> Date,
        period_end -> Date,
        reported_percent_complete -> Numeric,
        planned_percent_complete -> Nullable<Numeric>,
        project_percent_complete -> Nullable<Numeric>,
        source -> Text,
        status -> Text,
        notes -> Nullable<Text>,
        reported_by -> Text,
        reviewed_by -> Nullable<Text>,
        reviewed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_defects (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        inspection_id -> Nullable<Uuid>,
        task_id -> Nullable<Text>,
        title -> Text,
        description -> Nullable<Text>,
        location -> Nullable<Text>,
        severity -> Text,
        status -> Text,
        reported_by -> Text,
        assigned_to -> Nullable<Text>,
        follow_up_requested -> Bool,
        follow_up_task_id -> Nullable<Text>,
        resolved_at -> Nullable<Timestamptz>,
        verified_by -> Nullable<Text>,
        verified_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_approvals (id) {
        id -> Uuid,
        target_kind -> Text,
        target_id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        approver_id -> Text,
        decision -> Text,
        comment -> Nullable<Text>,
        decided_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_daily_logs (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        log_date -> Date,
        supervisor_id -> Text,
        weather -> Nullable<Text>,
        workforce_count -> Int8,
        work_done -> Nullable<Text>,
        materials_received -> Nullable<Text>,
        incidents_on_site -> Nullable<Text>,
        signed_by -> Nullable<Text>,
        signed_at -> Nullable<Timestamptz>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    supervision_photos (id) {
        id -> Uuid,
        project_id -> Text,
        company_id -> Nullable<Uuid>,
        subject_kind -> Text,
        subject_id -> Uuid,
        storage_target -> Text,
        storage_key -> Text,
        content_sha256 -> Text,
        content_type -> Text,
        size_bytes -> Int8,
        caption -> Nullable<Text>,
        uploaded_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
