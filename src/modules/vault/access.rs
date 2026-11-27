//! vault module - Visibility rules, loaders and version bookkeeping.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use std::sync::atomic::AtomicI64;
use std::sync::atomic::Ordering;
use uuid::Uuid;

use super::constants::MAX_VERSIONS_PER_DOCUMENT;
use super::constants::PERM_VAULT_MANAGE;
use super::helpers::check_capability;
use super::helpers::parse_uuid;
use super::helpers::Caller;
use super::state::VaultState;
use super::types::VaultDocument;
use super::types::VaultDocumentVersion;
use super::types::VaultFolder;

/// True when `document` belongs to `tenant_id`. Records carry
/// the tenant they were created under (bound server-side from the
/// resolved tenant context); a record from another tenant - or a
/// record with no tenant at all - is never visible, so tenant
/// isolation fails closed.
pub(crate) fn in_tenant(document_tenant: &Option<String>, tenant_id: &str) -> bool {
    document_tenant.as_deref() == Some(tenant_id)
}

/// True when `actor` may see `document`: the document must belong
/// to the caller's tenant, and the caller must be the owner,
/// hold the manage capability, be a listed client, or hold an
/// approved access request for this document. The caller passes
/// `is_manager` (the result of the `vault.manage` capability
/// check) so the visibility test stays a pure function of the
/// stored rows plus that one flag.
pub(crate) fn can_see_document(
    state: &VaultState,
    document: &VaultDocument,
    actor: &str,
    is_manager: bool,
    tenant_id: &str,
) -> bool {
    if !in_tenant(&document.tenant_id, tenant_id) {
        return false;
    }
    if is_manager || document.owner_id == actor {
        return true;
    }
    if document.shared_with_client_ids.iter().any(|id| id == actor) {
        return true;
    }
    state.access_requests.iter().any(|entry| {
        let request = entry.value();
        request.document_id == document.id
            && request.requester_id == actor
            && request.status == "approved"
    })
}

/// True when `caller` may decide an access request or revoke a
/// share link for `document`: the document owner, or a
/// manage-capability holder scoped to the document's company -
/// the caller's resolved company id must equal the document's
/// company id (both unscoped counts as a match). Tenant scoping
/// is enforced separately by the loaders, so a caller from
/// another tenant never reaches this check with a foreign
/// document. A manage holder for a different company is not a
/// scoped admin for this document.
pub(crate) fn is_document_owner_or_scoped_admin(
    document: &VaultDocument,
    caller: &Caller,
    is_manager: bool,
) -> bool {
    if document.owner_id == caller.actor {
        return true;
    }
    is_manager && document.company_id == caller.company_id
}

/// Deny with 403 unless `caller` is the document owner or a
/// scoped admin for the document (see
/// `is_document_owner_or_scoped_admin`).
pub(crate) fn require_document_owner_or_scoped_admin(
    document: &VaultDocument,
    caller: &Caller,
    is_manager: bool,
) -> UnifiedResult<()> {
    if is_document_owner_or_scoped_admin(document, caller, is_manager) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Only the document owner or a scoped administrator may perform this action",
        ))
    }
}

pub(crate) fn load_folder(
    state: &VaultState,
    raw_id: &str,
    tenant_id: &str,
) -> UnifiedResult<VaultFolder> {
    let id = parse_uuid(raw_id, "folder")?;
    state
        .folders
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|folder| in_tenant(&folder.tenant_id, tenant_id))
        .ok_or_else(|| UnifiedError::not_found(format!("Folder not found: {id}")))
}

/// Load a document for `actor`, enforcing the visibility rule.
/// A document the caller may not see answers 404, the same as an
/// unknown document, so existence never leaks.
pub(crate) fn load_visible_document(
    ctx: &AppContext,
    state: &VaultState,
    raw_id: &str,
    caller: &Caller,
) -> UnifiedResult<VaultDocument> {
    let id = parse_uuid(raw_id, "document")?;
    let document = state
        .documents
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Document not found: {id}")))?;
    let is_manager = check_capability(ctx, &caller.role, PERM_VAULT_MANAGE);
    if can_see_document(
        state,
        &document,
        &caller.actor,
        is_manager,
        &caller.tenant_id,
    ) {
        Ok(document)
    } else {
        Err(UnifiedError::not_found(format!("Document not found: {id}")))
    }
}

/// Load a document for a manage-only action (update, delete,
/// version upload, share, retention flag). The caller must hold
/// `vault.manage` or own the document; anything else answers 404.
/// The document must also belong to the caller's tenant.
pub(crate) fn load_managed_document(
    ctx: &AppContext,
    state: &VaultState,
    raw_id: &str,
    caller: &Caller,
) -> UnifiedResult<VaultDocument> {
    let id = parse_uuid(raw_id, "document")?;
    let document = state
        .documents
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Document not found: {id}")))?;
    let is_manager = check_capability(ctx, &caller.role, PERM_VAULT_MANAGE);
    if !in_tenant(&document.tenant_id, &caller.tenant_id) {
        return Err(UnifiedError::not_found(format!("Document not found: {id}")));
    }
    if is_manager || document.owner_id == caller.actor {
        Ok(document)
    } else {
        Err(UnifiedError::not_found(format!("Document not found: {id}")))
    }
}

/// Unguessable share token: two UUIDv7 values rendered as hex with
/// no separators (64 hex characters, ~244 bits of entropy). The
/// token is never written to logs.
pub(crate) fn new_share_token() -> String {
    format!("{}{}", Uuid::now_v7().simple(), Uuid::now_v7().simple())
}

pub(crate) fn next_version_number(state: &VaultState, document_id: Uuid) -> i64 {
    state
        .versions
        .iter()
        .filter(|entry| entry.value().document_id == document_id)
        .map(|entry| entry.value().version_number)
        .max()
        .unwrap_or(0)
        + 1
}

/// Allocate the next version number for `document_id`. The number
/// is 1-based and strictly increasing per document. A plain
/// max-plus-one scan races: two concurrent uploads can read the
/// same maximum and both assign the same number. Instead each
/// document has an atomic counter, seeded once from the highest
/// stored version number; every allocation is one atomic
/// increment, so concurrent uploads always receive distinct
/// numbers. (The seed scan runs only when the counter is first
/// created, inside the map-entry insertion, and the counter is
/// the only writer afterwards.)
pub(crate) fn allocate_version_number(state: &VaultState, document_id: Uuid) -> i64 {
    let counter = state
        .version_counters
        .entry(document_id)
        .or_insert_with(|| AtomicI64::new(next_version_number(state, document_id) - 1));
    counter.fetch_add(1, Ordering::SeqCst) + 1
}

pub(crate) fn versions_of(state: &VaultState, document_id: Uuid) -> Vec<VaultDocumentVersion> {
    let mut items: Vec<VaultDocumentVersion> = state
        .versions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|version| version.document_id == document_id)
        .collect();
    items.sort_by_key(|version| version.version_number);
    items
}

/// Enforce the per-document version cap: drop the oldest
/// non-current versions first. The current version is never
/// dropped. Called after inserting a new version.
pub(crate) fn enforce_version_cap(state: &VaultState, document: &VaultDocument) {
    let mut versions = versions_of(state, document.id);
    if versions.len() <= MAX_VERSIONS_PER_DOCUMENT {
        return;
    }
    versions.retain(|version| Some(version.id) != document.current_version_id);
    versions.sort_by_key(|version| version.version_number);
    while versions.len() >= MAX_VERSIONS_PER_DOCUMENT {
        if let Some(oldest) = versions.first() {
            let id = oldest.id;
            state.versions.remove(&id);
            versions.remove(0);
        } else {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::allocate_version_number;
    use super::can_see_document;
    use super::is_document_owner_or_scoped_admin;
    use crate::modules::vault::helpers::Caller;
    use crate::modules::vault::VaultDocumentVersion;
    use crate::modules::vault::{VaultAccessRequest, VaultDocument, VaultState};
    use chrono::Utc;
    use std::collections::HashSet;
    use std::sync::Arc;
    use uuid::Uuid;

    const TENANT: &str = "tenant-1";
    const OTHER_TENANT: &str = "tenant-2";

    fn document(owner: &str, shared: Vec<String>) -> VaultDocument {
        VaultDocument {
            id: Uuid::new_v4(),
            folder_id: Uuid::new_v4(),
            name: "contract.pdf".to_string(),
            description: None,
            owner_id: owner.to_string(),
            tenant_id: Some(TENANT.to_string()),
            company_id: Some("company-1".to_string()),
            current_version_id: None,
            storage_target: "files".to_string(),
            storage_key: "folder/contract.pdf".to_string(),
            checksum_sha256: "a".repeat(64),
            size_bytes: 1024,
            mime_type: "application/pdf".to_string(),
            shared_with_client_ids: shared,
            status: "active".to_string(),
            flagged_for_review: false,
            flagged_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn caller(actor: &str, company: Option<&str>) -> Caller {
        Caller {
            actor: actor.to_string(),
            role: "manager".to_string(),
            tenant_id: TENANT.to_string(),
            schema_name: "tenant_1".to_string(),
            company_id: company.map(str::to_string),
        }
    }

    fn version(document_id: Uuid, number: i64) -> VaultDocumentVersion {
        VaultDocumentVersion {
            id: Uuid::new_v4(),
            document_id,
            version_number: number,
            storage_target: "files".to_string(),
            storage_key: "folder/contract.pdf".to_string(),
            checksum_sha256: "a".repeat(64),
            size_bytes: 1024,
            mime_type: "application/pdf".to_string(),
            uploaded_by: "owner-1".to_string(),
            note: None,
            created_at: Utc::now(),
        }
    }

    fn request(document_id: Uuid, requester: &str, status: &str) -> VaultAccessRequest {
        VaultAccessRequest {
            id: Uuid::new_v4(),
            document_id,
            requester_id: requester.to_string(),
            tenant_id: Some(TENANT.to_string()),
            reason: None,
            status: status.to_string(),
            decided_by: None,
            decided_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn owner_can_see() {
        let state = VaultState::default();
        assert!(can_see_document(
            &state,
            &document("owner-1", vec![]),
            "owner-1",
            false,
            TENANT
        ));
    }

    #[test]
    fn manager_can_see() {
        let state = VaultState::default();
        assert!(can_see_document(
            &state,
            &document("owner-1", vec![]),
            "someone-else",
            true,
            TENANT
        ));
    }

    #[test]
    fn listed_client_can_see() {
        let state = VaultState::default();
        let doc = document("owner-1", vec!["client-9".to_string()]);
        assert!(can_see_document(&state, &doc, "client-9", false, TENANT));
    }

    #[test]
    fn stranger_cannot_see() {
        let state = VaultState::default();
        assert!(!can_see_document(
            &state,
            &document("owner-1", vec![]),
            "stranger",
            false,
            TENANT
        ));
    }

    #[test]
    fn approved_request_can_see() {
        let state = VaultState::default();
        let doc = document("owner-1", vec![]);
        let req = request(doc.id, "requester-1", "approved");
        state.access_requests.insert(req.id, req);
        assert!(can_see_document(&state, &doc, "requester-1", false, TENANT));
    }

    #[test]
    fn pending_request_cannot_see() {
        let state = VaultState::default();
        let doc = document("owner-1", vec![]);
        let req = request(doc.id, "requester-1", "pending");
        state.access_requests.insert(req.id, req);
        assert!(!can_see_document(
            &state,
            &doc,
            "requester-1",
            false,
            TENANT
        ));
    }

    #[test]
    fn denied_request_cannot_see() {
        let state = VaultState::default();
        let doc = document("owner-1", vec![]);
        let req = request(doc.id, "requester-1", "denied");
        state.access_requests.insert(req.id, req);
        assert!(!can_see_document(
            &state,
            &doc,
            "requester-1",
            false,
            TENANT
        ));
    }

    #[test]
    fn other_tenant_cannot_see_even_as_manager_or_owner() {
        let state = VaultState::default();
        let doc = document("owner-1", vec![]);
        // Manager from another tenant: tenant gate runs first.
        assert!(!can_see_document(
            &state,
            &doc,
            "someone-else",
            true,
            OTHER_TENANT
        ));
        // Even the owner id, presented under another tenant, sees nothing.
        assert!(!can_see_document(
            &state,
            &doc,
            "owner-1",
            false,
            OTHER_TENANT
        ));
    }

    #[test]
    fn document_without_tenant_is_visible_to_no_tenant() {
        let state = VaultState::default();
        let mut doc = document("owner-1", vec![]);
        doc.tenant_id = None;
        assert!(!can_see_document(&state, &doc, "owner-1", false, TENANT));
        assert!(!can_see_document(
            &state,
            &doc,
            "someone-else",
            true,
            TENANT
        ));
    }

    #[test]
    fn allocate_version_numbers_are_sequential_from_stored_max() {
        let state = VaultState::default();
        let doc_id = Uuid::new_v4();
        let first = version(doc_id, 1);
        let third = version(doc_id, 3);
        state.versions.insert(first.id, first);
        state.versions.insert(third.id, third);
        assert_eq!(allocate_version_number(&state, doc_id), 4);
        assert_eq!(allocate_version_number(&state, doc_id), 5);
    }

    #[test]
    fn allocate_version_numbers_are_unique_under_concurrency() {
        let state = Arc::new(VaultState::default());
        let doc_id = Uuid::new_v4();
        let seed = version(doc_id, 2);
        state.versions.insert(seed.id, seed);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let state = Arc::clone(&state);
            handles.push(std::thread::spawn(move || {
                (0..25)
                    .map(|_| allocate_version_number(&state, doc_id))
                    .collect::<Vec<i64>>()
            }));
        }
        let mut numbers: Vec<i64> = handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("worker thread panicked"))
            .collect();
        numbers.sort_unstable();
        let unique: HashSet<i64> = numbers.iter().copied().collect();
        assert_eq!(numbers.len(), 200);
        assert_eq!(unique.len(), 200, "duplicate version numbers allocated");
        assert_eq!(*numbers.first().expect("numbers"), 3);
        assert_eq!(*numbers.last().expect("numbers"), 202);
    }

    #[test]
    fn owner_may_decide_regardless_of_capability() {
        let doc = document("owner-1", vec![]);
        assert!(is_document_owner_or_scoped_admin(
            &doc,
            &caller("owner-1", None),
            false
        ));
    }

    #[test]
    fn same_company_manager_is_a_scoped_admin() {
        let doc = document("owner-1", vec![]);
        assert!(is_document_owner_or_scoped_admin(
            &doc,
            &caller("manager-1", Some("company-1")),
            true
        ));
    }

    #[test]
    fn other_company_manager_is_not_a_scoped_admin() {
        let doc = document("owner-1", vec![]);
        assert!(!is_document_owner_or_scoped_admin(
            &doc,
            &caller("manager-1", Some("company-2")),
            true
        ));
    }

    #[test]
    fn unscoped_manager_is_not_a_scoped_admin_for_company_document() {
        let doc = document("owner-1", vec![]);
        assert!(!is_document_owner_or_scoped_admin(
            &doc,
            &caller("manager-1", None),
            true
        ));
    }

    #[test]
    fn non_manager_cannot_decide_for_others_documents() {
        let doc = document("owner-1", vec![]);
        assert!(!is_document_owner_or_scoped_admin(
            &doc,
            &caller("manager-1", Some("company-1")),
            false
        ));
    }
}
