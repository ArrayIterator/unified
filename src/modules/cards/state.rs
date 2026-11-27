// cards - In-memory state and its methods.

use super::*;
use unified_core::hooks::HookBus;
use dashmap::DashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

// Module state (in-memory; migration 000099_cards is the
// persistence target - see the module header).

/// Live module state. Custom rules and daily counts are keyed by
/// tenant id; settings are stored per tenant with defaults when a
/// tenant has none. The hook bus is attached by `init` so hook
/// handlers can emit `cards.validated`.
pub struct CardsState {
    pub rules: DashMap<Uuid, CardCustomRule>,
    pub settings: DashMap<String, CardsSettings>,
    /// (tenant id, UTC date `YYYY-MM-DD`, matched rule, result)
    /// -> count. Result is `valid` or `invalid`. No PANs.
    pub counts: DashMap<(String, String, String, String), u64>,
    pub hooks: RwLock<Option<Arc<HookBus>>>,
}

impl CardsState {
    pub fn new() -> Self {
        Self {
            rules: DashMap::new(),
            settings: DashMap::new(),
            counts: DashMap::new(),
            hooks: RwLock::new(None),
        }
    }

    /// Settings for a tenant, or the defaults when the tenant has
    /// never saved settings.
    pub fn settings_for(&self, tenant_id: &str) -> CardsSettings {
        self.settings
            .get(tenant_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Enabled custom rules of one tenant, in a deterministic
    /// order (oldest first, then id) so matching ties resolve the
    /// same way on every call.
    fn tenant_rules(&self, tenant_id: &str) -> Vec<CardCustomRule> {
        let mut rules: Vec<CardCustomRule> = self
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant_id && rule.enabled)
            .collect();
        rules.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then(a.id.to_string().cmp(&b.id.to_string()))
        });
        rules
    }

    /// The enabled override rule for one standard brand id, if the
    /// tenant defined one. Earliest created wins when several
    /// exist for the same brand.
    pub(crate) fn override_rule(&self, tenant_id: &str, brand_id: &str) -> Option<CardCustomRule> {
        self.tenant_rules(tenant_id)
            .into_iter()
            .find(|rule| rule.overrides_standard_brand.as_deref() == Some(brand_id))
    }

    /// Best standalone (non-override) custom rule matching
    /// `digits`: longest prefix wins, then earliest created.
    pub(crate) fn match_custom_rule(
        &self,
        tenant_id: &str,
        digits: &str,
    ) -> Option<(CardCustomRule, usize)> {
        let mut best: Option<(CardCustomRule, usize)> = None;
        for rule in self.tenant_rules(tenant_id) {
            if rule.overrides_standard_brand.is_some() {
                continue;
            }
            let Some(prefix_len) = custom_rule_match_len(&rule, digits) else {
                continue;
            };
            let better = match &best {
                None => true,
                Some((current_rule, current_len)) => {
                    prefix_len > *current_len
                        || (prefix_len == *current_len
                            && (rule.created_at, rule.id.to_string())
                                < (current_rule.created_at, current_rule.id.to_string()))
                }
            };
            if better {
                best = Some((rule, prefix_len));
            }
        }
        best
    }
}

impl Default for CardsState {
    fn default() -> Self {
        Self::new()
    }
}
