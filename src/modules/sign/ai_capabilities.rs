//! sign - AI capabilities.
//!
//! Part of the `sign` module split; see `src/modules/sign.rs`.
//!
//! Declared here, registered once in `init` (see `module.rs`),
//! and run through the AI module's gated runner
//! (`run_ai_capability`): the allowlist gate runs first, PII is
//! redacted unless the capability allows it, and outputs are
//! returned inline, never published or sent by this module.

use crate::modules::ai;

/// AI capabilities this module exposes through the AI module's
/// contract. All are non-PII: inputs are redacted before they
/// reach the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "draft_envelope_message",
        module_id: "sign",
        name: "Draft a signing request message",
        description: "Draft the message sent to signers with a signing request, from an envelope title and context. The output is returned inline; it is never sent by this module.",
        input_fields: &["title", "context"],
        allows_pii: false,
        use_case: "content",
    },
    ai::AiCapability {
        id: "summarize_envelope_status",
        module_id: "sign",
        name: "Summarize envelope status",
        description: "Summarize caller-supplied envelope progress (who has signed, who is pending, expiry) into plain language. The output is returned inline; it never changes an envelope.",
        input_fields: &["status_summary"],
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
        ai::register_ai_capabilities("sign", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("sign");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "sign");
            assert!(!capability.name.is_empty());
            assert!(!capability.description.is_empty());
            assert!(!capability.input_fields.is_empty());
            assert!(!capability.use_case.is_empty());
        }
    }
}
