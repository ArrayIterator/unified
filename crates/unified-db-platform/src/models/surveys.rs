// Diesel models for the surveys domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Surveys (migration 000055_surveys).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = surveys)]
pub struct Survey {
    pub id: Uuid,
    pub family_id: Uuid,
    pub version: i32,
    pub supersedes: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = surveys)]
pub struct NewSurvey {
    pub id: Uuid,
    pub family_id: Uuid,
    pub version: i32,
    pub supersedes: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = survey_questions)]
pub struct SurveyQuestion {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub position: i32,
    pub question_type: String,
    pub text: String,
    pub required: bool,
    pub options: Value,
    pub min_value: Option<i32>,
    pub max_value: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = survey_questions)]
pub struct NewSurveyQuestion {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub position: i32,
    pub question_type: String,
    pub text: String,
    pub min_value: Option<i32>,
    pub max_value: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = survey_distributions)]
pub struct SurveyDistribution {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub trigger: String,
    pub department_id: Option<String>,
    pub product_id: Option<String>,
    pub agent_id: Option<String>,
    pub responsible_user_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub expires_in_days: i32,
    pub status: String,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub sent_count: i64,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = survey_distributions)]
pub struct NewSurveyDistribution {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub trigger: String,
    pub department_id: Option<String>,
    pub product_id: Option<String>,
    pub agent_id: Option<String>,
    pub responsible_user_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub scheduler_job_id: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = survey_responses)]
pub struct SurveyResponse {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub distribution_id: Option<Uuid>,
    pub recipient_id: String,
    pub recipient_email: Option<String>,
    pub agent_id: Option<String>,
    pub department_id: Option<String>,
    pub product_id: Option<String>,
    pub token: String,
    pub token_expires_at: DateTime<Utc>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub follow_up_status: String,
    pub follow_up_task_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = survey_responses)]
pub struct NewSurveyResponse {
    pub id: Uuid,
    pub survey_id: Uuid,
    pub distribution_id: Option<Uuid>,
    pub recipient_id: String,
    pub recipient_email: Option<String>,
    pub agent_id: Option<String>,
    pub department_id: Option<String>,
    pub product_id: Option<String>,
    pub token: String,
    pub token_expires_at: DateTime<Utc>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub follow_up_task_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = survey_answers)]
pub struct SurveyAnswer {
    pub id: Uuid,
    pub response_id: Uuid,
    pub question_id: Uuid,
    pub score: Option<i32>,
    pub text: Option<String>,
    pub choice: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = survey_answers)]
pub struct NewSurveyAnswer {
    pub id: Uuid,
    pub response_id: Uuid,
    pub question_id: Uuid,
    pub score: Option<i32>,
    pub text: Option<String>,
    pub choice: Option<String>,
}
