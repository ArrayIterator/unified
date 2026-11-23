// Auditors & Public Link Sharing module — temporary sandbox
//
// Split into submodules; the full module documentation lives in `module.rs`.

pub mod ai_capabilities;
pub mod types;
pub mod state;
mod helpers;
mod hooks;
mod handlers_auditors;
mod handlers_links;
pub mod module;

pub(crate) use self::types::{MAX_LABEL_LEN, MAX_EMAIL_LEN, MAX_REASON_LEN, MAX_SCOPE_ENTRIES,
    MAX_MODULE_LEN, MAX_KIND_LEN, PERM_AUDITORS_VIEW, PERM_AUDITORS_CREATE, PERM_AUDITORS_MANAGE,
    PERM_AUDITORS_REVOKE, PERM_SHARING_VIEW, PERM_SHARING_CREATE, PERM_SHARING_REVOKE,
    EXPIRE_REQUESTED, SCOPE_CHECK, SCHEDULER_ENQUEUE};
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{AuditorStatus, AuditorAccount, AuditorAccountView, AuditorSession,
    AuditorActionLogEntry, PublicLinkAccessLogEntry, PublicLinkView, PublicLinkCreated,
    AuditorCreated, ActiveAuditorSessionView, AuditorScopeInput, CreateAuditorRequest,
    ExtendAuditorRequest, RevokeRequest, CreateLinkRequest, PublicLinkQuery};
pub use self::state::{AuditorsState, expire_due_auditors};
pub(crate) use self::helpers::{Caller, caller, check_capability, require_permission,
    reject_auditor_caller, parse_uuid, require_text, valid_email, ok, created, err, paginate,
    client_ip, user_agent, load_auditor, load_link, validate_scope_input, schedule_expiry,
    require_tenant, caller_company, in_company_scope, require_record_in_company_scope,
    account_in_scope, require_account_in_scope, require_scope_entry_company, link_in_scope,
    require_link_in_scope};
pub(crate) use self::hooks::{on_scope_check, on_expire_requested, on_auth_login};
pub(crate) use self::handlers_auditors::{create_auditor, list_auditors, get_auditor, revoke_auditor,
    extend_auditor, list_auditor_sessions, revoke_auditor_session, list_auditor_activity,
    list_active_auditor_sessions, get_my_scope, expire_sweep};
pub(crate) use self::handlers_links::{create_link, list_links, get_link, revoke_link,
    list_link_access_log, list_access_log, list_shareable_kinds, get_public_by_token};
pub use self::module::{AuditorsModule, register};
