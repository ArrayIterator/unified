//! calculator - state.

use dashmap::DashMap;
use serde_json::Value;
use std::sync::RwLock;
use super::types::*;
use super::validation::*;

pub struct CalculatorState {
    pub calculations: DashMap<String, SavedCalculation>,
    pub reports: DashMap<String, CalculationReport>,
    pub history: DashMap<String, CalculationHistoryEntry>,
    pub rates: DashMap<String, CurrencyRate>,
    pub settings: RwLock<CalculatorSettings>,
}

impl CalculatorState {
    pub(crate) fn new() -> Self {
        Self {
            calculations: DashMap::new(),
            reports: DashMap::new(),
            history: DashMap::new(),
            rates: DashMap::new(),
            settings: RwLock::new(CalculatorSettings::default()),
        }
    }

    pub(crate) fn settings_snapshot(&self) -> CalculatorSettings {
        self.settings
            .read()
            .expect("calculator settings lock poisoned")
            .clone()
    }

    /// Append one history entry for a completed calculation.
    pub(crate) fn record_history(
        &self,
        company_id: &str,
        calculator_type: &str,
        actor: &str,
        inputs: Value,
        results: Value,
    ) -> CalculationHistoryEntry {
        let entry = CalculationHistoryEntry {
            id: new_id(),
            company_id: company_id.to_string(),
            calculator_type: calculator_type.to_string(),
            inputs,
            results,
            actor: actor.to_string(),
            created_at: now(),
            updated_at: now(),
        };
        self.history.insert(entry.id.clone(), entry.clone());
        entry
    }

    /// The stored rate for a currency pair, preferring the provider
    /// named in settings, otherwise the most recently fetched rate
    /// for the pair. Returns `None` when no rate is stored: callers
    /// must report the rate as unavailable, never invent one.
    pub(crate) fn rate_for(&self, base: &str, target: &str) -> Option<CurrencyRate> {
        if base == target {
            return None;
        }
        let preferred = self.settings_snapshot().rate_provider;
        let mut best: Option<CurrencyRate> = None;
        for entry in self.rates.iter() {
            let rate = entry.value();
            if rate.base_currency != base || rate.target_currency != target {
                continue;
            }
            let better = match &best {
                None => true,
                Some(current) => {
                    (rate.provider == preferred && current.provider != preferred)
                        || (rate.provider == current.provider
                            && rate.fetched_at > current.fetched_at)
                }
            };
            if better {
                best = Some(rate.clone());
            }
        }
        best
    }
}
