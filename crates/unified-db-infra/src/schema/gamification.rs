// Diesel schema for the gamification domain of the unified-db-infra bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); the tables below are
// byte-identical to the originals, only regrouped.

// ---------------------------------------------------------------------------
// Gamification (migration 000059_gamification).
// ---------------------------------------------------------------------------

diesel::table! {
    gamification_rules (id) {
        id -> Uuid,
        name -> Varchar,
        action -> Varchar,
        points -> Int8,
        company_id -> Nullable<Varchar>,
        daily_cap -> Nullable<Int8>,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_points_ledger (id) {
        id -> Uuid,
        user_id -> Varchar,
        company_id -> Nullable<Varchar>,
        department_id -> Nullable<Varchar>,
        rule_id -> Nullable<Uuid>,
        action -> Varchar,
        points -> Int8,
        balance_after -> Int8,
        season_id -> Nullable<Uuid>,
        reason -> Nullable<Varchar>,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_badges (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        criteria_action -> Varchar,
        criteria_threshold -> Int8,
        icon -> Nullable<Varchar>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_awards (id) {
        id -> Uuid,
        badge_id -> Uuid,
        user_id -> Varchar,
        reason -> Varchar,
        awarded_at -> Timestamptz,
        created_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_seasons (id) {
        id -> Uuid,
        name -> Varchar,
        company_id -> Nullable<Varchar>,
        starts_at -> Timestamptz,
        ends_at -> Timestamptz,
        status -> Varchar,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_rewards (id) {
        id -> Uuid,
        name -> Varchar,
        description -> Text,
        points_cost -> Int8,
        active -> Bool,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}

diesel::table! {
    gamification_reward_requests (id) {
        id -> Uuid,
        reward_id -> Uuid,
        user_id -> Varchar,
        status -> Varchar,
        points_spent -> Int8,
        decided_by -> Nullable<Varchar>,
        decided_at -> Nullable<Timestamptz>,
        created_at -> Timestamptz,
        updated_at -> Timestamptz,
    }
}
