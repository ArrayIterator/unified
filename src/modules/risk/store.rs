// risk — store.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::common::*;
use super::handlers_risks::*;
use super::handlers_plans::*;
use super::handlers_templates::*;
use super::handlers_instances::*;
use super::module::*;

impl RiskState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            risks: DashMap::new(),
            plans: DashMap::new(),
            tasks: DashMap::new(),
            templates: DashMap::new(),
            template_items: DashMap::new(),
            instances: DashMap::new(),
            instance_items: DashMap::new(),
            alerts: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    pub(crate) fn emit_with_results(&self, event: &str, data: Value) -> HookContext {
        self.bus().emit(event, data)
    }

    /// Ask the Roles module whether `role` holds `permission`
    /// (`role.check`). Fails closed: no answer means not allowed.
    pub(crate) fn check_capability(&self, role: &str, permission: &str) -> bool {
        let context = self.bus().emit(
            "role.check",
            json!({ "role": role, "permission": permission, "allowed": false }),
        );
        context
            .get("allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Send an alert through the Notifications module.
    pub(crate) fn notify(&self, user_id: &str, title: String, body: String) {
        if user_id.trim().is_empty() {
            return;
        }
        self.emit(
            "notification.requested",
            json!({
                "user_id": user_id.trim(),
                "title": title,
                "body": body,
                "kind": "risk",
            }),
        );
    }

    /// Record an alert and notify the owner, unless an identical
    /// alert (same risk, kind, and figures) already exists — the
    /// threshold and review checks run repeatedly and must not spam
    /// the owner with the same alert.
    pub(crate) fn record_alert(
        &self,
        risk: &Risk,
        kind: &str,
        message: String,
        figures: Value,
    ) -> Option<RiskAlert> {
        let duplicate = self.alerts.iter().any(|entry| {
            let alert = entry.value();
            alert.risk_id == risk.id && alert.kind == kind && alert.figures == figures
        });
        if duplicate {
            return None;
        }
        let alert = RiskAlert {
            id: new_id(),
            risk_id: risk.id.clone(),
            company_id: risk.company_id.clone(),
            kind: kind.to_string(),
            message: message.clone(),
            score: Some(risk.score),
            notified_user_id: Some(risk.owner_id.clone()),
            figures,
            created_at: Utc::now(),
        };
        self.alerts.insert(alert.id.clone(), alert.clone());
        Some(alert)
    }

    /// Check whether a risk crosses its alert threshold and, when it
    /// does, emit `risk.threshold_crossed`, notify the owner, and
    /// record the alert. Returns true when an alert was raised.
    /// The threshold rule is: alert when score >= alert_threshold.
    /// Closed and accepted risks never alert: editing one must not
    /// raise a fresh threshold alert for a risk that is done.
    pub(crate) fn check_threshold(&self, risk: &Risk) -> bool {
        if matches!(risk.status.as_str(), "closed" | "accepted") {
            return false;
        }
        if risk.score < risk.alert_threshold {
            return false;
        }
        let figures = json!({
            "likelihood": risk.likelihood,
            "impact": risk.impact,
            "score": risk.score,
            "band": risk.band,
            "alert_threshold": risk.alert_threshold,
        });
        let message = format!(
            "Risk \"{}\" scored {} ({} band, likelihood {} x impact {}) and reached its alert threshold of {}.",
            risk.title, risk.score, risk.band, risk.likelihood, risk.impact, risk.alert_threshold
        );
        let raised = self
            .record_alert(risk, "threshold_crossed", message.clone(), figures)
            .is_some();
        if raised {
            self.emit(
                "risk.threshold_crossed",
                json!({
                    "risk_id": risk.id,
                    "company_id": risk.company_id,
                    "title": risk.title,
                    "likelihood": risk.likelihood,
                    "impact": risk.impact,
                    "score": risk.score,
                    "band": risk.band,
                    "alert_threshold": risk.alert_threshold,
                    "owner_id": risk.owner_id,
                }),
            );
            self.notify(
                &risk.owner_id,
                format!("Risk threshold crossed: {}", risk.title),
                message,
            );
        }
        raised
    }

    /// Ask the Projects module to create the task for a mitigation
    /// task, through `risk.mitigation_task_requested`. Returns the
    /// project task id when a handler answers with one; None means
    /// no handler answered (the projects module has not wired this
    /// event yet — see the module header convergence note).
    pub(crate) fn request_project_task(&self, task: &MitigationTask, project_id: Option<&str>) -> Option<String> {
        let context = self.emit_with_results(
            "risk.mitigation_task_requested",
            json!({
                "risk_id": task.risk_id,
                "plan_id": task.plan_id,
                "task_id": task.id,
                "project_id": project_id,
                "title": task.title,
                "description": task.description,
                "assignee_id": task.assignee_id,
                "due_date": task.due_date,
            }),
        );
        for result in &context.results {
            if let Some(task_id) = result.get("project_task_id").and_then(|v| v.as_str()) {
                if !task_id.trim().is_empty() {
                    return Some(task_id.to_string());
                }
            }
        }
        None
    }

    /// Scan risks whose review date has passed without the risk
    /// being closed or accepted. One `review_overdue` alert per
    /// risk per review date, plus a notification to the owner.
    /// Returns the number of alerts raised.
    pub(crate) fn run_review_check(&self, company_id: Option<&str>) -> u32 {
        let now = Utc::now();
        let mut raised: u32 = 0;
        let risks: Vec<Risk> = self
            .risks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|risk| match company_id {
                Some(company) => risk.company_id.as_deref() == Some(company),
                None => true,
            })
            .collect();
        for risk in risks {
            if matches!(risk.status.as_str(), "closed" | "accepted") {
                continue;
            }
            let Some(review_at) = risk.next_review_at else {
                continue;
            };
            if review_at >= now {
                continue;
            }
            let figures = json!({
                "review_date": review_at,
                "score": risk.score,
                "band": risk.band,
            });
            let days = (now - review_at).num_days();
            let message = format!(
                "Risk \"{}\" is {} day(s) past its review date ({}).",
                risk.title,
                days,
                review_at.to_rfc3339()
            );
            if self
                .record_alert(&risk, "review_overdue", message.clone(), figures)
                .is_some()
            {
                raised += 1;
                self.emit(
                    "risk.review_overdue",
                    json!({
                        "risk_id": risk.id,
                        "company_id": risk.company_id,
                        "title": risk.title,
                        "review_date": review_at,
                        "days_overdue": days,
                        "score": risk.score,
                        "band": risk.band,
                        "owner_id": risk.owner_id,
                    }),
                );
                self.notify(
                    &risk.owner_id,
                    format!("Risk review overdue: {}", risk.title),
                    message,
                );
            }
        }
        raised
    }

    pub(crate) fn get_risk(&self, id: &str) -> UnifiedResult<Risk> {
        self.risks
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Risk not found"))
    }

    pub(crate) fn get_plan(&self, id: &str) -> UnifiedResult<MitigationPlan> {
        self.plans
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Mitigation plan not found"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn risk(status: &str, next_review_at: Option<DateTime<Utc>>) -> Risk {
        let score = score_for(5, 5);
        Risk {
            id: new_id(),
            company_id: None,
            title: "Data breach".to_string(),
            description: String::new(),
            category: "security".to_string(),
            likelihood: 5,
            impact: 5,
            score,
            band: band_for(score).to_string(),
            owner_id: "owner-1".to_string(),
            status: status.to_string(),
            next_review_at,
            alert_threshold: 15,
            created_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn threshold_alerts_deduplicate_and_skip_closed_risks() {
        let state = RiskState::new();
        let open = risk("identified", None);
        state.risks.insert(open.id.clone(), open.clone());
        // The first crossing raises; the repeat is deduplicated.
        assert!(state.check_threshold(&open));
        assert_eq!(state.alerts.len(), 1);
        assert!(!state.check_threshold(&open));
        assert_eq!(state.alerts.len(), 1);
        // A below-threshold risk never alerts.
        let mut low = open.clone();
        low.id = new_id();
        low.likelihood = 1;
        low.impact = 1;
        low.score = score_for(1, 1);
        low.band = band_for(low.score).to_string();
        assert!(!state.check_threshold(&low));
        assert_eq!(state.alerts.len(), 1);
        // Editing a closed (or accepted) high-score risk raises no
        // fresh alert or owner notification.
        let mut closed = open.clone();
        closed.id = new_id();
        closed.status = "closed".to_string();
        assert!(!state.check_threshold(&closed));
        let mut accepted = open.clone();
        accepted.id = new_id();
        accepted.status = "accepted".to_string();
        assert!(!state.check_threshold(&accepted));
        assert_eq!(state.alerts.len(), 1);
    }

    #[test]
    fn review_check_skips_closed_and_future_risks_and_fires_once_per_date() {
        let state = RiskState::new();
        let past = Utc::now() - chrono::Duration::days(2);
        let future = Utc::now() + chrono::Duration::days(2);
        let items = [
            risk("identified", Some(past)),
            risk("closed", Some(past)),
            risk("accepted", Some(past)),
            risk("identified", Some(future)),
            risk("identified", None),
        ];
        for item in items {
            state.risks.insert(item.id.clone(), item);
        }
        // Only the open risk with a past review date alerts.
        assert_eq!(state.run_review_check(None), 1);
        // Once per review date: a second run raises nothing new.
        assert_eq!(state.run_review_check(None), 0);
        assert_eq!(state.alerts.len(), 1);
    }
}
