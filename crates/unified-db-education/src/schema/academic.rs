// Diesel schema for the academic domain of the unified-db-education bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// Academic system (migration 000094_academic).

diesel::table! {
    academic_programs (id) {
        id -> Uuid,
        code -> Text,
        name -> Text,
        department_id -> Nullable<Text>,
        max_credits_per_semester -> Nullable<Numeric>,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_students (id) {
        id -> Uuid,
        user_id -> Text,
        student_number -> Text,
        program_id -> Uuid,
        advisor_id -> Nullable<Uuid>,
        enrollment_year -> Int4,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_lecturers (id) {
        id -> Uuid,
        user_id -> Text,
        lecturer_number -> Text,
        department_id -> Nullable<Text>,
        title -> Nullable<Text>,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_subjects (id) {
        id -> Uuid,
        code -> Text,
        title -> Text,
        program_id -> Uuid,
        credits -> Numeric,
        semester_number -> Nullable<Int4>,
        elearning_course_id -> Nullable<Text>,
        elearning_course_verified -> Bool,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_rooms (id) {
        id -> Uuid,
        code -> Text,
        name -> Text,
        building -> Nullable<Text>,
        capacity -> Nullable<Int4>,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_semesters (id) {
        id -> Uuid,
        code -> Text,
        name -> Text,
        start_date -> Date,
        end_date -> Date,
        status -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    academic_calendar_events (id) {
        id -> Uuid,
        semester_id -> Nullable<Uuid>,
        event_type -> Text,
        title -> Text,
        start_date -> Date,
        end_date -> Date,
        description -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
