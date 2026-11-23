//! cloud - AI capabilities.
//!
//! Declared here, registered once in `init` (see `module.rs`), and
//! run through the AI module's gated runner (`run_ai_capability`):
//! the allowlist gate runs first, PII is redacted unless the
//! capability allows it, and outputs are returned inline, never
//! published or sent by this module.

use crate::modules::ai;

/// AI capabilities this module exposes through the AI module's
/// contract. All are non-PII: inputs are redacted before they reach
/// the model.
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "summarize_cloud_usage",
        module_id: "cloud",
        name: "Summarize cloud usage",
        description: "Summarize a tenant's cloud footprint from plan, account, and app counts and resource totals.",
        input_fields: &["plans", "accounts", "apps"],
        allows_pii: false,
        use_case: "content",
    },
    ai::AiCapability {
        id: "draft_app_deploy_plan",
        module_id: "cloud",
        name: "Draft an app deployment plan",
        description: "Draft a deployment plan for an app from its image, plan resources, and target provider.",
        input_fields: &["image", "plan", "provider"],
        allows_pii: false,
        use_case: "content",
    },
    ai::AiCapability {
        id: "troubleshoot_deploy_failure",
        module_id: "cloud",
        name: "Troubleshoot a failed deployment",
        description: "Explain a failed app deployment from the provider error and the app status, and suggest the next check.",
        input_fields: &["error", "status"],
        allows_pii: false,
        use_case: "content",
    },
];

#[cfg(test)]
mod tests {
    use super::AI_CAPABILITIES;
    use crate::modules::ai;

    #[test]
    fn registers_capabilities() {
        ai::register_ai_capabilities("cloud", AI_CAPABILITIES);
        let registered = ai::ai_capability_registry().capabilities_for("cloud");
        assert!(!registered.is_empty());
        assert_eq!(registered.len(), AI_CAPABILITIES.len());
    }
}
