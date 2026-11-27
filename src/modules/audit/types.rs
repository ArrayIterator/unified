// Audit module - shared types and request/response shapes.
//
// Split from `src/modules/audit.rs`; the module root re-exports the public API of this module.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// Types

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: String,
    pub seq: u64,
    pub actor: String,
    pub action: String,
    pub resource: String,
    pub metadata: Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    /// Hash of the previous entry in the chain (`GENESIS_PREV_HASH`
    /// for the first entry). Together with `hash` this makes the log
    /// tamper-evident: deleting, reordering, or editing an entry
    /// breaks every later hash. See `AuditState::verify_chain`.
    pub prev_hash: String,
    /// SHA-256 of this entry chained to `prev_hash`. Computed at
    /// record time by `AuditState::record`; never accepted from input.
    pub hash: String,
}

/// Result of `AuditState::verify_chain`: a recomputation of every
/// stored entry hash plus the sequence/previous-hash linkage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainVerification {
    /// True when every stored entry hash recomputes and the chain
    /// links (sequence numbers and previous hashes) are unbroken.
    pub valid: bool,
    /// Number of entries checked.
    pub checked: u64,
    /// Hash of the newest stored entry (empty when the log is empty).
    pub head_hash: String,
    /// Sequence number of the first entry that failed verification,
    /// when `valid` is false.
    pub first_bad_seq: Option<u64>,
}
