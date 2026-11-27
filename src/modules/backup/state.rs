// Backup module - in-memory state.
//
// Split from `src/modules/backup.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::storage::StorageRegistry;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::config::{backup_settings, redact_value};
use super::types::{BackupJob, BackupRun, BackupSettings, DbSnapshot};

// State.

pub struct BackupState {
    pub(crate) jobs: DashMap<String, BackupJob>,
    pub(crate) runs: DashMap<String, BackupRun>,
    /// Job ids currently executing, to reject overlapping runs.
    pub(crate) running: DashMap<String, ()>,
    pub(crate) hooks: RwLock<Option<Arc<HookBus>>>,
    pub(crate) storage: RwLock<Option<Arc<StorageRegistry>>>,
    pub(crate) settings: RwLock<BackupSettings>,
    pub(crate) db: RwLock<Option<DbSnapshot>>,
    /// Redacted full configuration snapshot taken at startup, so
    /// hook-driven runs can snapshot configuration without a request
    /// context. Refreshed only on restart; HTTP handlers always use
    /// the live configuration instead.
    pub(crate) config_snapshot: RwLock<Value>,
}

impl BackupState {
    pub(crate) fn new() -> Self {
        Self {
            jobs: DashMap::new(),
            runs: DashMap::new(),
            running: DashMap::new(),
            hooks: RwLock::new(None),
            storage: RwLock::new(None),
            settings: RwLock::new(BackupSettings::default()),
            db: RwLock::new(None),
            config_snapshot: RwLock::new(Value::Null),
        }
    }

    fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("backup hooks lock poisoned").clone()
    }

    fn settings_snapshot(&self) -> BackupSettings {
        self.settings
            .read()
            .expect("backup settings lock poisoned")
            .clone()
    }

    fn db_snapshot(&self) -> Option<DbSnapshot> {
        self.db.read().expect("backup db lock poisoned").clone()
    }

    fn storage(&self) -> Option<Arc<StorageRegistry>> {
        self.storage
            .read()
            .expect("backup storage lock poisoned")
            .clone()
    }

    fn config_snapshot(&self) -> Value {
        self.config_snapshot
            .read()
            .expect("backup config lock poisoned")
            .clone()
    }
}

/// Everything one backup execution needs, gathered either from the live
/// application context (HTTP handlers) or from the module state snapshot
/// (hook-driven runs, which have no request context).
#[derive(Clone)]
pub(crate) struct BackupExecutor {
    pub(crate) state: Arc<BackupState>,
    pub(crate) storage: Arc<StorageRegistry>,
    pub(crate) settings: BackupSettings,
    pub(crate) db: DbSnapshot,
    pub(crate) hooks: Arc<HookBus>,
    /// Redacted full configuration document, used to build config
    /// snapshots for archives.
    pub(crate) config_snapshot: Value,
}

impl BackupExecutor {
    pub(crate) fn from_context(ctx: &AppContext, state: Arc<BackupState>) -> UnifiedResult<Self> {
        let config = ctx.config.read().expect("config lock poisoned");
        Ok(Self {
            state,
            storage: ctx.storage_registry.clone(),
            settings: backup_settings(&config),
            db: DbSnapshot::from_config(&config),
            hooks: ctx.hooks.clone(),
            config_snapshot: redact_value(serde_json::to_value(&*config).unwrap_or(Value::Null)),
        })
    }

    pub(crate) fn from_state(state: Arc<BackupState>) -> UnifiedResult<Self> {
        let storage = state
            .storage()
            .ok_or_else(|| UnifiedError::internal("Backup storage registry is not initialized"))?;
        let db = state
            .db_snapshot()
            .ok_or_else(|| UnifiedError::internal("Backup database settings are not initialized"))?;
        let hooks = state
            .hooks()
            .ok_or_else(|| UnifiedError::internal("Backup hook bus is not initialized"))?;
        Ok(Self {
            state: state.clone(),
            storage,
            settings: state.settings_snapshot(),
            db,
            hooks,
            config_snapshot: state.config_snapshot(),
        })
    }

    pub(crate) fn provider_for(&self, target: &str) -> UnifiedResult<Arc<dyn unified_core::storage::StorageProvider>> {
        self.storage.get(target).ok_or_else(|| {
            UnifiedError::not_found(format!("Storage target \"{target}\" is not registered"))
        })
    }
}

// Helpers: ids, hashing, cron validation.

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
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

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_digest() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn paginate_returns_the_requested_page() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
    }

    #[test]
    fn matches_search_is_case_insensitive() {
        let params = PaginationParams {
            search: Some("world".to_string()),
            ..Default::default()
        };
        assert!(matches_search("Hello World", &params));
        let params = PaginationParams {
            search: Some("missing".to_string()),
            ..Default::default()
        };
        assert!(!matches_search("Hello World", &params));
        assert!(matches_search("Hello World", &PaginationParams::default()));
    }
}
