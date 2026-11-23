// Diesel schema for the elearning domain of the unified-db-education bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// E-learning (migration 000079_elearning). Course certificates
// live in `elearning_certificates` below: the `certificates` name
// already belongs to the SSL certificates table (migration
// 000005), and the migration table was renamed during the final
// inspection to resolve the collision.
// ---------------------------------------------------------------------------

diesel::table! {
    courses (id) {
        id -> Uuid,
        title -> Text,
        description -> Text,
        category -> Text,
        level -> Text,
        language -> Text,
        thumbnail_ref -> Nullable<Text>,
        instructor_id -> Text,
        status -> Text,
        price_minor -> Int8,
        currency -> Text,
        certificate_enabled -> Bool,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    course_modules (id) {
        id -> Uuid,
        course_id -> Uuid,
        title -> Text,
        description -> Nullable<Text>,
        position -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    lessons (id) {
        id -> Uuid,
        module_id -> Uuid,
        course_id -> Uuid,
        title -> Text,
        lesson_type -> Text,
        content_ref -> Nullable<Text>,
        body -> Nullable<Text>,
        position -> Int8,
        required -> Bool,
        available_from -> Nullable<Timestamptz>,
        requires_previous -> Bool,
        duration_minutes -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    enrollments (id) {
        id -> Uuid,
        course_id -> Uuid,
        user_id -> Text,
        status -> Text,
        price_paid_minor -> Int8,
        currency -> Text,
        order_id -> Nullable<Text>,
        learner_name -> Nullable<Text>,
        enrolled_at -> Timestamptz,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    lesson_progress (id) {
        id -> Uuid,
        enrollment_id -> Uuid,
        lesson_id -> Uuid,
        course_id -> Uuid,
        user_id -> Text,
        completed -> Bool,
        completed_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quizzes (id) {
        id -> Uuid,
        lesson_id -> Uuid,
        course_id -> Uuid,
        title -> Text,
        description -> Nullable<Text>,
        pass_mark -> Int8,
        max_attempts -> Nullable<Int8>,
        allow_review -> Bool,
        time_limit_minutes -> Nullable<Int8>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quiz_questions (id) {
        id -> Uuid,
        quiz_id -> Uuid,
        question -> Text,
        options -> Jsonb,
        correct_index -> Int8,
        points -> Int8,
        position -> Int8,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    quiz_attempts (id) {
        id -> Uuid,
        quiz_id -> Uuid,
        lesson_id -> Uuid,
        enrollment_id -> Uuid,
        user_id -> Text,
        answers -> Jsonb,
        score -> Int8,
        max_score -> Int8,
        percent -> Int8,
        passed -> Bool,
        submitted_at -> Timestamptz,
    }
}

diesel::table! {
    elearning_certificates (id) {
        id -> Uuid,
        code -> Text,
        course_id -> Uuid,
        user_id -> Text,
        learner_name -> Text,
        course_title -> Text,
        issuer_name -> Text,
        issued_at -> Timestamptz,
        content_sha256 -> Text,
        status -> Text,
        anchor_status -> Text,
        anchor_tx -> Nullable<Text>,
        revoked_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    reviews (id) {
        id -> Uuid,
        course_id -> Uuid,
        user_id -> Text,
        rating -> Int8,
        comment -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    learning_paths (id) {
        id -> Uuid,
        title -> Text,
        description -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    learning_path_courses (id) {
        id -> Uuid,
        path_id -> Uuid,
        course_id -> Uuid,
        position -> Int8,
        created_at -> Timestamptz,
    }
}
