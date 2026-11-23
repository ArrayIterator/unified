// admissions module — Shared state.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{KEY_APPLICATION_STATUSES, KEY_CHECKLIST_STATUSES, KEY_DECISIONS, KEY_FEE_CURRENCY, KEY_PERIOD_STATUSES, KEY_RESULT_VISIBLE, KEY_SCORE_SCALE_MAX, KEY_WAITLIST_ORDER, KEY_WAITLIST_STATUSES, SEED_APPLICATION_STATUSES, SEED_CHECKLIST_STATUSES, SEED_DECISIONS, SEED_FEE_CURRENCY, SEED_PERIOD_STATUSES, SEED_RESULT_VISIBLE, SEED_SCORE_SCALE_MAX, SEED_WAITLIST_ORDER, SEED_WAITLIST_STATUSES, WAITLIST_ORDERS};
use super::types::{AdmissionPeriod, AdmissionResult, AdmissionSetting, Application, ApplicationScore, ChecklistItem, PeriodQuota, RequiredDocument, ScoreBreakdown, ScoreTotal, StageDefinition, WaitlistEntry};
use super::helpers::{caller, new_id, now, round4, today};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct AdmissionsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) periods: DashMap<String, AdmissionPeriod>,
    pub(crate) quotas: DashMap<String, PeriodQuota>,
    pub(crate) stages: DashMap<String, StageDefinition>,
    pub(crate) required_documents: DashMap<String, RequiredDocument>,
    pub(crate) applications: DashMap<String, Application>,
    pub(crate) scores: DashMap<String, ApplicationScore>,
    pub(crate) checklist: DashMap<String, ChecklistItem>,
    pub(crate) waitlist: DashMap<String, WaitlistEntry>,
    pub(crate) results: DashMap<String, AdmissionResult>,
    pub(crate) settings: DashMap<String, AdmissionSetting>,
}

impl AdmissionsState {
    pub(crate) fn new() -> Self {
        let state = Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            periods: DashMap::new(),
            quotas: DashMap::new(),
            stages: DashMap::new(),
            required_documents: DashMap::new(),
            applications: DashMap::new(),
            scores: DashMap::new(),
            checklist: DashMap::new(),
            waitlist: DashMap::new(),
            results: DashMap::new(),
            settings: DashMap::new(),
        };
        state.seed_settings();
        state
    }

    /// Seed the configurable defaults. Seeding runs once per process
    /// start into the in-memory store; an administrator's edits win
    /// afterwards (a seeded key is never overwritten here).
    pub(crate) fn seed_settings(&self) {
        let seeds: &[(&str, &str, &str)] = &[
            (
                KEY_PERIOD_STATUSES,
                SEED_PERIOD_STATUSES,
                "Allowed admission-period statuses, comma-separated.",
            ),
            (
                KEY_APPLICATION_STATUSES,
                SEED_APPLICATION_STATUSES,
                "Allowed application statuses, comma-separated.",
            ),
            (
                KEY_CHECKLIST_STATUSES,
                SEED_CHECKLIST_STATUSES,
                "Allowed document-checklist item statuses, comma-separated.",
            ),
            (
                KEY_WAITLIST_STATUSES,
                SEED_WAITLIST_STATUSES,
                "Allowed waitlist entry statuses, comma-separated.",
            ),
            (
                KEY_DECISIONS,
                SEED_DECISIONS,
                "Allowed result decisions, comma-separated.",
            ),
            (
                KEY_FEE_CURRENCY,
                SEED_FEE_CURRENCY,
                "Default 3-letter currency code for registration fees.",
            ),
            (
                KEY_SCORE_SCALE_MAX,
                SEED_SCORE_SCALE_MAX,
                "Maximum score on the scoring scale (scores run 0..=max).",
            ),
            (
                KEY_WAITLIST_ORDER,
                SEED_WAITLIST_ORDER,
                "Waitlist ordering policy: score_desc_submitted_asc or submitted_asc.",
            ),
            (
                KEY_RESULT_VISIBLE,
                SEED_RESULT_VISIBLE,
                "Whether applicants may see their own result once published (true/false).",
            ),
        ];
        for (key, value, description) in seeds {
            if !self.settings.contains_key(*key) {
                self.settings.insert(
                    key.to_string(),
                    AdmissionSetting {
                        key: key.to_string(),
                        value: value.to_string(),
                        description: description.to_string(),
                        updated_at: now(),
                    },
                );
            }
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Read a setting value, falling back to `default` when the key
    /// is absent or blank.
    pub(crate) fn setting(&self, key: &str, default: &str) -> String {
        self.settings
            .get(key)
            .map(|entry| entry.value().value.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    /// The allowed values for a vocabulary setting: the configured
    /// comma-separated list, or `defaults` when the setting is
    /// missing or empty.
    pub(crate) fn vocabulary(&self, key: &str, defaults: &[&str]) -> Vec<String> {
        let raw = self.setting(key, "");
        if raw.trim().is_empty() {
            return defaults.iter().map(|value| value.to_string()).collect();
        }
        raw.split(',')
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect()
    }

    /// Maximum score on the configured scale (default 100).
    pub(crate) fn score_scale_max(&self) -> f64 {
        self.setting(KEY_SCORE_SCALE_MAX, SEED_SCORE_SCALE_MAX)
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value > 0.0 && *value <= 1000.0)
            .unwrap_or(100.0)
    }

    /// Waitlist ordering policy from settings, falling back to the
    /// seed policy when the configured value is unknown.
    pub(crate) fn waitlist_order(&self) -> String {
        let configured = self.setting(KEY_WAITLIST_ORDER, SEED_WAITLIST_ORDER);
        if WAITLIST_ORDERS.contains(&configured.trim()) {
            configured.trim().to_string()
        } else {
            SEED_WAITLIST_ORDER.to_string()
        }
    }

    /// True when applicants may see their own published result.
    pub(crate) fn results_visible_to_applicants(&self) -> bool {
        self.setting(KEY_RESULT_VISIBLE, SEED_RESULT_VISIBLE)
            .trim()
            .eq_ignore_ascii_case("true")
    }

    pub(crate) fn period(&self, period_id: &str) -> UnifiedResult<AdmissionPeriod> {
        self.periods
            .get(period_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Admission period not found"))
    }

    pub(crate) fn application(&self, application_id: &str) -> UnifiedResult<Application> {
        self.applications
            .get(application_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Application not found"))
    }

    /// True when the period accepts submissions today: status `open`
    /// and today inside [open_date, close_date].
    pub(crate) fn period_accepts_submissions(&self, period: &AdmissionPeriod) -> bool {
        let today = today();
        period.status == "open" && period.open_date <= today && today <= period.close_date
    }

    /// Stages of one period, in definition order.
    pub(crate) fn stages_of(&self, period_id: &str) -> Vec<StageDefinition> {
        let mut stages: Vec<StageDefinition> = self
            .stages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|stage| stage.period_id == period_id)
            .collect();
        stages.sort_by(|a, b| {
            a.position
                .cmp(&b.position)
                .then_with(|| a.stage_key.cmp(&b.stage_key))
        });
        stages
    }

    /// Required documents of one period, in creation order.
    pub(crate) fn required_documents_of(&self, period_id: &str) -> Vec<RequiredDocument> {
        let mut documents: Vec<RequiredDocument> = self
            .required_documents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|document| document.period_id == period_id)
            .collect();
        documents.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        documents
    }

    /// Scores of one application, ordered by stage position.
    pub(crate) fn scores_of(&self, application_id: &str) -> Vec<ApplicationScore> {
        let mut scores: Vec<ApplicationScore> = self
            .scores
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|score| score.application_id == application_id)
            .collect();
        scores.sort_by(|a, b| {
            let position_a = self
                .stages
                .get(&a.stage_id)
                .map(|stage| stage.position)
                .unwrap_or(u32::MAX);
            let position_b = self
                .stages
                .get(&b.stage_id)
                .map(|stage| stage.position)
                .unwrap_or(u32::MAX);
            position_a
                .cmp(&position_b)
                .then_with(|| a.stage_key.cmp(&b.stage_key))
        });
        scores
    }

    /// Checklist items of one application, in creation order.
    pub(crate) fn checklist_of(&self, application_id: &str) -> Vec<ChecklistItem> {
        let mut items: Vec<ChecklistItem> = self
            .checklist
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.application_id == application_id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// Waiting waitlist entries of one period, in promotion order:
    /// by the configured policy — highest weighted total first, then
    /// earliest submission — or by submission time only.
    pub(crate) fn waiting_entries(&self, period_id: &str) -> Vec<WaitlistEntry> {
        let mut entries: Vec<WaitlistEntry> = self
            .waitlist
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.period_id == period_id && entry.status == "waiting")
            .collect();
        let order = self.waitlist_order();
        entries.sort_by(|a, b| {
            if order == "score_desc_submitted_asc" {
                let score_a = self
                    .applications
                    .get(&a.application_id)
                    .and_then(|application| application.total_score)
                    .unwrap_or(0.0);
                let score_b = self
                    .applications
                    .get(&b.application_id)
                    .and_then(|application| application.total_score)
                    .unwrap_or(0.0);
                score_b
                    .partial_cmp(&score_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.created_at.cmp(&b.created_at))
            } else {
                a.created_at.cmp(&b.created_at)
            }
        });
        entries
    }

    /// Number of accepted (not yet enrolled or withdrawn) seats taken
    /// for one (`program_id`, `level`) pair inside a period.
    /// `exclude_id` skips one application (the one being decided),
    /// so re-accepting an already-accepted application does not count
    /// it against the quota.
    pub(crate) fn accepted_count(
        &self,
        period_id: &str,
        program_id: &str,
        level: &str,
        exclude_id: Option<&str>,
    ) -> u32 {
        self.applications
            .iter()
            .filter(|entry| {
                let application = entry.value();
                if let Some(excluded) = exclude_id {
                    if application.id == excluded {
                        return false;
                    }
                }
                application.period_id == period_id
                    && application.program_id == program_id
                    && application.level == level
                    && matches!(application.status.as_str(), "accepted" | "enrolled")
            })
            .count() as u32
    }

    /// Seats configured for one (`program_id`, `level`) pair, if a
    /// quota row exists. `None` means no quota row for the pair.
    pub(crate) fn quota_seats(&self, period_id: &str, program_id: &str, level: &str) -> Option<u32> {
        self.quotas
            .iter()
            .find(|entry| {
                let quota = entry.value();
                quota.period_id == period_id
                    && quota.program_id == program_id
                    && quota.level == level
            })
            .map(|entry| entry.value().seats)
    }

    /// True when another accepted application fits the quota for the
    /// application's (`program_id`, `level`) pair. When the period
    /// has no quota rows at all the pair is unlimited; when quota
    /// rows exist but none covers the pair, acceptance is refused.
    /// The application itself is excluded from the count, so
    /// re-accepting an already-accepted application does not fail at
    /// a full quota.
    pub(crate) fn seat_available(&self, application: &Application) -> UnifiedResult<bool> {
        let any_quota = self
            .quotas
            .iter()
            .any(|entry| entry.value().period_id == application.period_id);
        if !any_quota {
            return Ok(true);
        }
        match self.quota_seats(
            &application.period_id,
            &application.program_id,
            &application.level,
        ) {
            Some(seats) => Ok(self.accepted_count(
                &application.period_id,
                &application.program_id,
                &application.level,
                Some(application.id.as_str()),
            ) < seats),
            None => Err(UnifiedError::conflict(format!(
                "No quota is configured for program \"{}\" level \"{}\" in this period",
                application.program_id, application.level
            ))),
        }
    }

    /// Weighted total for one application on the configured scale,
    /// plus the per-stage breakdown and the pass state of every
    /// required stage. Stages without a score are skipped; the total
    /// is the weight-normalised mean of the scored stages, so a
    /// partially scored application already has a meaningful total.
    pub(crate) fn compute_total(&self, application_id: &str) -> ScoreTotal {
        let application = self
            .applications
            .get(application_id)
            .map(|entry| entry.value().clone());
        let period_id = application
            .as_ref()
            .map(|value| value.period_id.clone())
            .unwrap_or_default();
        let scale_max = self.score_scale_max();
        let stages = self.stages_of(&period_id);
        let scores = self.scores_of(application_id);
        let mut breakdown: Vec<ScoreBreakdown> = Vec::new();
        let mut weighted_sum = 0.0;
        let mut weight_sum = 0.0;
        let mut has_required_stage = false;
        let mut all_required_passed = true;
        for stage in &stages {
            let score = scores
                .iter()
                .find(|entry| entry.stage_id == stage.id)
                .map(|entry| entry.score);
            let passed = match (score, stage.pass_score) {
                (Some(value), Some(threshold)) => Some(value >= threshold),
                _ => None,
            };
            if stage.required {
                has_required_stage = true;
                match passed {
                    Some(true) => {}
                    _ => all_required_passed = false,
                }
            }
            if let Some(value) = score {
                if stage.weight > 0.0 {
                    weighted_sum += value * stage.weight;
                    weight_sum += stage.weight;
                }
            }
            breakdown.push(ScoreBreakdown {
                stage_id: stage.id.clone(),
                stage_key: stage.stage_key.clone(),
                weight: stage.weight,
                score,
                passed,
            });
        }
        let total = if weight_sum > 0.0 {
            Some(round4(weighted_sum / weight_sum))
        } else {
            None
        };
        ScoreTotal {
            application_id: application_id.to_string(),
            total_score: total,
            scale_max,
            stages: breakdown,
            // With no required stages there is nothing to pass, so
            // the gate must stay closed rather than vacuously true.
            all_required_stages_passed: has_required_stage && all_required_passed,
        }
    }

    /// Recompute and store the weighted total on an application.
    /// Returns the fresh total.
    pub(crate) fn refresh_total(&self, application_id: &str) -> Option<f64> {
        let total = self.compute_total(application_id).total_score;
        if let Some(mut application) = self.applications.get_mut(application_id) {
            application.total_score = total;
            application.updated_at = now();
        }
        total
    }

    /// Create checklist items for an application from the period's
    /// current required documents. Existing items are left alone, so
    /// re-running this is safe.
    pub(crate) fn ensure_checklist(&self, application: &Application) {
        for document in self.required_documents_of(&application.period_id) {
            let exists = self.checklist.iter().any(|entry| {
                let item = entry.value();
                item.application_id == application.id
                    && item.required_document_id == document.id
            });
            if exists {
                continue;
            }
            let item = ChecklistItem {
                id: new_id(),
                application_id: application.id.clone(),
                period_id: application.period_id.clone(),
                required_document_id: document.id.clone(),
                document_key: document.document_key.clone(),
                title: document.title.clone(),
                required: document.required,
                status: "pending".to_string(),
                document_id: None,
                verified_by: None,
                verified_at: None,
                rejection_reason: None,
                created_at: now(),
                updated_at: now(),
            };
            self.checklist.insert(item.id.clone(), item);
        }
    }

    /// True when every required checklist item of an application is
    /// verified.
    pub(crate) fn checklist_complete(&self, application_id: &str) -> bool {
        self.checklist_of(application_id)
            .iter()
            .filter(|item| item.required)
            .all(|item| item.status == "verified")
    }

    /// Promote the first waiting entry of a period when a seat is
    /// free for its (`program_id`, `level`) pair. Returns the
    /// promoted application, if any. The caller owns the surrounding
    /// decision (a seat freed by withdrawal or rejection). A quota
    /// error is propagated, not treated as "no seat": a missing
    /// quota row is a configuration problem the caller must see.
    /// A candidate whose published result locks its decision is
    /// skipped (and logged) so the next candidate can be tried.
    pub(crate) fn promote_top_waitlisted(
        &self,
        period_id: &str,
    ) -> UnifiedResult<Option<Application>> {
        for entry in self.waiting_entries(period_id) {
            let Some(application) = self
                .applications
                .get(&entry.application_id)
                .map(|value| value.value().clone())
            else {
                continue;
            };
            if application.status != "waitlisted" {
                continue;
            }
            if !self.seat_available(&application)? {
                continue;
            }
            match self.promote_waitlist_entry(&entry, &application) {
                Ok(promoted) => return Ok(Some(promoted)),
                Err(error) => {
                    log::warn!(
                        "Skipping waitlist promotion for application {}: {}",
                        application.id,
                        error.message
                    );
                    continue;
                }
            }
        }
        Ok(None)
    }

    /// Apply a promotion: entry `promoted`, application `accepted`,
    /// result decision `accepted`, events and notification emitted.
    /// Fails (and rolls the promotion back) when the result cannot
    /// be updated, for example because it is published.
    pub(crate) fn promote_waitlist_entry(
        &self,
        entry: &WaitlistEntry,
        application: &Application,
    ) -> UnifiedResult<Application> {
        let mut promoted = application.clone();
        if let Some(mut waitlist_entry) = self.waitlist.get_mut(&entry.id) {
            waitlist_entry.status = "promoted".to_string();
            waitlist_entry.promoted_at = Some(now());
            waitlist_entry.updated_at = now();
        }
        if let Some(mut record) = self.applications.get_mut(&application.id) {
            record.status = "accepted".to_string();
            record.updated_at = now();
            promoted = record.clone();
        }
        if let Err(error) = self.upsert_result(&promoted, "accepted", "system", None) {
            if let Some(mut waitlist_entry) = self.waitlist.get_mut(&entry.id) {
                waitlist_entry.status = "waiting".to_string();
                waitlist_entry.promoted_at = None;
                waitlist_entry.updated_at = now();
            }
            if let Some(mut record) = self.applications.get_mut(&application.id) {
                record.status = "waitlisted".to_string();
                record.updated_at = now();
            }
            return Err(error);
        }
        self.emit(
            "admissions.waitlist_promoted",
            json!({
                "application_id": promoted.id,
                "period_id": promoted.period_id,
                "waitlist_entry_id": entry.id,
            }),
        );
        self.emit(
            "notification.requested",
            json!({
                "user_id": promoted.applicant_user_id,
                "title": "A seat opened for your application",
                "body": "Your application was promoted from the waitlist and is now accepted.",
                "kind": "admissions.waitlist_promoted",
            }),
        );
        Ok(promoted)
    }

    /// Create or update the result record for an application.
    /// Refuses to change the decision of a published result: the
    /// result must be unpublished first. Re-recording the same
    /// decision on a published result is allowed (idempotent).
    pub(crate) fn upsert_result(
        &self,
        application: &Application,
        decision: &str,
        decided_by: &str,
        note: Option<String>,
    ) -> UnifiedResult<AdmissionResult> {
        if let Some(mut existing) = self.results.iter_mut().find(|entry| {
            entry.value().application_id == application.id
        }) {
            if existing.published && existing.decision != decision {
                return Err(UnifiedError::conflict(
                    "Result is published; unpublish it before changing the decision",
                ));
            }
            existing.decision = decision.to_string();
            existing.decided_by = decided_by.to_string();
            if let Some(note) = note {
                existing.note = Some(note);
            }
            existing.updated_at = now();
            return Ok(existing.clone());
        }
        let result = AdmissionResult {
            id: new_id(),
            period_id: application.period_id.clone(),
            application_id: application.id.clone(),
            decision: decision.to_string(),
            published: false,
            published_at: None,
            decided_by: decided_by.to_string(),
            note,
            created_at: now(),
            updated_at: now(),
        };
        self.results.insert(result.id.clone(), result.clone());
        Ok(result)
    }

    /// Ask the Notifications module to tell an applicant about a
    /// published result.
    pub(crate) fn notify_result(&self, application: &Application, decision: &str) {
        let (title, body) = match decision {
            "accepted" => (
                "Your application was accepted",
                "Congratulations — your application was accepted. Follow the enrolment instructions to complete your registration.",
            ),
            "rejected" => (
                "Your application result is available",
                "Your application was not accepted this time. Thank you for applying.",
            ),
            _ => (
                "Your application is on the waitlist",
                "Your application is on the waitlist. You will be notified if a seat opens.",
            ),
        };
        self.emit(
            "notification.requested",
            json!({
                "user_id": application.applicant_user_id,
                "title": title,
                "body": body,
                "kind": "admissions.result_published",
            }),
        );
    }

    /// Request a notification through the notifications module.
    pub(crate) fn request_notification(&self, user_id: &str, title: &str, body: &str, kind: &str) {
        self.emit(
            "notification.requested",
            json!({
                "user_id": user_id,
                "title": title,
                "body": body,
                "kind": kind,
            }),
        );
    }
}

pub struct AdmissionsModule {
    pub(crate) state: Arc<AdmissionsState>,
}

impl AdmissionsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AdmissionsState::new()),
        }
    }
}

impl Default for AdmissionsModule {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn period(id: &str) -> AdmissionPeriod {
        AdmissionPeriod {
            id: id.to_string(),
            academic_year: "2026/2027".to_string(),
            term: "1".to_string(),
            name: "Test period".to_string(),
            open_date: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            close_date: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            status: "open".to_string(),
            registration_fee_amount: None,
            fee_currency: "USD".to_string(),
            registration_form_id: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn application(id: &str, period_id: &str, status: &str) -> Application {
        Application {
            id: id.to_string(),
            period_id: period_id.to_string(),
            applicant_user_id: "user-1".to_string(),
            full_name: "Test Applicant".to_string(),
            email: "test@example.com".to_string(),
            phone: None,
            program_id: "prog-1".to_string(),
            level: "undergraduate".to_string(),
            form_submission_id: None,
            status: status.to_string(),
            total_score: None,
            fee_amount: None,
            fee_currency: "USD".to_string(),
            fee_invoice_status: "none".to_string(),
            fee_invoice_id: None,
            student_id: None,
            source: "manual".to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    fn quota(period_id: &str, program_id: &str, seats: u32) -> PeriodQuota {
        PeriodQuota {
            id: new_id(),
            period_id: period_id.to_string(),
            program_id: program_id.to_string(),
            level: "undergraduate".to_string(),
            seats,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn stage(id: &str, period_id: &str, required: bool, pass_score: Option<f64>) -> StageDefinition {
        StageDefinition {
            id: id.to_string(),
            period_id: period_id.to_string(),
            stage_key: id.to_string(),
            name: "Test stage".to_string(),
            position: 1,
            weight: 1.0,
            pass_score,
            required,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn score(application_id: &str, period_id: &str, stage_id: &str, value: f64) -> ApplicationScore {
        ApplicationScore {
            id: new_id(),
            application_id: application_id.to_string(),
            period_id: period_id.to_string(),
            stage_id: stage_id.to_string(),
            stage_key: stage_id.to_string(),
            score: value,
            scored_by: "staff-1".to_string(),
            note: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn waitlist_entry(id: &str, period_id: &str, application_id: &str) -> WaitlistEntry {
        WaitlistEntry {
            id: id.to_string(),
            period_id: period_id.to_string(),
            application_id: application_id.to_string(),
            position: 1,
            status: "waiting".to_string(),
            promoted_at: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn seat_available_excludes_the_application_itself() {
        let state = AdmissionsState::new();
        state.periods.insert("p1".to_string(), period("p1"));
        state.quotas.insert("q1".to_string(), quota("p1", "prog-1", 1));
        let accepted = application("app-accepted", "p1", "accepted");
        let submitted = application("app-submitted", "p1", "submitted");
        state.applications.insert(accepted.id.clone(), accepted.clone());
        state.applications.insert(submitted.id.clone(), submitted.clone());
        // The single seat is taken, but re-accepting the accepted
        // application itself must still fit: it is excluded from the
        // count, so the quota looks at the other applications only.
        assert!(state.seat_available(&accepted).unwrap());
        assert!(!state.seat_available(&submitted).unwrap());
        assert_eq!(
            state.accepted_count("p1", "prog-1", "undergraduate", None),
            1
        );
        assert_eq!(
            state.accepted_count("p1", "prog-1", "undergraduate", Some("app-accepted")),
            0
        );
    }

    #[test]
    fn compute_total_requires_a_required_stage_to_pass() {
        let state = AdmissionsState::new();
        state.periods.insert("p1".to_string(), period("p1"));
        let app = application("app-1", "p1", "submitted");
        state.applications.insert(app.id.clone(), app.clone());
        // No stages at all: the gate must stay closed.
        assert!(!state.compute_total("app-1").all_required_stages_passed);
        // One optional stage, even passed, does not open the gate.
        let optional = stage("stage-optional", "p1", false, Some(50.0));
        state.stages.insert(optional.id.clone(), optional);
        let scored = score("app-1", "p1", "stage-optional", 90.0);
        state.scores.insert(scored.id.clone(), scored);
        assert!(!state.compute_total("app-1").all_required_stages_passed);
        // A required stage with a passing score opens it...
        let required = stage("stage-required", "p1", true, Some(50.0));
        state.stages.insert(required.id.clone(), required);
        let passing = score("app-1", "p1", "stage-required", 80.0);
        let passing_id = passing.id.clone();
        state.scores.insert(passing.id.clone(), passing);
        assert!(state.compute_total("app-1").all_required_stages_passed);
        // ...and a failing score closes it again.
        state.scores.get_mut(&passing_id).unwrap().score = 40.0;
        assert!(!state.compute_total("app-1").all_required_stages_passed);
    }

    #[test]
    fn upsert_result_refuses_to_change_a_published_decision() {
        let state = AdmissionsState::new();
        let app = application("app-1", "p1", "accepted");
        let created = state
            .upsert_result(&app, "accepted", "staff-1", None)
            .unwrap();
        assert_eq!(created.decision, "accepted");
        if let Some(mut result) = state.results.get_mut(&created.id) {
            result.published = true;
            result.published_at = Some(now());
        }
        assert!(state
            .upsert_result(&app, "rejected", "staff-1", None)
            .is_err());
        assert_eq!(
            state.results.get(&created.id).unwrap().decision,
            "accepted"
        );
        // Re-recording the same decision stays allowed.
        assert!(state
            .upsert_result(&app, "accepted", "staff-1", None)
            .is_ok());
    }

    #[test]
    fn promote_top_waitlisted_propagates_quota_errors() {
        let state = AdmissionsState::new();
        state.periods.insert("p1".to_string(), period("p1"));
        // A quota row exists for another program, so the candidate's
        // pair has no quota: seat_available errors instead of
        // answering "no seat".
        state.quotas.insert("q1".to_string(), quota("p1", "prog-other", 1));
        let app = application("app-1", "p1", "waitlisted");
        state.applications.insert(app.id.clone(), app.clone());
        let entry = waitlist_entry("entry-1", "p1", "app-1");
        state.waitlist.insert(entry.id.clone(), entry);
        assert!(state.promote_top_waitlisted("p1").is_err());
        assert_eq!(state.application("app-1").unwrap().status, "waitlisted");
    }

    #[test]
    fn promote_top_waitlisted_promotes_when_a_seat_is_free() {
        let state = AdmissionsState::new();
        state.periods.insert("p1".to_string(), period("p1"));
        state.quotas.insert("q1".to_string(), quota("p1", "prog-1", 1));
        let app = application("app-1", "p1", "waitlisted");
        state.applications.insert(app.id.clone(), app.clone());
        let entry = waitlist_entry("entry-1", "p1", "app-1");
        state.waitlist.insert(entry.id.clone(), entry);
        let promoted = state.promote_top_waitlisted("p1").unwrap();
        assert!(promoted.is_some());
        assert_eq!(promoted.unwrap().status, "accepted");
        assert_eq!(state.application("app-1").unwrap().status, "accepted");
        assert_eq!(
            state.waitlist.get("entry-1").unwrap().status,
            "promoted"
        );
    }

    #[test]
    fn promote_top_waitlisted_skips_a_candidate_with_a_published_result() {
        let state = AdmissionsState::new();
        state.periods.insert("p1".to_string(), period("p1"));
        state.quotas.insert("q1".to_string(), quota("p1", "prog-1", 1));
        let app = application("app-1", "p1", "waitlisted");
        state.applications.insert(app.id.clone(), app.clone());
        // Publish a "waitlisted" decision for the candidate:
        // promoting it would have to rewrite that decision.
        let result = state
            .upsert_result(&app, "waitlisted", "staff-1", None)
            .unwrap();
        if let Some(mut stored) = state.results.get_mut(&result.id) {
            stored.published = true;
        }
        let entry = waitlist_entry("entry-1", "p1", "app-1");
        state.waitlist.insert(entry.id.clone(), entry);
        assert!(state.promote_top_waitlisted("p1").unwrap().is_none());
        assert_eq!(state.application("app-1").unwrap().status, "waitlisted");
        assert_eq!(
            state.waitlist.get("entry-1").unwrap().status,
            "waiting"
        );
        assert_eq!(
            state.results.get(&result.id).unwrap().decision,
            "waitlisted"
        );
    }
}
