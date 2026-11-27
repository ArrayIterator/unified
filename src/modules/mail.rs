// Mail Manager module.
//
// Core mail support for the whole application: SMTP settings, sending,
// a send log, and three delivery connectors (SMTP, local sendmail, and a
// log-only debug connector). Other modules never send mail directly;
// they emit the `mail.send_requested` hook event and this module takes
// responsibility for delivery.
//
// Routes (all under `/api/v1/mail`):
//   GET    /settings              - public SMTP settings (password never returned)
//   PUT    /settings              - store SMTP settings (password is write-only)
//   POST   /send                  - send one message (optionally rendered from a template)
//   POST   /test                  - send a test message, report the real SMTP result
//   GET    /log                   - paginated send log (filter by status, search)
//   GET    /connectors            - available delivery connectors
//
// Hook contracts:
//   Listens to `mail.send_requested` (from Billing, Tickets, Auth,
//   Notifications, Email Templates). The message is queued in memory and
//   the `scheduler.enqueue` event is emitted so the Scheduler module can
//   own execution of the job. When no hook bus is available (for example
//   the module was registered without `init`), the message is delivered
//   directly in a worker thread instead and the outcome is logged.
//   The Scheduler (or any other module) can trigger delivery of a queued
//   job by emitting `mail.deliver` with `{ "job_id": ... }`.
//   Emits `mail.sent` / `mail.failed` after every delivery attempt.
//   When a `template_id` is supplied, the template is rendered through
//   the `template.render_requested` event owned by the Email Templates
//   module; when no handler answers, sending fails with 503.

use actix_web::HttpRequest;
use unified_core::error::{UnifiedError, UnifiedResult};

pub mod ai;
pub mod ai_capabilities;
mod handlers;
pub mod module;
mod service;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::*;
pub use self::state::*;
pub use self::types::{
    CONNECTOR_LOCAL, CONNECTOR_LOG, CONNECTOR_SMTP, EVENT_MAIL_DELIVER, EVENT_MAIL_FAILED,
    EVENT_MAIL_SEND_REQUESTED, EVENT_MAIL_SENT, EVENT_SCHEDULER_ENQUEUE, EVENT_TEMPLATE_RENDER,
    MailLogEntry, SmtpSettings,
};

// Tenant edge guard (C3 pilot 4, group B).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only: no per-record company check is applied.
/// Real partitioning needs a `tenant_id` on the records, a C11
/// follow-up.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// -- a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Resolve the caller's company for a WRITE (C3, strict by
/// default): a write requires a resolved company scope, so an
/// unresolved caller is refused with 403 instead of silently
/// writing a global (unscoped) record. Mutation handlers call
/// this instead of `caller_company`; read handlers keep
/// `caller_company`.
pub(crate) fn require_company_for_write(
    tenant: &unified_core::tenancy::TenantContext,
    req: &actix_web::HttpRequest,
) -> UnifiedResult<Option<String>> {
    match caller_company(tenant, req)? {
        Some(company) => Ok(Some(company)),
        None => Err(UnifiedError::forbidden(
            "A write requires a resolved company scope",
        )),
    }
}
