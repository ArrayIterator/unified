// Document Vault module — secure document storage per company and
// client: folders, documents (metadata only — file bytes live in
// the storage module, referenced by target + key), versioning,
// append-only access logs, expiring share links, access requests
// with approvals, and retention rules.
//
// Routes live under `/api/v1/vault`:
//
//   Folders:
//     GET/POST /folders — list folders / create a folder.
//     GET/PATCH/DELETE /folders/{id}.
//     POST /folders/{id}/retention — set the retention rule.
//     GET  /folders/{id}/documents — documents in a folder.
//   Documents:
//     GET/POST /documents — list visible documents / upload metadata.
//     GET/PATCH/DELETE /documents/{id}.
//     POST /documents/{id}/versions — upload a new version (metadata).
//     GET  /documents/{id}/versions — version list.
//     POST /documents/{id}/versions/{version_id}/restore.
//     POST /documents/{id}/download — record a download, return the
//       storage reference (no file bytes are served here).
//     GET  /documents/{id}/access-log — append-only access log.
//     POST /documents/{id}/share — create an expiring share link.
//     GET  /documents/{id}/share-links — links of a document.
//     POST /documents/{id}/request-access — request access.
//     POST /documents/{id}/flag-review — flag for retention review.
//   Share links:
//     POST /share-links/{id}/revoke — revoke a link.
//     GET  /shared/{token} — resolve a share link (public route;
//       see the public-access note below).
//   Access requests:
//     GET  /access-requests — pending / decided requests.
//     POST /access-requests/{id}/approve.
//     POST /access-requests/{id}/deny.
//   Retention:
//     GET  /retention/review — documents past their retention
//       window, flagged for review (never auto-deleted).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory folder, document, version, access-log, share-link, and
// access-request management; versioning where every upload is a new
// version and old versions are kept and restorable; an append-only
// access log written on every view / download / share action;
// unguessable share tokens with expiry, a download cap, and
// revocation; access requests with approve / deny decisions that
// are themselves audited; retention rules that only flag documents
// for review — deletion is always an explicit, audited manage
// action. What is NOT built here: file bytes (the storage module
// owns upload / download of bytes — this module only stores the
// storage target + key, the SHA-256 checksum, and the size, and
// verifies the checksum format on write); actual file encryption
// at rest (the storage target is responsible for it); virus
// scanning; and automatic deletion (by design — see the retention
// note in the module header).
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`). There is no reusable per-request "is this
// endpoint public?" helper to call from a handler — the check lives
// in the API Manager middleware / `resolve_public` path — so the
// public route below relies on that gate: an administrator must
// create the matching `ApiAccessRule` row for
// `GET /api/v1/vault/shared/{token}`. The route is registered in
// `route_info()` with `route_version_support()` defaulting to
// `All`, so the API Manager's per-version registry picks it up. Do
// NOT treat the absence of a capability check on the public route
// as an open door: without the rule the API Manager denies the
// call. The share token itself is unguessable (two UUIDv7 values),
// carries an expiry and a download cap, and every resolution is
// written to the access log.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Visibility rule (enforced server-side on every read): a caller
// sees a document when they hold `vault.manage`, when they own the
// document, when their user id is in the document's
// `shared_with_client_ids`, or when an approved access request
// covers them for that document. Every other caller gets a 404,
// the same as for an unknown document, so existence never leaks.
//
// Events emitted on the hook bus:
//   - `vault.document.uploaded`, `vault.document.versioned`,
//     `vault.document.restored`, `vault.document.shared`,
//     `vault.document.deleted`, `vault.document.flagged`
//   - `vault.access.requested`, `vault.access.granted`,
//     `vault.access.denied`
//   - `vault.link.created`, `vault.link.revoked`
//   - `vault.folder.created`, `vault.folder.deleted`
//   - `notification.requested` (access granted / denied) with the
//     notifications module's payload shape: `{"kind", "user_id",
//     "title", "body", "module": "vault", "link", "priority",
//     "channels"}`.

mod access;
pub mod ai_capabilities;
pub mod constants;
mod handlers_documents;
mod handlers_folders;
mod handlers_sharing;
mod helpers;
pub mod module;
pub mod state;
pub mod types;
mod validation;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::{ACTION_APPROVE, ACTION_DELETE, ACTION_DENY, ACTION_RESTORE};
pub use self::constants::{ACTION_DOWNLOAD, ACTION_REQUEST, ACTION_SHARE, ACTION_VIEW};
pub use self::constants::{ACTION_FLAG_REVIEW, ACTION_LINK_REVOKE, ACTION_UPLOAD};
pub use self::module::{VaultModule, register};
pub use self::state::VaultState;
pub use self::types::{CreateDocumentRequest, SetRetentionRequest, UpdateFolderRequest};
pub use self::types::{CreateFolderRequest, VaultAccessRequest, VaultShareLink};
pub use self::types::{CreateShareLinkRequest, UpdateDocumentRequest, UploadVersionRequest};
pub use self::types::{DecideAccessRequest, RequestAccessRequest};
pub use self::types::{VaultAccessLog, VaultDocument, VaultDocumentVersion, VaultFolder};
