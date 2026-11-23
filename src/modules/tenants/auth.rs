//! tenants - auth.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{HttpRequest};
use serde_json::{json};
use super::{constants::*, vocabulary::*, events::*, settings::*, records::*, state::*, helpers::*, provisioning::*, destroy::*, handlers::*, module::*, tenancy::*};



// ---------------------------------------------------------------------------
// Viewer identity and capability checks (same trusted-parameter
// pattern as the SaaS-packages module: X-User-Id plus X-Role
// headers, `role.check` hook, fail closed).
// ---------------------------------------------------------------------------
/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks).
pub struct Caller {
    pub actor: String,
    pub role: String,
}


pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}


/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx
        .hooks
        .emit("role.check", json!({ "role": role, "permission": permission, "allowed": false }));
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}


pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}
