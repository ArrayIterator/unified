// documents — Hook event names emitted and listened to by this module.

/// Emitted when a new document is finalized. Payload is the
/// document record.
pub const DOCUMENT_UPLOADED: &str = "document.uploaded";
/// Emitted when a new version is created (re-upload or restore).
/// Payload is the version record.
pub const VERSION_CREATED: &str = "document.version.created";
/// Emitted on every download. Payload:
/// `{"document_id", "version", "user_id", "via",
/// "scan_status", "bytes"}`.
pub const DOCUMENT_DOWNLOADED: &str = "document.downloaded";
/// Emitted when a document is deleted. Payload:
/// `{"id", "name", "owner_id", "company_id"}`.
pub const DOCUMENT_DELETED: &str = "document.deleted";
/// Emitted when a scan finishes. Payload:
/// `{"document_id", "version", "verdict", "threat",
/// "scanner"}`.
pub const SCAN_COMPLETED: &str = "document.scan.completed";
/// Emitted when a quota check rejects an upload. Payload:
/// `{"scope", "scope_id", "module", "user_id", "company_id",
/// "bytes", "detail"}`.
pub const QUOTA_EXCEEDED: &str = "document.quota.exceeded";
/// Emitted when a lazy-finalize job verifies a document's
/// whole-file hash against the client-supplied expectation.
/// Payload: `{"document_id", "session_id", "checksum"}`.
pub const FINALIZE_VERIFIED: &str = "document.finalize.verified";
/// Emitted when a lazy-finalize job fails (hash mismatch or
/// unreadable object). The object is deleted/quarantined and the
/// session marked failed; the document row stays (checksum NULL)
/// so the failure is visible to the owner. Payload:
/// `{"document_id", "session_id", "error"}`.
pub const FINALIZE_FAILED: &str = "document.finalize.failed";
/// Emitted when a document's visibility flips. Payload:
/// `{"document_id", "visibility", "previous", "actor"}`.
pub const VISIBILITY_FLIPPED: &str = "document.visibility.flipped";
/// Emitted when a folder is created. Payload is the folder record.
pub const FOLDER_CREATED: &str = "document.folder.created";
/// Emitted when a folder is renamed. Payload:
/// `{"folder_id", "name", "previous", "actor"}`.
pub const FOLDER_UPDATED: &str = "document.folder.updated";
/// Emitted when a folder is deleted. Payload:
/// `{"id", "name", "owner_id", "company_id"}`.
pub const FOLDER_DELETED: &str = "document.folder.deleted";
/// Emitted when a folder moves to a new parent. Payload:
/// `{"folder_id", "parent_id", "previous_parent_id", "actor"}`.
pub const FOLDER_MOVED: &str = "document.folder.moved";
/// Emitted when a document moves to another folder. Payload:
/// `{"document_id", "folder_id", "previous_folder_id", "actor"}`.
pub const DOCUMENT_MOVED: &str = "document.moved";
/// Emitted when a document is renamed. Payload:
/// `{"document_id", "name", "previous", "actor"}`.
pub const DOCUMENT_RENAMED: &str = "document.renamed";
/// Emitted to the Search module (owned by the search module) when
/// a document is created or its metadata changes, so the instant
/// search index carries the current title and description.
/// Payload: `{"id", "kind": "documents.document", "title",
/// "snippet", "url", "updated_at"}`. The Search module validates
/// the kind against its own `SEARCH_KINDS` list; until
/// `documents.document` is listed there, it answers the event
/// with `{"indexed": false, ...}` and the document stays out of
/// instant search (see `emit_search_index`).
pub const SEARCH_INDEX: &str = "search.index";
