//! Presets + Setup Wizard module. — Wizard handlers. Navigation is index-based over the step list stored on the wizard state: no handler matches on a business-type name. Saving a step merges whichever fields the payload carries (preset choice, optional-module toggles, settings answers) and then advances one stored step; the step the wizard is on only decides whether the preset application runs now (the stored "apply" step) or at completion.
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::apply::*;
use super::events::*;
use super::helpers::*;
use super::identity::*;
use super::lookups::*;
use super::preset_handlers::*;
use super::records::*;
use super::requests::*;
use super::seeds::*;
use super::stores::*;
use super::wizard_steps::*;

use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::json::{error_response, success_response};
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{Map, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn start_wizard(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    body: web::Json<StartWizardRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match require_caller_permission(&ctx, &req, "presets:apply") {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    let request = body.into_inner();
    // The wizard is keyed by company: a body company other than the
    // caller's scope is refused, and an unresolved caller may not
    // start a wizard for any company.
    let company_id = match require_company_match_uuid(&_tenant, &req, &Some(request.company_id)) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return error_response(
                &UnifiedError::forbidden(
                    "company_id does not match the caller's company scope",
                ),
                debug,
            )
        }
        Err(error) => return error_response(&error, debug),
    };
    // A wizard is resumable: starting again for the same tenant
    // returns the in-progress wizard instead of discarding it.
    if let Some(wizard) = state.wizards.get(&company_id) {
        if wizard.status == "in_progress" {
            return success_response(wizard.value().clone(), 200);
        }
    }
    let steps = match request.preset_id {
        Some(preset_id) => {
            match state.presets.get(&preset_id) {
                Some(preset) => {
                    if preset.archived {
                        return error_response(
                            &UnifiedError::conflict(format!(
                                "Preset \"{}\" is archived and cannot be used",
                                preset.slug
                            )),
                            debug,
                        );
                    }
                    if preset.steps.is_empty() {
                        DEFAULT_WIZARD_STEPS
                            .iter()
                            .map(|step| (*step).to_string())
                            .collect()
                    } else {
                        preset.steps.clone()
                    }
                }
                None => {
                    return error_response(
                        &UnifiedError::not_found(format!("Preset \"{preset_id}\" not found")),
                        debug,
                    );
                }
            }
        }
        None => DEFAULT_WIZARD_STEPS
            .iter()
            .map(|step| (*step).to_string())
            .collect(),
    };
    let actor_id = caller.actor.clone();
    let now = now();
    let wizard = WizardState {
        id: Uuid::now_v7(),
        company_id,
        actor_id: actor_id.clone(),
        preset_id: request.preset_id,
        steps: steps.clone(),
        current_step: steps.first().cloned().unwrap_or_else(|| "done".to_string()),
        status: "in_progress".to_string(),
        skipped_steps: Vec::new(),
        disabled_optional_modules: Vec::new(),
        settings: Value::Object(Map::new()),
        application_id: None,
        created_at: now,
        updated_at: now,
    };
    state.wizards.insert(wizard.company_id, wizard.clone());
    emit_audit(
        &ctx,
        WIZARD_STARTED,
        &actor_id,
        "presets.wizard.started",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({ "preset_id": wizard.preset_id }),
    );
    success_response(wizard, 201)
}

pub(super) async fn get_wizard(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    _query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    if let Err(error) = require_caller_permission(&ctx, &req, "presets:view") {
        return error_response(&error, debug);
    }
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    // The wizard is keyed by company: the path company must equal
    // the caller scope, and an unresolved caller reaches no wizard.
    let company_id = match require_company_match_uuid(&_tenant, &req, &Some(company_id)) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return error_response(
                &UnifiedError::forbidden(
                    "company_id does not match the caller's company scope",
                ),
                debug,
            )
        }
        Err(error) => return error_response(&error, debug),
    };

    match state.wizards.get(&company_id) {
        Some(wizard) => success_response(wizard.value().clone(), 200),
        None => error_response(
            &UnifiedError::not_found(format!(
                "No setup wizard found for company \"{company_id}\""
            )),
            debug,
        ),
    }
}

pub(super) async fn save_wizard_step(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    body: web::Json<SaveStepRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match require_caller_permission(&ctx, &req, "presets:apply") {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    let request = body.into_inner();
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    // The wizard is keyed by company: the path company must equal
    // the caller scope, and an unresolved caller reaches no wizard.
    let company_id = match require_company_match_uuid(&_tenant, &req, &Some(company_id)) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return error_response(
                &UnifiedError::forbidden(
                    "company_id does not match the caller's company scope",
                ),
                debug,
            )
        }
        Err(error) => return error_response(&error, debug),
    };

    let Some(mut wizard) = state.wizards.get(&company_id).map(|entry| entry.value().clone())
    else {
        return error_response(
            &UnifiedError::not_found(format!(
                "No setup wizard found for company \"{company_id}\""
            )),
            debug,
        );
    };
    if wizard.status == "completed" {
        return error_response(
            &UnifiedError::conflict("The setup wizard is already completed; restart it to run it again"),
            debug,
        );
    }
    // The payload may name the step being saved; it must be the
    // wizard's current step, so steps cannot be skipped silently by
    // saving out of order (use the skip endpoint to skip).
    if let Some(step) = &request.step {
        if step != &wizard.current_step {
            return error_response(
                &UnifiedError::bad_request(format!(
                    "Step \"{step}\" is not the wizard's current step \"{}\"",
                    wizard.current_step
                )),
                debug,
            );
        }
    }
    let saved_step = wizard.current_step.clone();
    let mut changed: Vec<String> = Vec::new();
    // Business-type choice: store the preset and adopt its stored
    // step list (restarting at the first step when the current step
    // is not part of the new list).
    if let Some(preset_id) = request.preset_id {
        let Some(preset) = state.presets.get(&preset_id).map(|entry| entry.value().clone())
        else {
            return error_response(
                &UnifiedError::not_found(format!("Preset \"{preset_id}\" not found")),
                debug,
            );
        };
        if preset.archived {
            return error_response(
                &UnifiedError::conflict(format!(
                    "Preset \"{}\" is archived and cannot be used",
                    preset.slug
                )),
                debug,
            );
        }
        wizard.preset_id = Some(preset_id);
        if !preset.steps.is_empty() {
            wizard.steps = preset.steps.clone();
            if !wizard.steps.contains(&wizard.current_step) {
                wizard.current_step = wizard
                    .steps
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "done".to_string());
            }
        }
        changed.push("preset_id".to_string());
    }
    // Review step: toggle optional modules. Required modules are
    // locked on (see `validate_disabled_optional`).
    if let Some(disabled) = request.disabled_optional_modules {
        let Some(preset_id) = wizard.preset_id else {
            return error_response(
                &UnifiedError::bad_request(
                    "Choose a business type (preset) before reviewing the module list",
                ),
                debug,
            );
        };
        let Some(preset) = state.presets.get(&preset_id).map(|entry| entry.value().clone())
        else {
            return error_response(
                &UnifiedError::not_found(format!("Preset \"{preset_id}\" not found")),
                debug,
            );
        };
        match validate_disabled_optional(&preset, &disabled) {
            Ok(cleaned) => wizard.disabled_optional_modules = cleaned,
            Err(error) => return error_response(&error, debug),
        }
        changed.push("disabled_optional_modules".to_string());
    }
    // Settings answers: merged over the stored answers. The
    // company-name / branding / domain answers are handed off via
    // event at completion; they are stored here, not acted on.
    if let Some(settings) = request.settings {
        if !settings.is_object() {
            return error_response(
                &UnifiedError::bad_request("Wizard settings must be a JSON object"),
                debug,
            );
        }
        wizard.settings = merge_settings(&wizard.settings, &settings);
        changed.push("settings".to_string());
    }
    // The step the wizard is on now: choosing a preset above may
    // have adopted a new stored step list and reset it, so it is
    // read again here instead of reusing the saved step name.
    let current_step = wizard.current_step.clone();
    // The stored "apply" step runs the preset application now, so
    // the wizard can report the result before the settings step.
    if current_step == "apply" {
        match ensure_wizard_applied(&ctx, &state, &mut wizard) {
            Ok(_) => changed.push("applied".to_string()),
            Err(error) => return error_response(&error, debug),
        }
    }
    match next_step(&wizard.steps, &current_step) {
        Some(next) => wizard.current_step = next,
        None => {
            // Saving the last stored step leaves the wizard on it;
            // completion is explicit through the complete endpoint.
            changed.push("final_step".to_string());
        }
    }
    wizard.updated_at = now();
    let actor_id = caller.actor.clone();
    state.wizards.insert(wizard.company_id, wizard.clone());
    emit_audit(
        &ctx,
        WIZARD_STEP_SAVED,
        &actor_id,
        "presets.wizard.step.saved",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({ "step": saved_step, "changed": changed }),
    );
    success_response(wizard, 200)
}

pub(super) async fn skip_wizard_step(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    body: web::Json<WizardActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match require_caller_permission(&ctx, &req, "presets:apply") {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    let request = body.into_inner();
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    // The wizard is keyed by company: the path company must equal
    // the caller scope, and an unresolved caller reaches no wizard.
    let company_id = match require_company_match_uuid(&_tenant, &req, &Some(company_id)) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return error_response(
                &UnifiedError::forbidden(
                    "company_id does not match the caller's company scope",
                ),
                debug,
            )
        }
        Err(error) => return error_response(&error, debug),
    };

    let Some(mut wizard) = state.wizards.get(&company_id).map(|entry| entry.value().clone())
    else {
        return error_response(
            &UnifiedError::not_found(format!(
                "No setup wizard found for company \"{company_id}\""
            )),
            debug,
        );
    };
    if wizard.status == "completed" {
        return error_response(
            &UnifiedError::conflict("The setup wizard is already completed; restart it to run it again"),
            debug,
        );
    }
    if let Some(step) = &request.step {
        if step != &wizard.current_step {
            return error_response(
                &UnifiedError::bad_request(format!(
                    "Step \"{step}\" is not the wizard's current step \"{}\"",
                    wizard.current_step
                )),
                debug,
            );
        }
    }
    let skipped = wizard.current_step.clone();
    // The "apply" step is the one step that cannot be skipped:
    // skipping it would leave the preset unapplied, and skipping
    // it as the last stored step used to mark the wizard completed
    // with no application at all — a later complete then failed
    // as "already completed", leaving no path to apply except
    // restart. Save the step to apply the preset, or restart the
    // wizard.
    if skipped == "apply" {
        return error_response(
            &UnifiedError::bad_request(
                "The \"apply\" step cannot be skipped: save it to apply the preset, or restart the wizard",
            ),
            debug,
        );
    }
    wizard.skipped_steps.push(skipped.clone());
    match next_step(&wizard.steps, &skipped) {
        Some(next) => wizard.current_step = next,
        None => {
            // Skipping the last stored step finishes the wizard
            // without applying anything that was not already applied.
            wizard.status = "completed".to_string();
        }
    }
    wizard.updated_at = now();
    let actor_id = caller.actor.clone();
    state.wizards.insert(wizard.company_id, wizard.clone());
    emit_audit(
        &ctx,
        WIZARD_SKIPPED,
        &actor_id,
        "presets.wizard.step.skipped",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({ "step": skipped }),
    );
    success_response(wizard, 200)
}

pub(super) async fn resume_wizard(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    _body: web::Json<WizardActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match require_caller_permission(&ctx, &req, "presets:apply") {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    // The wizard is keyed by company: the path company must equal
    // the caller scope, and an unresolved caller reaches no wizard.
    let company_id = match require_company_match_uuid(&_tenant, &req, &Some(company_id)) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return error_response(
                &UnifiedError::forbidden(
                    "company_id does not match the caller's company scope",
                ),
                debug,
            )
        }
        Err(error) => return error_response(&error, debug),
    };

    let Some(wizard) = state.wizards.get(&company_id).map(|entry| entry.value().clone()) else {
        return error_response(
            &UnifiedError::not_found(format!(
                "No setup wizard found for company \"{company_id}\""
            )),
            debug,
        );
    };
    let actor_id = caller.actor.clone();
    emit_audit(
        &ctx,
        WIZARD_RESUMED,
        &actor_id,
        "presets.wizard.resumed",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({ "current_step": wizard.current_step, "status": wizard.status }),
    );
    success_response(wizard, 200)
}

pub(super) async fn restart_wizard(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    body: web::Json<WizardActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match require_caller_permission(&ctx, &req, "presets:apply") {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    let request = body.into_inner();
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    // The wizard is keyed by company: the path company must equal
    // the caller scope, and an unresolved caller reaches no wizard.
    let company_id = match require_company_match_uuid(&_tenant, &req, &Some(company_id)) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return error_response(
                &UnifiedError::forbidden(
                    "company_id does not match the caller's company scope",
                ),
                debug,
            )
        }
        Err(error) => return error_response(&error, debug),
    };

    let steps = match request.preset_id {
        Some(preset_id) => {
            match state.presets.get(&preset_id) {
                Some(preset) => {
                    if preset.archived {
                        return error_response(
                            &UnifiedError::conflict(format!(
                                "Preset \"{}\" is archived and cannot be used",
                                preset.slug
                            )),
                            debug,
                        );
                    }
                    if preset.steps.is_empty() {
                        DEFAULT_WIZARD_STEPS
                            .iter()
                            .map(|step| (*step).to_string())
                            .collect()
                    } else {
                        preset.steps.clone()
                    }
                }
                None => {
                    return error_response(
                        &UnifiedError::not_found(format!("Preset \"{preset_id}\" not found")),
                        debug,
                    );
                }
            }
        }
        None => DEFAULT_WIZARD_STEPS
            .iter()
            .map(|step| (*step).to_string())
            .collect(),
    };
    let actor_id = caller.actor.clone();
    let now = now();
    let wizard = WizardState {
        id: Uuid::now_v7(),
        company_id,
        actor_id: actor_id.clone(),
        preset_id: request.preset_id,
        steps: steps.clone(),
        current_step: steps.first().cloned().unwrap_or_else(|| "done".to_string()),
        status: "in_progress".to_string(),
        skipped_steps: Vec::new(),
        disabled_optional_modules: Vec::new(),
        settings: Value::Object(Map::new()),
        application_id: None,
        created_at: now,
        updated_at: now,
    };
    state.wizards.insert(wizard.company_id, wizard.clone());
    emit_audit(
        &ctx,
        WIZARD_RESTARTED,
        &actor_id,
        "presets.wizard.restarted",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({ "preset_id": wizard.preset_id }),
    );
    success_response(wizard, 201)
}

pub(super) async fn complete_wizard(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<PresetsState>,
    company_id: web::Path<String>,
    _body: web::Json<WizardActionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return error_response(&error, debug),
    };
    let caller = match require_caller_permission(&ctx, &req, "presets:apply") {
        Ok(caller) => caller,
        Err(error) => return error_response(&error, debug),
    };
    let company_id = match parse_company_id(&company_id.into_inner()) {
        Ok(id) => id,
        Err(error) => return error_response(&error, debug),
    };
    // The wizard is keyed by company: the path company must equal
    // the caller scope, and an unresolved caller reaches no wizard.
    let company_id = match require_company_match_uuid(&_tenant, &req, &Some(company_id)) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return error_response(
                &UnifiedError::forbidden(
                    "company_id does not match the caller's company scope",
                ),
                debug,
            )
        }
        Err(error) => return error_response(&error, debug),
    };

    let Some(mut wizard) = state.wizards.get(&company_id).map(|entry| entry.value().clone())
    else {
        return error_response(
            &UnifiedError::not_found(format!(
                "No setup wizard found for company \"{company_id}\""
            )),
            debug,
        );
    };
    if wizard.status == "completed" {
        // Completing twice returns the same summary instead of
        // applying the preset a second time.
        if let Some(application_id) = wizard.application_id {
            if let Some(application) = state.applications.get(&application_id) {
                let summary = summary_for(application.value(), &wizard);
                return success_response(summary, 200);
            }
        }
        // Recovery for a wizard that reached "completed" without
        // ever applying (for example by skipping its last step
        // before the apply step was made unskippable): apply now
        // instead of failing as "already completed" and stranding
        // the tenant with restart as the only path to apply.
        if wizard.preset_id.is_some() {
            return match ensure_wizard_applied(&ctx, &state, &mut wizard) {
                Ok(application) => {
                    let summary = summary_for(&application, &wizard);
                    success_response(summary, 200)
                }
                Err(error) => error_response(&error, debug),
            };
        }
        return error_response(
            &UnifiedError::conflict("The setup wizard is already completed"),
            debug,
        );
    }
    if wizard.preset_id.is_none() {
        return error_response(
            &UnifiedError::bad_request(
                "The wizard has no business type chosen yet: save the business_type step first",
            ),
            debug,
        );
    }
    let application = match ensure_wizard_applied(&ctx, &state, &mut wizard) {
        Ok(application) => application,
        Err(error) => return error_response(&error, debug),
    };
    // Settings handoff: the stored answers go out for the owning
    // modules (company, branding, domains) instead of being applied
    // here. The wizard never implements branding or domains itself.
    let company_name = wizard
        .settings
        .get("company_name")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let branding = wizard.settings.get("branding").cloned();
    let domain = wizard
        .settings
        .get("domain")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    ctx.hooks.emit(
        WIZARD_SETTINGS_COMPLETED,
        serde_json::json!({
            "company_id": wizard.company_id,
            "actor_id": wizard.actor_id,
            "company_name": company_name,
            "branding": branding,
            "domain": domain,
        }),
    );
    wizard.status = "completed".to_string();
    wizard.current_step = wizard
        .steps
        .last()
        .cloned()
        .unwrap_or_else(|| "done".to_string());
    wizard.updated_at = now();
    state.wizards.insert(wizard.company_id, wizard.clone());
    let summary = summary_for(&application, &wizard);
    let actor_id = caller.actor.clone();
    emit_audit(
        &ctx,
        WIZARD_COMPLETED,
        &actor_id,
        "presets.wizard.completed",
        "wizard",
        &wizard.id.to_string(),
        Some(wizard.company_id),
        serde_json::json!({
            "preset_id": wizard.preset_id,
            "application_id": application.id,
            "enabled": summary.enabled,
            "skipped_not_entitled": summary.skipped_not_entitled,
            "skipped_dependency_missing": summary.skipped_dependency_missing,
            "failed": summary.failed,
        }),
    );
    success_response(summary, 200)
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::modules::{Module, ModuleRegistry};
    use actix_web::http::StatusCode;
    use actix_web::test::TestRequest;
    use chrono::Utc;

    /// A minimal registry module for wizard tests.
    struct FakeModule {
        id: &'static str,
    }

    impl Module for FakeModule {
        fn id(&self) -> &'static str {
            self.id
        }

        fn name(&self) -> &'static str {
            self.id
        }

        fn description(&self) -> &'static str {
            "test-only module"
        }
    }

    /// Context whose `role.check` allows `presets:apply`, over a
    /// registry holding one fake module.
    fn test_ctx() -> web::Data<Arc<AppContext>> {
        let registry = Arc::new(ModuleRegistry::new());
        registry.register(Arc::new(FakeModule { id: "alpha" }));
        let hooks = Arc::new(HookBus::new());
        hooks.register(
            "role.check",
            "test",
            Arc::new(move |context: &mut HookContext| {
                let permission = context
                    .get("permission")
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                context.set(
                    "allowed",
                    serde_json::json!(permission == "presets:apply"),
                );
            }),
        );
        web::Data::new(Arc::new(AppContext::new(
            AppConfig::default(),
            hooks,
            registry,
        )))
    }

    fn authed_request() -> HttpRequest {
        verified_request("user-1", "operator")
    }

    fn action_body() -> web::Json<WizardActionRequest> {
        web::Json(WizardActionRequest {
            actor: ActorInput {
                viewer_id: Some("body-actor".to_string()),
                viewer_role: Some("admin".to_string()),
                company_id: None,
            },
            preset_id: None,
            step: None,
        })
    }

    fn seed_preset(state: &PresetsState) -> Preset {
        let now = Utc::now();
        let preset = Preset {
            id: Uuid::now_v7(),
            slug: "test-preset".to_string(),
            name: "Test Preset".to_string(),
            description: String::new(),
            business_type: "test".to_string(),
            built_in: false,
            archived: false,
            modules: vec![PresetModuleEntry {
                module_id: "alpha".to_string(),
                required: true,
                sort_order: 0,
            }],
            settings: serde_json::json!({}),
            steps: DEFAULT_WIZARD_STEPS
                .iter()
                .map(|step| step.to_string())
                .collect(),
            created_by: None,
            created_at: now,
            updated_at: now,
        };
        state.presets.insert(preset.id, preset.clone());
        preset
    }

    fn seed_wizard(
        state: &PresetsState,
        company_id: Uuid,
        preset_id: Option<Uuid>,
        steps: Vec<&str>,
        current_step: &str,
        status: &str,
    ) -> WizardState {
        let now = Utc::now();
        let wizard = WizardState {
            id: Uuid::now_v7(),
            company_id,
            actor_id: "user-1".to_string(),
            preset_id,
            steps: steps.iter().map(|step| step.to_string()).collect(),
            current_step: current_step.to_string(),
            status: status.to_string(),
            skipped_steps: Vec::new(),
            disabled_optional_modules: Vec::new(),
            settings: serde_json::json!({}),
            application_id: None,
            created_at: now,
            updated_at: now,
        };
        state.wizards.insert(company_id, wizard.clone());
        wizard
    }

    #[test]
    fn skip_apply_step_is_refused_and_leaves_the_wizard_open() {
        let ctx = test_ctx();
        let state = PresetsState::new();
        let company_id = Uuid::now_v7();
        seed_wizard(
            &state,
            company_id,
            None,
            vec!["business_type", "review_modules", "apply", "settings", "done"],
            "apply",
            "in_progress",
        );
        let response = skip_wizard_step(
            authed_request(),
            ctx,
            web::Data::new(state.clone()),
            web::Path::from(company_id.to_string()),
            action_body(),
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let stored = state
            .wizards
            .get(&company_id)
            .expect("wizard stored")
            .value()
            .clone();
        assert_eq!(stored.status, "in_progress");
        assert_eq!(stored.current_step, "apply");
        assert!(stored.skipped_steps.is_empty());
        assert!(stored.application_id.is_none());
    }

    #[test]
    fn skip_last_non_apply_step_still_completes_the_wizard() {
        let ctx = test_ctx();
        let state = PresetsState::new();
        let company_id = Uuid::now_v7();
        seed_wizard(
            &state,
            company_id,
            None,
            vec!["business_type", "done"],
            "done",
            "in_progress",
        );
        let response = skip_wizard_step(
            authed_request(),
            ctx,
            web::Data::new(state.clone()),
            web::Path::from(company_id.to_string()),
            action_body(),
        );
        assert_eq!(response.status(), StatusCode::OK);
        let stored = state
            .wizards
            .get(&company_id)
            .expect("wizard stored")
            .value()
            .clone();
        assert_eq!(stored.status, "completed");
        assert_eq!(stored.skipped_steps, vec!["done".to_string()]);
    }

    #[test]
    fn complete_recovers_a_wizard_completed_without_applying() {
        let ctx = test_ctx();
        let state = PresetsState::new();
        let preset = seed_preset(&state);
        let company_id = Uuid::now_v7();
        seed_wizard(
            &state,
            company_id,
            Some(preset.id),
            vec!["business_type", "review_modules", "apply", "settings", "done"],
            "done",
            "completed",
        );
        let response = complete_wizard(
            authed_request(),
            ctx,
            web::Data::new(state.clone()),
            web::Path::from(company_id.to_string()),
            action_body(),
        );
        assert_eq!(response.status(), StatusCode::OK);
        let stored = state
            .wizards
            .get(&company_id)
            .expect("wizard stored")
            .value()
            .clone();
        let application_id = stored.application_id.expect("application recorded");
        let application = state
            .applications
            .get(&application_id)
            .expect("application stored")
            .value()
            .clone();
        assert_eq!(application.status, "completed");
        assert_eq!(application.outcomes.len(), 1);
        assert_eq!(application.outcomes[0].module_id, "alpha");
        assert_eq!(application.outcomes[0].status, OUTCOME_ENABLED);
    }
}
