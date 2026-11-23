// Diesel schema for the risk domain of the unified-db-operations bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    risks (id) {
        id -> Uuid,
        company_id -> Nullable<Uuid>,
        title -> Varchar,
        description -> Text,
        category -> Varchar,
        likelihood -> Int2,
        impact -> Int2,
        score -> Int2,
        band -> Varchar,
        owner_id -> Varchar,
        status -> Varchar,
        next_review_at -> Nullable<Timestamptz>,
        alert_threshold -> Int2,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_mitigation_plans (id) {
        id -> Uuid,
        risk_id -> Uuid,
        title -> Varchar,
        description -> Text,
        target_date -> Nullable<Timestamptz>,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_mitigation_tasks (id) {
        id -> Uuid,
        plan_id -> Uuid,
        risk_id -> Uuid,
        title -> Varchar,
        description -> Text,
        assignee_id -> Nullable<Varchar>,
        status -> Varchar,
        due_date -> Nullable<Timestamptz>,
        external_project_task_id -> Nullable<Varchar>,
        awaiting_projects_handler -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_templates (id) {
        id -> Uuid,
        title -> Varchar,
        description -> Text,
        category -> Varchar,
        created_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_template_items (id) {
        id -> Uuid,
        template_id -> Uuid,
        title -> Varchar,
        description -> Text,
        required -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_instances (id) {
        id -> Uuid,
        template_id -> Uuid,
        company_id -> Nullable<Uuid>,
        risk_id -> Nullable<Uuid>,
        title -> Varchar,
        status -> Varchar,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_checklist_instance_items (id) {
        id -> Uuid,
        instance_id -> Uuid,
        template_item_id -> Uuid,
        title -> Varchar,
        required -> Bool,
        sort_order -> Int4,
        completed -> Bool,
        evidence -> Text,
        completed_by -> Nullable<Varchar>,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    risk_alerts (id) {
        id -> Uuid,
        risk_id -> Uuid,
        company_id -> Nullable<Uuid>,
        kind -> Varchar,
        message -> Text,
        score -> Nullable<Int2>,
        notified_user_id -> Nullable<Varchar>,
        figures -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
