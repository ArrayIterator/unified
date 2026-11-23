// Diesel schema for the academic_records domain of the unified-db-education bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

diesel::table! {
    academic_schedules (id) {
        id -> Uuid,
        subject_id -> Uuid,
        lecturer_id -> Uuid,
        room_id -> Uuid,
        semester_id -> Uuid,
        day_of_week -> Int4,
        start_time -> Text,
        end_time -> Text,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_enrollments (id) {
        id -> Uuid,
        student_id -> Uuid,
        subject_id -> Uuid,
        semester_id -> Uuid,
        credits -> Numeric,
        status -> Text,
        advisor_id -> Nullable<Uuid>,
        rejection_reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_attendance (id) {
        id -> Uuid,
        schedule_id -> Uuid,
        student_id -> Uuid,
        date -> Date,
        status -> Text,
        source -> Text,
        entry_id -> Nullable<Text>,
        minutes -> Nullable<Int8>,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_grade_components (id) {
        id -> Uuid,
        subject_id -> Uuid,
        name -> Text,
        kind -> Text,
        weight_percent -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_grades (id) {
        id -> Uuid,
        enrollment_id -> Uuid,
        component_id -> Uuid,
        score -> Numeric,
        graded_by -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_grade_appeals (id) {
        id -> Uuid,
        enrollment_id -> Uuid,
        component_id -> Nullable<Uuid>,
        student_id -> Uuid,
        reason -> Text,
        status -> Text,
        reviewed_by -> Nullable<Uuid>,
        decision -> Nullable<Text>,
        corrected_score -> Nullable<Numeric>,
        resolution_notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_transcripts (id) {
        id -> Uuid,
        student_id -> Uuid,
        issued_by -> Text,
        gpa -> Numeric,
        total_credits -> Numeric,
        lines -> Jsonb,
        print_job_id -> Nullable<Text>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    academic_theses (id) {
        id -> Uuid,
        student_id -> Uuid,
        title -> Text,
        advisor_id -> Uuid,
        co_advisor_id -> Nullable<Uuid>,
        status -> Text,
        proposal_date -> Nullable<Date>,
        defense_date -> Nullable<Date>,
        defense_result -> Nullable<Text>,
        notes -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_standing_records (id) {
        id -> Uuid,
        student_id -> Uuid,
        semester_id -> Uuid,
        gpa -> Numeric,
        total_credits -> Numeric,
        standing -> Text,
        reason -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_definitions (id) {
        id -> Uuid,
        kind -> Text,
        key -> Text,
        label -> Text,
        config -> Jsonb,
        active -> Bool,
        sort_order -> Int4,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_settings (key) {
        key -> Text,
        value -> Jsonb,
        description -> Nullable<Text>,
        updated_at -> Timestamptz,
    }
}
