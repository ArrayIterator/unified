//! crm - AI capabilities.
//!
//! Part of the `crm` module split; see `src/modules/crm.rs`.
//!
//! Declared here, registered once in `init` (see `module.rs`
//! or `routes.rs`), and run through the AI module's gated runner
//! (`run_ai_capability`): the allowlist gate runs first, PII is
//! redacted unless the capability allows it, and outputs are
//! returned inline, never published or sent by this module.

use crate::modules::ai;

/// AI capabilities this module exposes through the AI module's
/// contract. All are non-PII: inputs are redacted before they
/// reach the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "draft_follow_up",
        module_id: "crm",
        name: "Draft a follow-up",
        description: "Draft a follow-up message for a lead or contact from the supplied notes. The output is returned inline; it is never sent by this module.",
        input_fields: &["notes"],
        allows_pii: false,
        use_case: "email",
    },
    ai::AiCapability {
        id: "analyze_lead",
        module_id: "crm",
        name: "Analyze a lead",
        description: "Analyze caller-supplied lead notes and return fit signals, open questions, and a suggested next step.",
        input_fields: &["notes"],
        allows_pii: false,
        use_case: "complete",
    },
];

#[cfg(test)]
mod tests {
    use super::AI_CAPABILITIES;
    use crate::modules::ai;

    #[test]
    fn registers_capabilities() {
        ai::register_ai_capabilities("crm", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("crm");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "crm");
            assert!(!capability.name.is_empty());
            assert!(!capability.description.is_empty());
            assert!(!capability.input_fields.is_empty());
            assert!(!capability.use_case.is_empty());
        }
    }

    #[test]
    fn capability_ids_are_unique() {
        let mut ids: Vec<&str> = AI_CAPABILITIES.iter().map(|item| item.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), AI_CAPABILITIES.len());
    }
}
