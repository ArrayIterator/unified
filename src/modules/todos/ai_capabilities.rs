//! todos - AI capabilities.
//!
//! Part of the `todos` module split; see `src/modules/todos.rs`.
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
        id: "summarize_day",
        module_id: "todos",
        name: "Summarize the day",
        description: "Summarize a caller-supplied todo list for one day into what is done, due, and overdue.",
        input_fields: &["todos"],
        allows_pii: false,
        use_case: "complete",
    },
    ai::AiCapability {
        id: "draft_day_plan",
        module_id: "todos",
        name: "Draft a day plan",
        description: "Draft a day plan from a caller-supplied todo list, ordered by priority and due time.",
        input_fields: &["todos"],
        allows_pii: false,
        use_case: "todos.draft_day_plan",
    },
];

#[cfg(test)]
mod tests {
    use super::AI_CAPABILITIES;
    use crate::modules::ai;

    #[test]
    fn registers_capabilities() {
        ai::register_ai_capabilities("todos", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("todos");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }

    #[test]
    fn capabilities_are_well_formed() {
        for capability in AI_CAPABILITIES {
            assert!(!capability.id.is_empty());
            assert_eq!(capability.module_id, "todos");
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
