//! Presets + Setup Wizard module. - Wizard steps. Closed vocabulary of step names (see the module header: the same closed-set pattern as `ProductCategory`). A preset may store any ordered subset of these names as its step list; the wizard stores that list and navigates by index, so step order is data.
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
use super::wizard_handlers::*;

/// Every step name a preset or wizard may use.
pub const WIZARD_STEP_NAMES: &[&str] = &[
    "business_type",
    "review_modules",
    "apply",
    "settings",
    "done",
];

/// Configurable default step sequence, used only when a preset
/// stores no step list of its own. Presets created or cloned through
/// the API may store a different sequence.
pub const DEFAULT_WIZARD_STEPS: &[&str] = &[
    "business_type",
    "review_modules",
    "apply",
    "settings",
    "done",
];

/// Outcome status of one module inside a preset application.
pub const OUTCOME_ENABLED: &str = "enabled";
/// The module was already enabled for the tenant before this apply.
pub const OUTCOME_ALREADY_ENABLED: &str = "already_enabled";
/// The tenant holds no entitlement for a sellable module.
pub const OUTCOME_SKIPPED_NOT_ENTITLED: &str = "skipped_not_entitled";
/// A declared dependency is not registered, or the dependency graph
/// has a cycle.
pub const OUTCOME_SKIPPED_DEPENDENCY_MISSING: &str = "skipped_dependency_missing";
/// An optional module the caller toggled off in the review step.
pub const OUTCOME_SKIPPED_OPTIONAL_OFF: &str = "skipped_optional_off";
/// Enabling failed for another reason (see `reason`).
pub const OUTCOME_FAILED: &str = "failed";
