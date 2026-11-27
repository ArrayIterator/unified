// Diesel models for the gamification domain of the unified-db-infra bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Gamification (migration 000059_gamification).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_rules)]
pub struct GamificationRule {
    pub id: Uuid,
    pub name: String,
    pub action: String,
    pub points: i64,
    pub company_id: Option<String>,
    pub daily_cap: Option<i64>,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_rules)]
pub struct NewGamificationRule {
    pub id: Uuid,
    pub name: String,
    pub action: String,
    pub points: i64,
    pub company_id: Option<String>,
    pub daily_cap: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_points_ledger)]
pub struct GamificationPointsLedger {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<String>,
    pub department_id: Option<String>,
    pub rule_id: Option<Uuid>,
    pub action: String,
    pub points: i64,
    pub balance_after: i64,
    pub season_id: Option<Uuid>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_points_ledger)]
pub struct NewGamificationPointsLedger {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Option<String>,
    pub department_id: Option<String>,
    pub rule_id: Option<Uuid>,
    pub action: String,
    pub points: i64,
    pub balance_after: i64,
    pub season_id: Option<Uuid>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_badges)]
pub struct GamificationBadge {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub criteria_action: String,
    pub criteria_threshold: i64,
    pub icon: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_badges)]
pub struct NewGamificationBadge {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub criteria_action: String,
    pub criteria_threshold: i64,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_awards)]
pub struct GamificationAward {
    pub id: Uuid,
    pub badge_id: Uuid,
    pub user_id: String,
    pub reason: String,
    pub awarded_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_awards)]
pub struct NewGamificationAward {
    pub id: Uuid,
    pub badge_id: Uuid,
    pub user_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_seasons)]
pub struct GamificationSeason {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_seasons)]
pub struct NewGamificationSeason {
    pub id: Uuid,
    pub name: String,
    pub company_id: Option<String>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_rewards)]
pub struct GamificationReward {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub points_cost: i64,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_rewards)]
pub struct NewGamificationReward {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub points_cost: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = gamification_reward_requests)]
pub struct GamificationRewardRequest {
    pub id: Uuid,
    pub reward_id: Uuid,
    pub user_id: String,
    pub status: String,
    pub points_spent: i64,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = gamification_reward_requests)]
pub struct NewGamificationRewardRequest {
    pub id: Uuid,
    pub reward_id: Uuid,
    pub user_id: String,
    pub points_spent: i64,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}
