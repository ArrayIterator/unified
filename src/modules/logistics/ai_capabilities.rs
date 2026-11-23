//! logistics - AI capabilities.
//!
//! Part of the `logistics` module split; see `src/modules/logistics.rs`.
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
        id: "draft_delivery_message",
        module_id: "logistics",
        name: "Draft a delivery message",
        description: "Draft a delivery update message from the caller-supplied shipment details. The output is returned inline; it is never sent by this module.",
        input_fields: &["shipment_details"],
        allows_pii: false,
        use_case: "email",
    },
    ai::AiCapability {
        id: "summarize_shipment",
        module_id: "logistics",
        name: "Summarize a shipment",
        description: "Summarize caller-supplied tracking events for one shipment into current status and next expected step.",
        input_fields: &["tracking_events"],
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
        ai::register_ai_capabilities("logistics", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("logistics");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "logistics");
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
