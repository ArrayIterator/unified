//! tickets - AI capabilities.
//!
//! Part of the `tickets` module split; see `src/modules/tickets.rs`.
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
        id: "draft_reply",
        module_id: "tickets",
        name: "Draft a ticket reply",
        description: "Draft a reply to a caller-supplied ticket message. The output is returned inline; it is never sent by this module.",
        input_fields: &["message"],
        allows_pii: false,
        use_case: "support_reply",
    },
    ai::AiCapability {
        id: "summarize_ticket",
        module_id: "tickets",
        name: "Summarize a ticket",
        description: "Summarize a caller-supplied ticket thread into the issue, what was tried, and the current status.",
        input_fields: &["thread"],
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
        ai::register_ai_capabilities("tickets", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("tickets");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "tickets");
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
