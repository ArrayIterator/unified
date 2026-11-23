// Presets + Setup Wizard module.
//
// Full module notes (routes, honesty notes) live at the
// top of `src/modules/presets/module.rs`.

pub mod ai_capabilities;
mod apply;
pub mod events;
mod helpers;
mod identity;
mod lookups;
mod module;
mod preset_handlers;
pub mod records;
pub mod requests;
pub mod seeds;
pub mod stores;
mod wizard_handlers;
pub mod wizard_steps;

// Re-exported from `events`.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::events::PRESET_APPLIED;
pub use self::events::PRESET_ARCHIVED;
pub use self::events::PRESET_CLONED;
pub use self::events::PRESET_CREATED;
pub use self::events::PRESET_MODULES_REQUESTED;
pub use self::events::PRESET_MODULE_ENABLED;
pub use self::events::PRESET_MODULE_SKIPPED;
pub use self::events::PRESET_SETTINGS_APPLIED;
pub use self::events::PRESET_UPDATED;
pub use self::events::WIZARD_COMPLETED;
pub use self::events::WIZARD_RESTARTED;
pub use self::events::WIZARD_RESUMED;
pub use self::events::WIZARD_SETTINGS_COMPLETED;
pub use self::events::WIZARD_SKIPPED;
pub use self::events::WIZARD_STARTED;
pub use self::events::WIZARD_STEP_SAVED;
// Re-exported from `wizard_steps`.
pub use self::wizard_steps::DEFAULT_WIZARD_STEPS;
pub use self::wizard_steps::OUTCOME_ALREADY_ENABLED;
pub use self::wizard_steps::OUTCOME_ENABLED;
pub use self::wizard_steps::OUTCOME_FAILED;
pub use self::wizard_steps::OUTCOME_SKIPPED_DEPENDENCY_MISSING;
pub use self::wizard_steps::OUTCOME_SKIPPED_NOT_ENTITLED;
pub use self::wizard_steps::OUTCOME_SKIPPED_OPTIONAL_OFF;
pub use self::wizard_steps::WIZARD_STEP_NAMES;
// Re-exported from `records`.
pub use self::records::ModuleApplyOutcome;
pub use self::records::Preset;
pub use self::records::PresetApplication;
pub use self::records::PresetModuleEntry;
pub use self::records::WizardState;
pub use self::records::WizardSummary;
// Re-exported from `stores`.
pub use self::stores::register;
pub use self::stores::PresetsModule;
pub use self::stores::PresetsState;
// Re-exported from `seeds`.
pub use self::seeds::SEED_AGENCY_ID;
pub use self::seeds::SEED_FREELANCER_ID;
pub use self::seeds::SEED_PROPERTY_SELLER_ID;
pub use self::seeds::SEED_SAAS_SELLER_ID;
// Re-exported from `requests`.
pub use self::requests::PresetPreview;
pub use self::requests::PresetPreviewModule;
