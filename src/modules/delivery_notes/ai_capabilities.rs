//! Delivery notes AI capabilities.
//!
//! Declared here, registered once in `init` (see `module.rs`),
//! and run through the AI module's gated runner
//! (`run_ai_capability`): the allowlist gate runs first, and
//! outputs are returned inline, never published or sent by
//! this module.
//!
//! `summarize_delivery_note` turns a DO's stored facts (parties,
//! items, quantities, driver, vehicle, status, proof, return)
//! into a short summary. `check_delivery_readiness` reports
//! the stored-facts checklist (items shipped, driver and
//! vehicle named, proof linked when required) as data. The AI
//! never issues, delivers, returns, or cancels a DO on its own.

use crate::modules::ai;

/// AI capabilities this module exposes through the AI module's
/// contract.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "summarize_delivery_note",
        module_id: "delivery_notes",
        name: "Summarize a delivery note",
        description: "Summarize a delivery note from its stored facts (number, status, sender, receiver, warehouse, driver, vehicle, items with ordered/shipped/returned quantities, proof of delivery, return reason and scope) into a short paragraph. The output is returned inline; it never changes the delivery note.",
        input_fields: &["tenant_id", "delivery_note_id", "number", "status", "sender_name", "receiver_name", "items", "total_ordered", "total_shipped", "total_returned", "driver_name", "vehicle_plate", "proof_of_delivery_id", "return_reason", "return_scope"],
        allows_pii: false,
        use_case: "complete",
    },
    ai::AiCapability {
        id: "check_delivery_readiness",
        module_id: "delivery_notes",
        name: "Check delivery readiness",
        description: "Check whether a delivery note is ready to go out, from its stored facts only: every item shipped, a driver and vehicle named, sender and receiver present, and a proof of delivery linked when the tenant requires one. The output is a readiness checklist returned inline; it never issues or delivers the note.",
        input_fields: &["tenant_id", "delivery_note_id", "number", "status", "has_items", "all_shipped", "has_driver", "has_vehicle", "has_proof_of_delivery", "proof_required"],
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
        ai::register_ai_capabilities("delivery_notes", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("delivery_notes");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "delivery_notes");
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

    #[test]
    fn no_capability_allows_pii() {
        assert!(AI_CAPABILITIES.iter().all(|item| !item.allows_pii));
    }
}
