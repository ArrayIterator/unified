// Diesel schema for the okr domain of the unified-db-people bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// OKRs (migration 000043_okrs). `OkrObjective`, `OkrKeyResult` and
// `OkrCheckin` carry NUMERIC columns, so per the repo header rule
// they skip ALL serde derives. The self-reference
// `okr_objectives.parent_objective_id` gets no `joinable!` (Diesel
// does not allow a table to join to itself in `allow_tables` form).
// ---------------------------------------------------------------------------

diesel::table! {
    okr_cycles (id) {
        id -> Uuid,
        company_id -> Varchar,
        name -> Varchar,
        description -> Nullable<Text>,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    okr_objectives (id) {
        id -> Uuid,
        cycle_id -> Uuid,
        company_id -> Varchar,
        parent_objective_id -> Nullable<Uuid>,
        title -> Varchar,
        description -> Nullable<Text>,
        level -> Varchar,
        owner_id -> Varchar,
        final_score -> Nullable<Numeric>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    okr_key_results (id) {
        id -> Uuid,
        objective_id -> Uuid,
        title -> Varchar,
        unit -> Varchar,
        target_value -> Numeric,
        start_value -> Numeric,
        current_value -> Numeric,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    okr_checkins (id) {
        id -> Uuid,
        key_result_id -> Uuid,
        author_id -> Varchar,
        note -> Text,
        confidence -> Varchar,
        kind -> Varchar,
        reason -> Nullable<Text>,
        value_snapshot -> Nullable<Numeric>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    okr_links (id) {
        id -> Uuid,
        key_result_id -> Uuid,
        link_type -> Varchar,
        target_id -> Uuid,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
