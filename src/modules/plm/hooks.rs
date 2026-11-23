//! Hook handlers for user deletion, product deletion, and
//! manufacturing BOM version creation.

use super::state::PlmState;
use super::types::DELETED_USER_ID;
use chrono::Utc;
use serde_json::json;
use unified_core::hooks::HookContext;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// User deletion (users module owns the event)
// ---------------------------------------------------------------------------

/// `user.deleted` carries `user_id` (falling back to `id`, the
/// same shape the forums and rental modules read). Actor fields
/// on versions, ECOs, approvals, BOM versions, and audit
/// entries are anonymized to `deleted-user` so the lifecycle
/// history stays auditable without the identity.
pub(crate) fn on_user_deleted(state: PlmState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(user_id) = user_id else {
        ctx.add_result(json!({ "plm": "skipped", "reason": "user.deleted carries no user_id" }));
        return;
    };
    let now = Utc::now();
    let mut anonymized_versions = 0i64;
    for mut entry in state.versions.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_versions += 1;
        }
    }
    let mut anonymized_ecos = 0i64;
    for mut entry in state.ecos.iter_mut() {
        if entry.value().requested_by == user_id {
            entry.value_mut().requested_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_ecos += 1;
        }
    }
    let mut anonymized_approvals = 0i64;
    for mut entry in state.approvals.iter_mut() {
        if entry.value().approver_id.as_deref() == Some(user_id.as_str()) {
            entry.value_mut().approver_id = Some(DELETED_USER_ID.to_string());
            entry.value_mut().updated_at = now;
            anonymized_approvals += 1;
        }
    }
    let mut anonymized_bom_versions = 0i64;
    for mut entry in state.bom_versions.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            anonymized_bom_versions += 1;
        }
    }
    let mut anonymized_audit = 0i64;
    for mut entry in state.audit.iter_mut() {
        if entry.value().actor_id == user_id {
            entry.value_mut().actor_id = DELETED_USER_ID.to_string();
            anonymized_audit += 1;
        }
    }
    ctx.add_result(json!({
        "plm": "user_deleted",
        "user_id": user_id,
        "anonymized_versions": anonymized_versions,
        "anonymized_ecos": anonymized_ecos,
        "anonymized_approvals": anonymized_approvals,
        "anonymized_bom_versions": anonymized_bom_versions,
        "anonymized_audit": anonymized_audit,
    }));
}

// ---------------------------------------------------------------------------
// Product deletion (products module owns the event)
// ---------------------------------------------------------------------------

/// `product.deleted` carries the full deleted product record,
/// so the product id is read from `id` (falling back to
/// `product_id`). Versions of that product are marked
/// `product_deleted` instead of being removed: the lifecycle
/// history of a deleted product stays visible and auditable,
/// and each marking writes an audit entry.
pub(crate) fn on_product_deleted(state: PlmState, ctx: &mut HookContext) {
    let product_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("product_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(product_id) = product_id else {
        ctx.add_result(
            json!({ "plm": "skipped", "reason": "product.deleted carries no product id" }),
        );
        return;
    };
    let now = Utc::now();
    let mut marked = 0i64;
    let mut version_ids: Vec<(Uuid, String, String)> = Vec::new();
    for mut entry in state.versions.iter_mut() {
        let version = entry.value_mut();
        if version.product_id == product_id && !version.product_deleted {
            version.product_deleted = true;
            version.updated_at = now;
            version_ids.push((version.id, version.tenant_id.clone(), version.revision()));
            marked += 1;
        }
    }
    for (version_id, tenant_id, revision) in version_ids {
        state.record_audit(
            &tenant_id,
            "version",
            version_id,
            "product_deleted",
            "system",
            Some(format!(
                "product {product_id} deleted; revision {revision} retained"
            )),
            None,
            Some("product_deleted".to_string()),
        );
    }
    ctx.add_result(json!({
        "plm": "product_deleted",
        "product_id": product_id,
        "versions_marked": marked,
    }));
}

// ---------------------------------------------------------------------------
// Manufacturing BOM version (manufacturing module owns the event)
// ---------------------------------------------------------------------------

/// `manufacturing.bom.version.created` carries `bom_id`,
/// `version_id`, and `version` (the manufacturing version
/// number). When a PLM BOM snapshot exists for the same BOM
/// and version number, an audit entry links the two, so the
/// manufacturing change is traceable from the PLM side.
/// Payloads with no matching PLM snapshot are skipped, not
/// failed: most manufacturing versions have no PLM record.
pub(crate) fn on_bom_version_created(state: PlmState, ctx: &mut HookContext) {
    let bom_id = ctx
        .get("bom_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let version_number = ctx.get("version").and_then(|value| value.as_i64());
    let Some((bom_id, version_number)) = bom_id.zip(version_number) else {
        ctx.add_result(json!({ "plm": "skipped", "reason": "manufacturing.bom.version.created carries no bom_id or version" }));
        return;
    };
    let matched: Vec<(Uuid, String)> = state
        .bom_versions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|version| version.bom_id == bom_id && version.version_number == version_number)
        .map(|version| (version.id, version.tenant_id.clone()))
        .collect();
    for (bom_version_id, tenant_id) in &matched {
        state.record_audit(
            tenant_id,
            "bom_version",
            *bom_version_id,
            "manufacturing_version_created",
            "system",
            Some(format!(
                "manufacturing BOM version {version_number} created for BOM {bom_id}"
            )),
            None,
            Some(version_number.to_string()),
        );
    }
    ctx.add_result(json!({
        "plm": "bom_version_created",
        "bom_id": bom_id,
        "version": version_number,
        "linked_plm_snapshots": matched.len(),
    }));
}
