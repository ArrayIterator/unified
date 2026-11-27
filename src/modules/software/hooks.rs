//! Hook handlers for version-control import events.

use unified_core::hooks::{HookContext};
use unified_core::json;
use serde_json::{json};
use std::sync::{Arc};
use super::types::{SoftwareVersion, VersionRequirements};
use super::state::SoftwareState;
use super::validation::{new_id, now, validate_version_string};

// Hook handlers: VCS import and validation.

/// Answer `vcs.software.validate` (from the VCS module) with
/// `{ "valid": bool }`, so a repository connection can only point at a
/// software record that really exists. A handler that answers
/// `valid: false` rejects the connection with 422.
pub(crate) fn on_vcs_software_validate(state: Arc<SoftwareState>, ctx: &mut HookContext) {
    let software_id = ctx
        .get("software_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let valid = !software_id.trim().is_empty() && state.software.contains_key(software_id);
    ctx.add_result(json!({ "valid": valid }));
}

/// Create a draft software version for a release imported from a VCS
/// provider (`vcs.release.imported`, payload is the imported version
/// draft). The version is always a draft: nothing here publishes it,
/// publishing stays a manual step through
/// `POST /api/v1/software/releases/{id}/publish` after a release is
/// created for the version. Importing the same tag twice is a no-op:
/// when a draft version already exists for the same software record
/// and version string it is reported and skipped, so a re-sync never
/// creates duplicates. A draft whose software record does not exist
/// is skipped rather than creating an orphan version.
pub(crate) fn on_vcs_release_imported(state: Arc<SoftwareState>, ctx: &mut HookContext) {
    let software_id = ctx
        .get("software_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    let version = ctx
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    let tag = ctx
        .get("tag")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    if software_id.trim().is_empty() || version.trim().is_empty() {
        ctx.add_result(json!({
            "created": false,
            "reason": "software_id and version are required",
        }));
        return;
    }
    if validate_version_string(&version).is_err() {
        ctx.add_result(json!({
            "created": false,
            "reason": format!("Version \"{version}\" is not a valid semantic version"),
            "tag": tag,
        }));
        return;
    }
    let Some(software) = state
        .software
        .get(software_id.as_str())
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "created": false,
            "reason": "software not found",
            "software_id": software_id,
            "tag": tag,
        }));
        return;
    };
    if let Some(existing) = state.versions.iter().find(|entry| {
        entry.value().software_id.as_deref() == Some(software_id.as_str())
            && entry.value().version == version
    }) {
        ctx.add_result(json!({
            "created": false,
            "duplicate": true,
            "version_id": existing.value().id,
            "software_id": software_id,
            "version": version,
            "tag": tag,
        }));
        return;
    }
    let changelog = ctx
        .get("changelog")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|changelog| !changelog.trim().is_empty())
        .or_else(|| {
            ctx.get("title")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    let draft = SoftwareVersion {
        id: new_id(),
        product: software.slug.clone(),
        version: version.clone(),
        changelog,
        file_id: None,
        license_required: false,
        software_id: Some(software_id.clone()),
        requirements: VersionRequirements::default(),
        created_at: now(),
        updated_at: now(),
    };
    state.versions.insert(draft.id.clone(), draft.clone());
    ctx.add_result(json!({
        "created": true,
        "version_id": draft.id,
        "software_id": software_id,
        "software_slug": software.slug,
        "version": version,
        "tag": tag,
        "status": "draft",
    }));
}
