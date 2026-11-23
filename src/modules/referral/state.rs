//! In-memory referral store.

use super::types::{
    AntiGamingRule, AuditRecord, JobEligibility, KnownEmployee, PipelineStage, Referral,
    ReferralLink, ReferralStageHistory, Reward, RewardRule,
};
use dashmap::DashMap;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the referral module. The hook bus is
/// attached in `init` (the bus lives in `AppContext`);
/// handlers tolerate a missing bus by skipping event emission.
/// Durable rows for these types are defined by migration
/// `2026-11-22-000120_referral`; until that migration is executed,
/// referral data does not survive a restart.
#[derive(Clone)]
pub struct ReferralState {
    pub eligibility: Arc<DashMap<Uuid, JobEligibility>>,
    pub links: Arc<DashMap<Uuid, ReferralLink>>,
    pub stages: Arc<DashMap<Uuid, PipelineStage>>,
    pub reward_rules: Arc<DashMap<Uuid, RewardRule>>,
    pub referrals: Arc<DashMap<Uuid, Referral>>,
    pub stage_history: Arc<DashMap<Uuid, ReferralStageHistory>>,
    pub rewards: Arc<DashMap<Uuid, Reward>>,
    pub anti_gaming_rules: Arc<DashMap<Uuid, AntiGamingRule>>,
    pub known_employees: Arc<DashMap<String, KnownEmployee>>,
    pub audit: Arc<DashMap<Uuid, AuditRecord>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ReferralState {
    fn default() -> Self {
        Self {
            eligibility: Arc::new(DashMap::new()),
            links: Arc::new(DashMap::new()),
            stages: Arc::new(DashMap::new()),
            reward_rules: Arc::new(DashMap::new()),
            referrals: Arc::new(DashMap::new()),
            stage_history: Arc::new(DashMap::new()),
            rewards: Arc::new(DashMap::new()),
            anti_gaming_rules: Arc::new(DashMap::new()),
            known_employees: Arc::new(DashMap::new()),
            audit: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ReferralState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Pipeline stages of one tenant, in pipeline order.
    pub(crate) fn stages_of(&self, tenant_id: &str) -> Vec<PipelineStage> {
        let mut items: Vec<PipelineStage> = self
            .stages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|stage| stage.tenant_id == tenant_id)
            .collect();
        items.sort_by_key(|stage| stage.sort_order);
        items
    }

    /// Reward rules of one tenant, newest first.
    pub(crate) fn reward_rules_of(&self, tenant_id: &str) -> Vec<RewardRule> {
        let mut items: Vec<RewardRule> = self
            .reward_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// The tenant's default reward rule, when one is set and
    /// active.
    pub(crate) fn default_reward_rule(&self, tenant_id: &str) -> Option<RewardRule> {
        self.reward_rules_of(tenant_id)
            .into_iter()
            .find(|rule| rule.is_default && rule.active)
    }

    /// The reward rule that applies to one job: the job's
    /// per-job override when one is set and active, otherwise
    /// the tenant's default rule.
    pub(crate) fn reward_rule_for_job(&self, tenant_id: &str, job_id: Uuid) -> Option<RewardRule> {
        let eligibility = self
            .eligibility
            .iter()
            .map(|entry| entry.value().clone())
            .find(|row| row.tenant_id == tenant_id && row.job_id == job_id);
        if let Some(row) = eligibility {
            if let Some(rule_id) = row.reward_rule_id {
                if let Some(rule) = self.reward_rules.get(&rule_id).map(|entry| entry.clone()) {
                    if rule.tenant_id == tenant_id && rule.active {
                        return Some(rule);
                    }
                }
            }
        }
        self.default_reward_rule(tenant_id)
    }

    /// Referrals of one tenant, newest first.
    pub(crate) fn referrals_of(&self, tenant_id: &str) -> Vec<Referral> {
        let mut items: Vec<Referral> = self
            .referrals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|referral| referral.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// The referral for one (candidate email, job) pair in a
    /// tenant, when one exists. Emails are stored normalized,
    /// so the lookup normalizes the input the same way.
    pub(crate) fn referral_for_email_job(
        &self,
        tenant_id: &str,
        candidate_email: &str,
        job_id: Uuid,
    ) -> Option<Referral> {
        let wanted = super::types::normalize_email(candidate_email);
        self.referrals
            .iter()
            .map(|entry| entry.value().clone())
            .find(|referral| {
                referral.tenant_id == tenant_id
                    && referral.job_id == job_id
                    && super::types::normalize_email(&referral.candidate_email) == wanted
            })
    }

    /// The latest referral of one candidate email in a
    /// tenant, across all jobs (for the same-candidate
    /// cooldown, which spans jobs).
    pub(crate) fn latest_referral_for_email(
        &self,
        tenant_id: &str,
        candidate_email: &str,
    ) -> Option<Referral> {
        let wanted = super::types::normalize_email(candidate_email);
        let mut items: Vec<Referral> = self
            .referrals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|referral| {
                referral.tenant_id == tenant_id
                    && super::types::normalize_email(&referral.candidate_email) == wanted
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items.into_iter().next()
    }

    /// Referrals of one referrer in a tenant, newest first.
    pub(crate) fn referrals_of_referrer(
        &self,
        tenant_id: &str,
        referrer_user_id: &str,
    ) -> Vec<Referral> {
        let mut items: Vec<Referral> = self
            .referrals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|referral| {
                referral.tenant_id == tenant_id && referral.referrer_user_id == referrer_user_id
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Stage history of one referral, oldest first.
    pub(crate) fn history_of(&self, referral_id: Uuid) -> Vec<ReferralStageHistory> {
        let mut items: Vec<ReferralStageHistory> = self
            .stage_history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.referral_id == referral_id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// The moment a referral first entered one stage, from
    /// its history (`None` when it never entered it). Used
    /// for reward retention math.
    pub(crate) fn first_entered_stage_at(
        &self,
        referral_id: Uuid,
        stage_id: Uuid,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        self.history_of(referral_id)
            .into_iter()
            .find(|row| row.to_stage_id == stage_id)
            .map(|row| row.created_at)
    }

    /// Rewards of one tenant, newest first.
    pub(crate) fn rewards_of(&self, tenant_id: &str) -> Vec<Reward> {
        let mut items: Vec<Reward> = self
            .rewards
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reward| reward.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// The reward of one referral under one rule, when one
    /// exists. A referral earns at most one reward per rule.
    pub(crate) fn reward_for(&self, referral_id: Uuid, rule_id: Uuid) -> Option<Reward> {
        self.rewards
            .iter()
            .map(|entry| entry.value().clone())
            .find(|reward| reward.referral_id == referral_id && reward.rule_id == rule_id)
    }

    /// Links of one tenant, newest first.
    pub(crate) fn links_of(&self, tenant_id: &str) -> Vec<ReferralLink> {
        let mut items: Vec<ReferralLink> = self
            .links
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|link| link.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// The active link of one (employee, job) pair in a
    /// tenant, when one exists. At most one is active.
    pub(crate) fn link_for_employee_job(
        &self,
        tenant_id: &str,
        referrer_user_id: &str,
        job_id: Uuid,
    ) -> Option<ReferralLink> {
        self.links
            .iter()
            .map(|entry| entry.value().clone())
            .find(|link| {
                link.tenant_id == tenant_id
                    && link.referrer_user_id == referrer_user_id
                    && link.job_id == job_id
                    && link.active
            })
    }

    /// A link by its token hash. The lookup is global across
    /// tenants: the token is unguessable and the public click
    /// endpoint carries no tenant context.
    pub(crate) fn link_for_token_hash(&self, token_hash: &str) -> Option<ReferralLink> {
        self.links
            .iter()
            .map(|entry| entry.value().clone())
            .find(|link| link.token_hash == token_hash)
    }

    /// Anti-gaming rules of one tenant, newest first.
    pub(crate) fn anti_gaming_rules_of(&self, tenant_id: &str) -> Vec<AntiGamingRule> {
        let mut items: Vec<AntiGamingRule> = self
            .anti_gaming_rules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rule| rule.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// True when the user id is a known employee (learned
    /// from `hr.employee_created` events). Absence means
    /// unknown, never "not an employee".
    pub(crate) fn is_known_employee(&self, user_id: &str) -> bool {
        self.known_employees.contains_key(user_id)
    }

    /// Audit rows of one tenant, newest first.
    pub(crate) fn audit_of(&self, tenant_id: &str) -> Vec<AuditRecord> {
        let mut items: Vec<AuditRecord> = self
            .audit
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Emit a `notification.requested` event for a user, in
    /// the notifications module's payload shape.
    pub(crate) fn notify(&self, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": "referral",
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "referral",
                "category": "referral",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}
