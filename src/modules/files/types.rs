//! files module - File and folder record types plus the in-memory state.

use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, RwLock};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::middleware::TenantContext as ResolvedTenantContext;
use unified_core::tenancy::CallerScope;

pub(crate) const MAX_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

/// Bytes of an upload sniffed for MIME detection. Only the leading
/// bytes are kept in memory; the rest streams straight to disk.
pub(crate) const MIME_SNIFF_LEN: usize = 8 * 1024;

/// Maximum folder name length, matching the apikeys name limit.
pub(crate) const MAX_FOLDER_NAME_LEN: usize = 120;

/// Maximum tenant/company id length, matching the bi module.
pub(crate) const MAX_COMPANY_LEN: usize = 128;

/// Capability required to read files: list, get, download.
pub(crate) const PERM_FILES_VIEW: &str = "files.view";

/// Capability required to change files: create folders, upload,
/// delete, move.
pub(crate) const PERM_FILES_MANAGE: &str = "files.manage";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: String,
    pub name: String,
    /// `file` or `folder`.
    pub kind: String,
    pub size: u64,
    pub mime: String,
    /// Parent folder id; empty means the root.
    pub folder_id: String,
    /// Relative path on disk, or the shared URL for shared files.
    /// For locally stored records this is always
    /// `disk_path(folder_id, id)`; hook-registered shared files carry
    /// the shared URL instead.
    pub path: String,
    /// Tenant that owns the record, resolved server-side from the
    /// caller (see `resolve_caller_company`). Empty means global:
    /// a shared row every company may read (see
    /// `CallerScope::can_access_record`).
    #[serde(default)]
    pub company_id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateFolder {
    pub name: String,
    pub folder_id: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MoveRequest {
    pub folder_id: String,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct FilesState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) records: DashMap<String, FileRecord>,
    pub(crate) base_dir: PathBuf,
}

impl FilesState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            records: DashMap::new(),
            base_dir: PathBuf::from("data/files"),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// True when `folder_id` is the root or an existing folder record.
    /// Unscoped: hook handlers have no caller, so they use this; HTTP
    /// handlers must use `folder_accessible` instead.
    pub(crate) fn folder_exists(&self, folder_id: &str) -> bool {
        folder_id.is_empty()
            || self
                .records
                .get(folder_id)
                .is_some_and(|entry| entry.value().kind == "folder")
    }

    /// True when `folder_id` is the root or an existing folder record
    /// the caller may access (tenant-scoped; see
    /// `CallerScope::can_access_record`).
    pub(crate) fn folder_accessible(&self, folder_id: &str, scope: &CallerScope) -> bool {
        folder_id.is_empty()
            || self.records.get(folder_id).is_some_and(|entry| {
                let record = entry.value();
                record.kind == "folder" && scope.can_access_record(&record.company_id)
            })
    }

    /// Disk path for a file record: `data/files/<folder>/<record-id>`.
    /// Both segments are validated as single path components and the
    /// resolved path is verified to stay inside `base_dir` — the
    /// same containment rule as the scripting storage SDK
    /// (`resolve_module_path`): an absolute path or a `..` escape
    /// in either segment is refused instead of silently joining it
    /// (inspection C13). Record ids are server-generated UUIDs and
    /// a folder id must already exist in the store, so a rejection
    /// here means a corrupted record or a forged request, never a
    /// legitimate file.
    pub(crate) fn disk_path(&self, folder_id: &str, id: &str) -> UnifiedResult<PathBuf> {
        if id.is_empty() {
            return Err(UnifiedError::bad_request("file id must not be empty"));
        }
        Self::check_path_segment(folder_id, "folder id")?;
        Self::check_path_segment(id, "file id")?;
        let joined = if folder_id.is_empty() {
            self.base_dir.join(id)
        } else {
            self.base_dir.join(folder_id).join(id)
        };
        // Containment after canonicalization: a symlink inside the
        // directory pointing outside is still an escape. The parent
        // is checked for paths that do not exist yet (write
        // targets); an existing path is canonicalized itself, so a
        // symlink *file* pointing outside is caught too.
        let base = self
            .base_dir
            .canonicalize()
            .unwrap_or_else(|_| self.base_dir.clone());
        if joined.exists() {
            let canonical = joined.canonicalize().unwrap_or_else(|_| joined.clone());
            if !canonical.starts_with(&base) {
                return Err(UnifiedError::bad_request(
                    "file path must stay inside the files directory",
                ));
            }
            return Ok(joined);
        }
        let parent = joined.parent().unwrap_or(&joined).to_path_buf();
        let canonical_parent = parent.canonicalize().unwrap_or(parent);
        if !canonical_parent.starts_with(&base) {
            return Err(UnifiedError::bad_request(
                "file path must stay inside the files directory",
            ));
        }
        Ok(joined)
    }

    /// One segment of a disk path: exactly one normal component.
    /// Empty is allowed (the folder id is empty for the root);
    /// `.`, `..`, absolute paths, and embedded separators are
    /// refused.
    fn check_path_segment(segment: &str, what: &str) -> UnifiedResult<()> {
        if segment.is_empty() {
            return Ok(());
        }
        let path = Path::new(segment);
        if path.is_absolute() {
            return Err(UnifiedError::bad_request(format!(
                "{what} must be a relative path segment"
            )));
        }
        let mut components = path.components();
        match components.next() {
            Some(Component::Normal(_)) if components.next().is_none() => Ok(()),
            _ => Err(UnifiedError::bad_request(format!(
                "{what} must be a single path segment"
            ))),
        }
    }

    /// All descendant file records of a folder (recursive, by folder id).
    pub(crate) fn descendant_files(&self, folder_id: &str) -> Vec<FileRecord> {
        let mut out = Vec::new();
        let mut pending: Vec<String> = vec![folder_id.to_string()];
        while let Some(current) = pending.pop() {
            for entry in self.records.iter() {
                let record = entry.value();
                if record.folder_id == current {
                    if record.kind == "folder" {
                        pending.push(record.id.clone());
                    } else {
                        out.push(record.clone());
                    }
                }
            }
        }
        out
    }

    /// All descendant folder records of a folder (recursive, by folder
    /// id). Used by move to remap child folders and to refuse moving a
    /// folder into one of its own descendants.
    pub(crate) fn descendant_folders(&self, folder_id: &str) -> Vec<FileRecord> {
        let mut out = Vec::new();
        let mut pending: Vec<String> = vec![folder_id.to_string()];
        while let Some(current) = pending.pop() {
            for entry in self.records.iter() {
                let record = entry.value();
                if record.folder_id == current && record.kind == "folder" {
                    pending.push(record.id.clone());
                    out.push(record.clone());
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Caller authentication and tenant scoping
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the bi and calculator modules.
/// The trusted edge sets these headers; handlers never trust an
/// identity taken from a body or query parameter.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the bi and calculator modules).
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// The caller's company scope, resolved server-side — never taken
/// from a request body or a query filter. Precedence, most
/// authoritative first (see `CallerScope` in
/// `unified_core::tenancy`):
///
/// 1. the verified caller's company (`caller_from(req).company_id`);
/// 2. the tenant-context company the middleware resolved for this
///    request;
/// 3. the `user.company_requested` hook answered by the users module.
///
/// The first source that resolves wins. Platform is an explicit,
/// positively-decided scope, never a fallback: it requires no
/// resolvable company AND a control-plane request AND a verified
/// admin role (`CallerScope::decide`). Any other caller with no
/// company is `Unresolved` and reads global rows only — the old
/// no-company-means-platform behaviour was the HIGH-3 hole and is
/// gone.
pub(crate) fn resolve_caller_company(
    ctx: &AppContext,
    req: &HttpRequest,
    caller: &Caller,
) -> CallerScope {
    let is_admin = caller.role == "admin" || caller.role == "super_admin";
    // The verified caller's company wins; the tenant context and
    // the hook are only fallbacks for callers whose company the
    // middleware did not resolve. A header value is never trusted
    // here, and neither is a body/query `company_id`.
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty() && value.len() <= MAX_COMPANY_LEN);
    if let Some(company) = verified {
        return CallerScope::Company(company);
    }
    let tenant = ResolvedTenantContext::from_request(req)
        .map(|resolved| resolved.company_id)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value.len() <= MAX_COMPANY_LEN);
    if let Some(company) = tenant {
        return CallerScope::Company(company);
    }
    let hook = ctx
        .hooks
        .emit("user.company_requested", json!({ "user_id": caller.actor }));
    let hooked = hook
        .get("company_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= MAX_COMPANY_LEN)
        .map(str::to_string);
    CallerScope::decide(hooked, req, is_admin)
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use actix_web::test::TestRequest;
    use unified_core::config::AppConfig;
    use unified_core::modules::ModuleRegistry;

    fn record(id: &str, kind: &str, folder_id: &str, company_id: &str) -> FileRecord {
        FileRecord {
            id: id.to_string(),
            name: format!("{id}.{kind}"),
            kind: kind.to_string(),
            size: 0,
            mime: String::new(),
            folder_id: folder_id.to_string(),
            path: String::new(),
            company_id: company_id.to_string(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn state_with(records: Vec<FileRecord>) -> FilesState {
        let state = FilesState::new();
        for item in records {
            state.records.insert(item.id.clone(), item);
        }
        state
    }

    #[test]
    fn disk_path_uses_folder_id() {
        let state = FilesState::new();
        assert_eq!(
            state.disk_path("", "abc").expect("valid test ids"),
            PathBuf::from("data/files/abc")
        );
        assert_eq!(
            state.disk_path("folder-1", "abc").expect("valid test ids"),
            PathBuf::from("data/files/folder-1/abc")
        );
    }

    #[test]
    fn disk_path_rejects_escapes() {
        let state = FilesState::new();
        // Parent traversal in either segment.
        assert!(state.disk_path("..", "abc").is_err());
        assert!(state.disk_path("folder-1", "../abc").is_err());
        assert!(state.disk_path("..", "..").is_err());
        // Absolute paths.
        assert!(state.disk_path("/etc", "abc").is_err());
        assert!(state.disk_path("folder-1", "/etc/passwd").is_err());
        // Embedded separators smuggle a second level in one segment.
        assert!(state.disk_path("a/b", "abc").is_err());
        assert!(state.disk_path("folder-1", "a/b").is_err());
        // Dot segments and an empty file id.
        assert!(state.disk_path(".", "abc").is_err());
        assert!(state.disk_path("folder-1", "").is_err());
    }

    #[test]
    fn descendant_files_and_folders_are_recursive() {
        let state = state_with(vec![
            record("a", "folder", "", ""),
            record("b", "folder", "a", ""),
            record("f1", "file", "a", ""),
            record("f2", "file", "b", ""),
            record("other", "file", "", ""),
        ]);
        let mut files = state
            .descendant_files("a")
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>();
        files.sort();
        assert_eq!(files, vec!["f1".to_string(), "f2".to_string()]);
        let folders = state.descendant_folders("a");
        assert_eq!(folders.len(), 1);
        assert_eq!(folders[0].id, "b");
        assert!(state.descendant_files("other").is_empty());
    }

    #[test]
    fn folder_accessible_is_tenant_scoped() {
        let state = state_with(vec![
            record("own", "folder", "", "company-a"),
            record("theirs", "folder", "", "company-b"),
            record("plain", "file", "", "company-a"),
        ]);
        let company_a = CallerScope::Company("company-a".to_string());
        assert!(state.folder_accessible("", &company_a));
        assert!(state.folder_accessible("own", &company_a));
        assert!(!state.folder_accessible("theirs", &company_a));
        // Platform (explicitly decided) may access any folder.
        assert!(state.folder_accessible("theirs", &CallerScope::Platform));
        // Unresolved reads global rows only: another company's
        // folder is denied (the old `None`-means-platform hole).
        assert!(!state.folder_accessible("theirs", &CallerScope::Unresolved));
        assert!(!state.folder_accessible("plain", &company_a));
        assert!(!state.folder_accessible("missing", &company_a));
    }

    #[test]
    fn caller_scope_scopes_by_company() {
        // Platform (explicitly decided) reads any record.
        assert!(CallerScope::Platform.can_access_record("company-a"));
        assert!(CallerScope::Platform.can_access_record(""));
        let company_a = CallerScope::Company("company-a".to_string());
        assert!(company_a.can_access_record("company-a"));
        assert!(company_a.can_access_record(""));
        assert!(!company_a.can_access_record("company-b"));
        // Unresolved reads global rows only.
        assert!(CallerScope::Unresolved.can_access_record(""));
        assert!(!CallerScope::Unresolved.can_access_record("company-a"));
    }

    /// Platform is a positive decision, not a fallback: it needs a
    /// control-plane request (no resolved tenant context) AND a
    /// verified admin role. A tenant-host request or a non-admin
    /// caller with no company is Unresolved — and denied
    /// company-owned records.
    #[test]
    fn resolve_caller_company_platform_requires_control_plane_admin() {
        use actix_web::HttpMessage;
        use unified_core::hooks::HookContext;
        use unified_core::middleware::{TenantContext as MwTenantContext, TenantKind};

        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        let admin = Caller {
            actor: "tester".to_string(),
            role: "admin".to_string(),
        };
        let viewer = Caller {
            actor: "tester".to_string(),
            role: "viewer".to_string(),
        };
        // Admin, control-plane request, no company anywhere:
        // Platform.
        let req = verified_request("tester", "admin");
        assert_eq!(
            resolve_caller_company(&ctx, &req, &admin),
            CallerScope::Platform
        );
        // Same admin, but the request resolved to a tenant host:
        // Unresolved, denied company-owned records.
        let tenant_req = verified_request("tester", "admin");
        tenant_req.extensions_mut().insert(MwTenantContext {
            tenant_id: uuid::Uuid::new_v4().to_string(),
            company_id: String::new(),
            site_id: String::new(),
            kind: TenantKind::Subdomain,
            host: "tenant.example.test".to_string(),
            scope: None,
            product_id: None,
            package_id: None,
            product_subdomain_id: None,
        });
        let scope = resolve_caller_company(&ctx, &tenant_req, &admin);
        assert_eq!(scope, CallerScope::Unresolved);
        assert!(!scope.can_access_record("company-a"));
        // Non-admin on a control-plane request: Unresolved too.
        let req = verified_request("tester", "viewer");
        assert_eq!(
            resolve_caller_company(&ctx, &req, &viewer),
            CallerScope::Unresolved
        );
        // The hook fallback still resolves a company when the
        // verified caller and tenant context carry none.
        let hooks = Arc::new(HookBus::new());
        hooks.register(
            "user.company_requested",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                ctx.set("company_id", serde_json::json!("company-a"));
            }),
        );
        let hooked_ctx =
            AppContext::new(AppConfig::default(), hooks, Arc::new(ModuleRegistry::new()));
        let req = verified_request("tester", "viewer");
        assert_eq!(
            resolve_caller_company(&hooked_ctx, &req, &viewer),
            CallerScope::Company("company-a".to_string())
        );
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        let anonymous = TestRequest::default().to_http_request();
        assert!(caller(&anonymous).is_err());
        let no_role = TestRequest::default().to_http_request();
        assert!(caller(&no_role).is_err());
        let authed = verified_request("tester", "admin");
        let found = caller(&authed).unwrap();
        assert_eq!(found.actor, test_user_uuid("tester").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn check_capability_fails_closed_without_a_role_hook() {
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        assert!(!check_capability(&ctx, "admin", PERM_FILES_VIEW));
        assert!(require_permission(&ctx, "admin", PERM_FILES_MANAGE).is_err());
    }
}
