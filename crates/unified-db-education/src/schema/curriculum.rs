// Diesel schema for the curriculum domain of the unified-db-education bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Curriculum (migration 000092_curriculum).

diesel::table! {
    curriculum_frameworks (id) {
        id -> Uuid,
        code -> Varchar,
        name -> Varchar,
        description -> Text,
        country -> Varchar,
        is_active -> Bool,
        is_built_in -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_levels (id) {
        id -> Uuid,
        framework_id -> Uuid,
        code -> Varchar,
        name -> Varchar,
        grade_count -> Int4,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_subjects (id) {
        id -> Uuid,
        framework_id -> Uuid,
        level_id -> Uuid,
        code -> Varchar,
        name -> Varchar,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_core_competencies (id) {
        id -> Uuid,
        framework_id -> Uuid,
        subject_id -> Uuid,
        level_id -> Uuid,
        code -> Varchar,
        title -> Varchar,
        description -> Text,
        grade_year -> Int4,
        version -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_basic_competencies (id) {
        id -> Uuid,
        framework_id -> Uuid,
        core_competency_id -> Uuid,
        code -> Varchar,
        title -> Varchar,
        description -> Text,
        grade_year -> Int4,
        version -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_structures (id) {
        id -> Uuid,
        framework_id -> Uuid,
        level_id -> Uuid,
        grade_year -> Int4,
        subject_id -> Uuid,
        hours_per_week -> Numeric,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_assessment_domains (id) {
        id -> Uuid,
        code -> Varchar,
        name -> Varchar,
        parent_domain_id -> Nullable<Uuid>,
        description -> Text,
        sort_order -> Int4,
        is_active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_grade_descriptors (id) {
        id -> Uuid,
        framework_id -> Uuid,
        domain_id -> Nullable<Uuid>,
        min_score -> Numeric,
        max_score -> Numeric,
        grade_label -> Varchar,
        descriptor -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_lesson_plans (id) {
        id -> Uuid,
        framework_id -> Uuid,
        subject_id -> Uuid,
        core_competency_id -> Nullable<Uuid>,
        basic_competency_id -> Nullable<Uuid>,
        title -> Varchar,
        objectives -> Array<Text>,
        materials -> Array<Text>,
        activities -> Array<Text>,
        assessment_notes -> Text,
        time_minutes -> Int4,
        grade_year -> Int4,
        semester -> Int4,
        teacher_id -> Varchar,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_report_cards (id) {
        id -> Uuid,
        student_ref -> Varchar,
        framework_id -> Uuid,
        level_id -> Uuid,
        grade_year -> Int4,
        semester -> Int4,
        academic_year -> Varchar,
        status -> Varchar,
        issued_at -> Nullable<Timestamptz>,
        issued_by -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_report_card_entries (id) {
        id -> Uuid,
        report_card_id -> Uuid,
        subject_id -> Uuid,
        domain_id -> Uuid,
        score -> Nullable<Numeric>,
        grade_label -> Nullable<Varchar>,
        descriptor -> Nullable<Text>,
        notes -> Text,
        academic_grade_id -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_interventions (id) {
        id -> Uuid,
        student_ref -> Varchar,
        basic_competency_id -> Uuid,
        kind -> Varchar,
        status -> Varchar,
        teacher_id -> Varchar,
        scheduled_at -> Nullable<Timestamptz>,
        completed_at -> Nullable<Timestamptz>,
        notes -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    curriculum_settings (key) {
        key -> Text,
        value -> Jsonb,
        updated_at -> Timestamptz,
    }
}
