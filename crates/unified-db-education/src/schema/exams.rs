// Diesel schema for the exams domain of the unified-db-education bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Online exams (migration 000095_exams).
// No FKs by design (see migration notes): no joinable!s.
// ---------------------------------------------------------------------------

diesel::table! {
    exam_question_banks (id) {
        id -> Uuid,
        title -> Text,
        description -> Text,
        category -> Text,
        owner_id -> Text,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_questions (id) {
        id -> Uuid,
        bank_id -> Uuid,
        version -> Int4,
        previous_version_id -> Nullable<Uuid>,
        superseded_by -> Nullable<Uuid>,
        question_type -> Text,
        category -> Text,
        difficulty -> Text,
        text -> Text,
        options -> Jsonb,
        correct_answer -> Nullable<Jsonb>,
        answer_key -> Nullable<Text>,
        marks -> Float8,
        status -> Text,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_question_versions (id) {
        id -> Uuid,
        question_id -> Uuid,
        version -> Int4,
        snapshot -> Jsonb,
        reason -> Nullable<Text>,
        created_by -> Text,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    exams (id) {
        id -> Uuid,
        title -> Text,
        description -> Text,
        category -> Text,
        bank_ids -> Array<Uuid>,
        course_id -> Nullable<Text>,
        academic_enrollment_id -> Nullable<Text>,
        academic_component_id -> Nullable<Text>,
        status -> Text,
        opens_at -> Nullable<Timestamptz>,
        closes_at -> Nullable<Timestamptz>,
        duration_minutes -> Int4,
        pass_mark -> Float8,
        attempts_allowed -> Int4,
        price_minor -> Int8,
        is_paid -> Bool,
        settings -> Jsonb,
        total_marks -> Float8,
        created_by -> Text,
        company_id -> Nullable<Text>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_question_links (id) {
        id -> Uuid,
        exam_id -> Uuid,
        question_id -> Uuid,
        question_version -> Int4,
        position -> Int4,
        section -> Nullable<Text>,
        marks -> Float8,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    exam_rubrics (id) {
        id -> Uuid,
        exam_id -> Uuid,
        title -> Text,
        description -> Text,
        criteria -> Jsonb,
        created_by -> Text,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_attempts (id) {
        id -> Uuid,
        exam_id -> Uuid,
        user_id -> Text,
        attempt_number -> Int4,
        status -> Text,
        started_at -> Timestamptz,
        expires_at -> Timestamptz,
        duration_seconds -> Int8,
        seed -> Int8,
        question_order -> Jsonb,
        option_orders -> Jsonb,
        answers -> Jsonb,
        autosaved_at -> Nullable<Timestamptz>,
        submitted_at -> Nullable<Timestamptz>,
        score -> Nullable<Float8>,
        max_score -> Float8,
        passed -> Nullable<Bool>,
        graded_at -> Nullable<Timestamptz>,
        result_id -> Nullable<Uuid>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_grades (id) {
        id -> Uuid,
        attempt_id -> Uuid,
        exam_id -> Uuid,
        question_id -> Uuid,
        grader_id -> Nullable<Text>,
        rubric_id -> Nullable<Uuid>,
        criterion_scores -> Jsonb,
        marks_awarded -> Nullable<Float8>,
        max_marks -> Float8,
        status -> Text,
        comment -> Nullable<Text>,
        second_grader_id -> Nullable<Text>,
        audit -> Jsonb,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_proctor_signals (id) {
        id -> Uuid,
        attempt_id -> Uuid,
        exam_id -> Uuid,
        user_id -> Text,
        signal_type -> Text,
        detail -> Nullable<Text>,
        recorded_at -> Timestamptz,
    }
}

diesel::table! {
    exam_results (id) {
        id -> Uuid,
        attempt_id -> Uuid,
        exam_id -> Uuid,
        user_id -> Text,
        score -> Float8,
        max_score -> Float8,
        percentage -> Float8,
        passed -> Bool,
        status -> Text,
        per_question -> Jsonb,
        graded_at -> Timestamptz,
        published_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    exam_activations (id) {
        id -> Uuid,
        exam_id -> Uuid,
        user_id -> Text,
        order_id -> Nullable<Text>,
        status -> Text,
        activated_at -> Timestamptz,
        revoked_at -> Nullable<Timestamptz>,
    }
}
