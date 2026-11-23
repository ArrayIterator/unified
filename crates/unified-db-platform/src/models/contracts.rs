// Diesel models for the contracts domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Contracts (migration 000048_contracts).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = contract_templates)]
pub struct ContractTemplate {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub body: String,
    pub placeholders: Vec<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = contract_templates)]
pub struct NewContractTemplate {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub body: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = contracts)]
pub struct Contract {
    pub id: Uuid,
    pub template_id: Option<Uuid>,
    pub title: String,
    pub company_party: String,
    pub counterparty_name: String,
    pub counterparty_email: String,
    pub terms: String,
    pub value: PgNumeric,
    pub currency: String,
    pub signing_order: String,
    pub status: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub signed_at: Option<DateTime<Utc>>,
    pub content_hash: Option<String>,
    pub void_reason: Option<String>,
    pub terminate_reason: Option<String>,
    pub renewed_from_id: Option<Uuid>,
    pub alert_days: Vec<i32>,
    pub expiry_alert_status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = contracts)]
pub struct NewContract {
    pub id: Uuid,
    pub template_id: Option<Uuid>,
    pub title: String,
    pub company_party: String,
    pub counterparty_name: String,
    pub counterparty_email: String,
    pub terms: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub signed_at: Option<DateTime<Utc>>,
    pub content_hash: Option<String>,
    pub void_reason: Option<String>,
    pub terminate_reason: Option<String>,
    pub renewed_from_id: Option<Uuid>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = contract_signers)]
pub struct ContractSigner {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub name: String,
    pub email: String,
    pub order_index: i32,
    pub status: String,
    pub token_hash: String,
    pub token_expires_at: DateTime<Utc>,
    pub signed_at: Option<DateTime<Utc>>,
    pub declined_reason: Option<String>,
    pub viewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = contract_signers)]
pub struct NewContractSigner {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub name: String,
    pub email: String,
    pub token_hash: String,
    pub token_expires_at: DateTime<Utc>,
    pub signed_at: Option<DateTime<Utc>>,
    pub declined_reason: Option<String>,
    pub viewed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = contract_audit_trail)]
pub struct ContractAuditTrail {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub signer_id: Option<Uuid>,
    pub actor: String,
    pub action: String,
    pub ip_hash: String,
    pub user_agent_hash: String,
    pub details: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = contract_audit_trail)]
pub struct NewContractAuditTrail {
    pub id: Uuid,
    pub contract_id: Uuid,
    pub signer_id: Option<Uuid>,
    pub actor: String,
    pub action: String,
    pub ip_hash: String,
    pub user_agent_hash: String,
    pub details: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = auto_open_rules)]
pub struct AutoOpenRule {
    pub service_id: String,
    pub enabled: bool,
    pub severity: String,
    pub auto_resolve_on_recovery: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = auto_open_rules)]
pub struct NewAutoOpenRule {
    pub service_id: String,
}
