//! admissions - AI capabilities.
//!
//! Part of the `admissions` module split; see `src/modules/admissions.rs`.
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
        id: "draft_result_message",
        module_id: "admissions",
        name: "Draft admission result message",
        description: "Draft a result announcement message for one applicant from the admission period, program, and result. The output is returned inline; it is never sent by this module.",
        input_fields: &["period", "program", "result"],
        allows_pii: false,
        use_case: "email",
    },
    ai::AiCapability {
        id: "summarize_applications",
        module_id: "admissions",
        name: "Summarize applications",
        description: "Summarize caller-supplied application statistics for one admission period into key figures and trends. Only aggregate figures are accepted, never applicant identities.",
        input_fields: &["period", "statistics"],
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
        ai::register_ai_capabilities("admissions", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("admissions");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "admissions");
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
