// Audit module - in-memory state.
//
// Split from `src/modules/audit.rs`; the module root re-exports the public API of this module.
//
// Tamper-evidence: every entry carries a hash chain. Each entry hash
// is SHA-256 over the entry fields chained to the previous entry
// hash, so any edit, deletion, or reordering of a stored entry is
// detectable through `verify_chain`. The chain head and sequence
// number are also written to a small watermark file after every
// record (see `enable_persistence`), so a restart cannot silently
// reset the sequence counter: a fresh process resumes after the
// last persisted sequence instead of starting again at 1.
//
// Known remaining gap: the entries themselves are still held only
// in memory. The watermark proves that entries existed and in what
// order, but it cannot restore entries lost to a restart. Full
// durability needs the entries persisted to the `audit_entries`
// table (which has no hash columns yet, so that also needs a
// migration) — out of scope for this pass.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Mutex;
use uuid::Uuid;

use super::types::{AuditEntry, ChainVerification};

/// Previous-hash value of the first entry in a fresh chain.
pub(crate) const GENESIS_PREV_HASH: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// Environment variable overriding the watermark file path. When it
/// is unset, `AuditModule::init` uses `audit-chain-head.json` in the
/// process working directory.
pub(crate) const WATERMARK_PATH_ENV: &str = "UNIFIED_AUDIT_WATERMARK_PATH";

/// Default watermark file name (relative to the working directory).
pub(crate) const DEFAULT_WATERMARK_PATH: &str = "audit-chain-head.json";

/// Canonical hash input for one entry. The fields are joined with
/// unit separators so no two distinct field tuples can collide into
/// the same input string.
pub(crate) fn compute_entry_hash(
    seq: u64,
    prev_hash: &str,
    actor: &str,
    action: &str,
    resource: &str,
    ip: Option<&str>,
    created_at: DateTime<Utc>,
    metadata: &Value,
) -> String {
    let metadata_json = serde_json::to_string(metadata).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(seq.to_string().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(prev_hash.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(actor.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(action.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(resource.as_bytes());
    hasher.update(b"\x1f");
    hasher.update(ip.unwrap_or("").as_bytes());
    hasher.update(b"\x1f");
    hasher.update(created_at.to_rfc3339().as_bytes());
    hasher.update(b"\x1f");
    hasher.update(metadata_json.as_bytes());
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

pub struct AuditState {
    pub(crate) entries: DashMap<String, AuditEntry>,
    pub(crate) seq: AtomicU64,
    /// Hash of the newest recorded entry (`GENESIS_PREV_HASH` while
    /// the log is empty). Guarded by a mutex because `record` reads
    /// and advances it as one step.
    pub(crate) head: Mutex<String>,
    /// Watermark file path once persistence is enabled (by
    /// `AuditModule::init`); `None` keeps the state purely in
    /// memory, which is what unit tests use.
    pub(crate) watermark_path: Mutex<Option<PathBuf>>,
}

impl AuditState {
    // `pub(crate)`: constructed by `AuditModule::new` in routes.rs.
    pub(crate) fn new() -> Self {
        Self {
            entries: DashMap::new(),
            seq: AtomicU64::new(0),
            head: Mutex::new(GENESIS_PREV_HASH.to_string()),
            watermark_path: Mutex::new(None),
        }
    }

    /// State with watermark persistence enabled immediately: loads
    /// the sequence number and chain head from `path` when the file
    /// exists, so recording resumes after the last persisted entry
    /// instead of restarting the sequence at 1.
    pub(crate) fn with_watermark(path: PathBuf) -> Self {
        let state = Self::new();
        state.enable_persistence(path);
        state
    }

    /// Enable watermark persistence at `path`, loading any existing
    /// watermark first. Only applies while the log is still empty,
    /// so a second call (or a late `init`) can never rewind a live
    /// chain. Load failures are ignored: a missing or corrupt
    /// watermark starts a fresh chain rather than failing startup.
    pub(crate) fn enable_persistence(&self, path: PathBuf) {
        if !self.entries.is_empty() {
            return;
        }
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(saved) = serde_json::from_str::<Value>(&raw) {
                if let Some(seq) = saved.get("seq").and_then(Value::as_u64) {
                    self.seq.store(seq, Ordering::SeqCst);
                }
                if let Some(head) = saved.get("head_hash").and_then(Value::as_str) {
                    if head.len() == 64 && head.chars().all(|c| c.is_ascii_hexdigit()) {
                        if let Ok(mut guard) = self.head.lock() {
                            *guard = head.to_string();
                        }
                    }
                }
            }
        }
        if let Ok(mut guard) = self.watermark_path.lock() {
            *guard = Some(path);
        }
    }

    /// Current chain head hash (empty-log genesis value included).
    pub(crate) fn head_hash(&self) -> String {
        self.head
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_else(|_| GENESIS_PREV_HASH.to_string())
    }

    /// Write the `{seq, head_hash}` watermark atomically (temporary
    /// file plus rename). Best-effort: a failure is reported on
    /// stderr but never fails the record itself.
    fn persist_watermark(&self, seq: u64, head: &str) {
        let Some(path) = self
            .watermark_path
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or(None)
        else {
            return;
        };
        let payload = serde_json::json!({ "seq": seq, "head_hash": head });
        let raw = match serde_json::to_string(&payload) {
            Ok(raw) => raw,
            Err(_) => return,
        };
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, raw).is_err() {
            eprintln!("Audit watermark: cannot write {}", tmp.display());
            return;
        }
        if std::fs::rename(&tmp, &path).is_err() {
            eprintln!("Audit watermark: cannot publish {}", path.display());
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// Append one entry. This is the only way entries are created.
    /// `pub(crate)`: called by the hook handlers in routes.rs.
    pub(crate) fn record(&self, event: &str, data: &Value) -> AuditEntry {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let actor = data
            .get("actor_id")
            .or_else(|| data.get("actor"))
            .or_else(|| data.get("user_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("system")
            .to_string();
        let resource = data
            .get("resource")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or_else(|| {
                data.get("id")
                    .and_then(|v| v.as_str())
                    .map(|id| format!("{event}:{id}"))
            })
            .or_else(|| {
                data.get("order_id")
                    .and_then(|v| v.as_str())
                    .map(|id| format!("order:{id}"))
            })
            .or_else(|| {
                data.get("module_id")
                    .and_then(|v| v.as_str())
                    .map(|id| format!("module:{id}"))
            })
            .unwrap_or_else(|| event.to_string());
        let ip = data
            .get("ip")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let created_at = Utc::now();
        let prev_hash = self.head_hash();
        let hash = compute_entry_hash(
            seq,
            &prev_hash,
            &actor,
            event,
            &resource,
            ip.as_deref(),
            created_at,
            data,
        );
        let entry = AuditEntry {
            id: Uuid::new_v4().to_string(),
            seq,
            actor,
            action: event.to_string(),
            resource,
            metadata: data.clone(),
            ip,
            created_at,
            prev_hash,
            hash: hash.clone(),
        };
        self.entries.insert(entry.id.clone(), entry.clone());
        if let Ok(mut guard) = self.head.lock() {
            *guard = hash.clone();
        }
        self.persist_watermark(seq, &hash);
        entry
    }

    /// Recompute every stored entry hash and check the chain links.
    /// Entries are verified in sequence order; the first entry must
    /// hash against its own stored `prev_hash`, and every later
    /// entry must link to the previous entry's hash with consecutive
    /// sequence numbers. A tampered, deleted, or reordered entry
    /// fails here. Note this only covers entries still held in
    /// memory — entries lost to a restart are not verifiable until
    /// full database persistence lands (see the module note above).
    pub(crate) fn verify_chain(&self) -> ChainVerification {
        let mut items: Vec<AuditEntry> = self
            .entries
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| a.seq.cmp(&b.seq));
        let mut first_bad_seq: Option<u64> = None;
        let mut expected_prev = String::new();
        for (index, entry) in items.iter().enumerate() {
            let recomputed = compute_entry_hash(
                entry.seq,
                &entry.prev_hash,
                &entry.actor,
                &entry.action,
                &entry.resource,
                entry.ip.as_deref(),
                entry.created_at,
                &entry.metadata,
            );
            let links = if index == 0 {
                true
            } else {
                entry.prev_hash == expected_prev
                    && entry.seq == items[index - 1].seq + 1
            };
            if recomputed != entry.hash || !links {
                first_bad_seq = Some(entry.seq);
                break;
            }
            expected_prev = entry.hash.clone();
        }
        let head_hash = items
            .last()
            .map(|entry| entry.hash.clone())
            .unwrap_or_else(|| self.head_hash());
        ChainVerification {
            valid: first_bad_seq.is_none(),
            checked: items.len() as u64,
            head_hash,
            first_bad_seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_assigns_sequence_and_actor() {
        let state = AuditState::new();
        let first = state.record(
            "user.created",
            &serde_json::json!({"actor_id": "u1", "id": "42"}),
        );
        assert_eq!(first.seq, 1);
        assert_eq!(first.actor, "u1");
        assert_eq!(first.action, "user.created");
        assert_eq!(first.resource, "user.created:42");
        assert_eq!(first.ip, None);
        let second = state.record("user.updated", &serde_json::json!({"actor": "u2"}));
        assert_eq!(second.seq, 2);
        assert_eq!(second.actor, "u2");
    }

    #[test]
    fn record_defaults_actor_and_resource() {
        let state = AuditState::new();
        let entry = state.record("order.created", &serde_json::json!({}));
        assert_eq!(entry.actor, "system");
        assert_eq!(entry.resource, "order.created");
        let with_ip = state.record(
            "auth.failed",
            &serde_json::json!({"ip": "1.2.3.4", "resource": "thing:1"}),
        );
        assert_eq!(with_ip.ip.as_deref(), Some("1.2.3.4"));
        assert_eq!(with_ip.resource, "thing:1");
    }

    #[test]
    fn record_chains_hashes_to_previous_entry() {
        let state = AuditState::new();
        let first = state.record("user.created", &serde_json::json!({"actor_id": "u1"}));
        assert_eq!(first.prev_hash, GENESIS_PREV_HASH);
        assert_eq!(first.hash.len(), 64);
        let second = state.record("user.updated", &serde_json::json!({"actor": "u2"}));
        assert_eq!(second.prev_hash, first.hash);
        assert_ne!(second.hash, first.hash);
        let recomputed = compute_entry_hash(
            second.seq,
            &second.prev_hash,
            &second.actor,
            &second.action,
            &second.resource,
            second.ip.as_deref(),
            second.created_at,
            &second.metadata,
        );
        assert_eq!(recomputed, second.hash);
    }

    #[test]
    fn verify_chain_passes_on_untampered_log() {
        let state = AuditState::new();
        state.record("user.created", &serde_json::json!({"actor_id": "u1"}));
        state.record("user.updated", &serde_json::json!({"actor": "u2"}));
        state.record("order.created", &serde_json::json!({"actor": "u3"}));
        let result = state.verify_chain();
        assert!(result.valid);
        assert_eq!(result.checked, 3);
        assert_eq!(result.first_bad_seq, None);
        assert_eq!(result.head_hash.len(), 64);
    }

    #[test]
    fn verify_chain_detects_tampered_entry() {
        let state = AuditState::new();
        let first = state.record("user.created", &serde_json::json!({"actor_id": "u1"}));
        state.record("user.updated", &serde_json::json!({"actor": "u2"}));
        // Simulate an undetected edit: rewrite the stored metadata.
        if let Some(mut stored) = state.entries.get_mut(&first.id) {
            stored.metadata = serde_json::json!({"actor_id": "attacker"});
        }
        let result = state.verify_chain();
        assert!(!result.valid);
        assert_eq!(result.first_bad_seq, Some(1));
    }

    #[test]
    fn verify_chain_detects_deleted_entry() {
        let state = AuditState::new();
        let first = state.record("user.created", &serde_json::json!({"actor_id": "u1"}));
        let second = state.record("user.updated", &serde_json::json!({"actor": "u2"}));
        state.record("order.created", &serde_json::json!({"actor": "u3"}));
        let _ = first;
        // Simulate silent deletion of the middle entry.
        state.entries.remove(&second.id);
        let result = state.verify_chain();
        assert!(!result.valid);
        assert_eq!(result.first_bad_seq, Some(3));
    }

    #[test]
    fn watermark_round_trip_continues_sequence_after_restart() {
        let dir = std::env::temp_dir()
            .join(format!("unified-audit-watermark-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("chain-head.json");
        let first_head = {
            let state = AuditState::with_watermark(path.clone());
            state.record("user.created", &serde_json::json!({"actor_id": "u1"}));
            let second = state.record("user.updated", &serde_json::json!({"actor": "u2"}));
            assert_eq!(second.seq, 2);
            assert!(path.exists());
            second.hash.clone()
        };
        // A fresh process loads the watermark instead of restarting at 1.
        let restarted = AuditState::with_watermark(path.clone());
        let third = restarted.record("order.created", &serde_json::json!({"actor": "u3"}));
        assert_eq!(third.seq, 3);
        assert_eq!(third.prev_hash, first_head);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn watermark_load_ignores_missing_or_corrupt_file() {
        let dir = std::env::temp_dir()
            .join(format!("unified-audit-watermark-bad-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("chain-head.json");
        std::fs::write(&path, "not json at all").expect("write corrupt watermark");
        let state = AuditState::with_watermark(path.clone());
        let entry = state.record("user.created", &serde_json::json!({}));
        assert_eq!(entry.seq, 1);
        assert_eq!(entry.prev_hash, GENESIS_PREV_HASH);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
