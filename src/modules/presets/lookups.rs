//! Presets + Setup Wizard module. — Shared lookups.
//!
//! Split from `src/modules/presets.rs`; behaviour unchanged.

use super::apply::*;
use super::events::*;
use super::helpers::*;
use super::identity::*;
use super::preset_handlers::*;
use super::records::*;
use super::requests::*;
use super::seeds::*;
use super::stores::*;
use super::wizard_handlers::*;
use super::wizard_steps::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use uuid::Uuid;

/// Find a preset by UUID or by slug. A slug lookup never collides
/// with a UUID because slugs may not contain the characters a UUID
/// needs in the same shape; the UUID parse is tried first.
pub(super) fn find_preset(state: &PresetsState, key: &str) -> Option<Preset> {
    if let Ok(id) = Uuid::parse_str(key) {
        if let Some(preset) = state.presets.get(&id) {
            return Some(preset.value().clone());
        }
    }
    state
        .presets
        .iter()
        .find(|entry| entry.value().slug == key)
        .map(|entry| entry.value().clone())
}

pub(super) fn slug_in_use(state: &PresetsState, slug: &str, except: Option<Uuid>) -> bool {
    state.presets.iter().any(|entry| {
        entry.value().slug == slug && Some(entry.value().id) != except
    })
}

/// Advance one step in the stored step list. Returns `None` when
/// `current` is the last step (or is not in the list at all).
pub(super) fn next_step(steps: &[String], current: &str) -> Option<String> {
    steps
        .iter()
        .position(|step| step == current)
        .and_then(|index| steps.get(index + 1).cloned())
}

/// Run the wizard's preset application once, and remember it on the
/// wizard. A second call returns the stored application instead of
/// applying twice.
pub(super) fn ensure_wizard_applied(
    ctx: &AppContext,
    state: &PresetsState,
    wizard: &mut WizardState,
) -> UnifiedResult<PresetApplication> {
    if let Some(application_id) = wizard.application_id {
        if let Some(application) = state.applications.get(&application_id) {
            return Ok(application.value().clone());
        }
    }
    let Some(preset_id) = wizard.preset_id else {
        return Err(UnifiedError::bad_request(
            "The wizard has no business type chosen yet: save the business_type step first",
        ));
    };
    let Some(preset) = state.presets.get(&preset_id).map(|entry| entry.value().clone()) else {
        return Err(UnifiedError::not_found(format!(
            "Preset \"{preset_id}\" not found"
        )));
    };
    if preset.archived {
        return Err(UnifiedError::conflict(format!(
            "Preset \"{}\" is archived and cannot be applied",
            preset.slug
        )));
    }
    let application = apply_preset(
        ctx,
        state,
        &preset,
        wizard.company_id,
        &wizard.actor_id.clone(),
        &wizard.disabled_optional_modules.clone(),
        Some(&wizard.settings.clone()),
    )?;
    wizard.application_id = Some(application.id);
    wizard.updated_at = now();
    state.wizards.insert(wizard.company_id, wizard.clone());
    Ok(application)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_step_advances_and_stops() {
        let steps = vec![
            "business_type".to_string(),
            "review_modules".to_string(),
            "apply".to_string(),
        ];
        assert_eq!(
            next_step(&steps, "business_type").as_deref(),
            Some("review_modules")
        );
        assert_eq!(
            next_step(&steps, "review_modules").as_deref(),
            Some("apply")
        );
        assert_eq!(next_step(&steps, "apply"), None);
        assert_eq!(next_step(&steps, "missing"), None);
        assert_eq!(next_step(&[], "business_type"), None);
    }
}
