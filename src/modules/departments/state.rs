//! In-memory department and routing-rule store.

use unified_core::hooks::{HookBus};
use unified_core::json;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use super::types::{Department, RoutingRule};

// State

pub struct DepartmentsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) departments: DashMap<String, Department>,
    pub(crate) rules: DashMap<String, RoutingRule>,
}

impl DepartmentsState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            departments: DashMap::new(),
            rules: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Ask the Roles module whether a role exists (`role.check` with
    /// a `role_id` payload). `false` when no handler answers.
    pub(crate) fn role_exists(&self, role_id: &str) -> bool {
        let context = self.bus().emit(
            "role.check",
            json!({ "role_id": role_id, "permission": "*", "allowed": true }),
        );
        context
            .get("allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Ask the Tickets module how many tickets still belong to a
    /// department (`department.delete_check`). `None` when nothing
    /// answers.
    pub(crate) fn ticket_count_for(&self, department_id: &str) -> Option<u64> {
        let context = self.bus().emit(
            "department.delete_check",
            json!({ "department_id": department_id }),
        );
        for result in &context.results {
            if let Some(count) = result.get("ticket_count").and_then(|v| v.as_u64()) {
                return Some(count);
            }
        }
        None
    }

    pub(crate) fn sorted_rules(&self) -> Vec<RoutingRule> {
        let mut rules: Vec<RoutingRule> = self
            .rules
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        rules.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at))
        });
        rules
    }

    /// The department a ticket routes to, plus the rule that matched
    /// (if any). Rules run first; the default department is the
    /// fallback; inactive departments never receive tickets.
    pub(crate) fn route_for(
        &self,
        title: &str,
        body: &str,
        category: &str,
        product: Option<&str>,
    ) -> (Option<String>, Option<String>) {
        let title_lower = title.to_lowercase();
        let body_lower = body.to_lowercase();
        let category_lower = category.to_lowercase();
        let product_lower = product.map(str::to_lowercase);
        for rule in self.sorted_rules() {
            if !rule.enabled {
                continue;
            }
            let Some(department) = self.departments.get(&rule.department_id) else {
                continue;
            };
            if department.status != "active" {
                continue;
            }
            let matched = match rule.match_type.as_str() {
                "category" => category_lower == rule.match_value.to_lowercase(),
                "product" => product_lower
                    .as_ref()
                    .is_some_and(|product| *product == rule.match_value.to_lowercase()),
                "keyword" => {
                    let needle = rule.match_value.to_lowercase();
                    !needle.is_empty()
                        && (title_lower.contains(&needle) || body_lower.contains(&needle))
                }
                _ => false,
            };
            if matched {
                return (Some(rule.department_id.clone()), Some(rule.id.clone()));
            }
        }
        let default = self
            .departments
            .iter()
            .find(|entry| entry.value().is_default && entry.value().status == "active")
            .map(|entry| entry.key().clone());
        (default, None)
    }
}
