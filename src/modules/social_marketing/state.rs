//! In-memory social marketing store.

use super::types::{
    AuditRecord, CampaignPostCounts, MetricTotals, PostStatus, SocialAccount, SocialCampaign,
    SocialMetric, SocialNetwork, SocialPost,
};
use dashmap::DashMap;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// Shared state

/// In-memory stores for the social marketing module. The
/// hook bus is attached in `init` (the bus lives in
/// `AppContext`); handlers tolerate a missing bus by
/// skipping event emission. Durable rows for these types
/// are defined by migration
/// `2026-11-22-000122_social_marketing`; until that
/// migration is executed, social data does not survive a
/// restart.
#[derive(Clone)]
pub struct SocialMarketingState {
    pub networks: Arc<DashMap<Uuid, SocialNetwork>>,
    pub accounts: Arc<DashMap<Uuid, SocialAccount>>,
    pub posts: Arc<DashMap<Uuid, SocialPost>>,
    pub campaigns: Arc<DashMap<Uuid, SocialCampaign>>,
    pub metrics: Arc<DashMap<Uuid, SocialMetric>>,
    pub audit: Arc<DashMap<Uuid, AuditRecord>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for SocialMarketingState {
    fn default() -> Self {
        Self {
            networks: Arc::new(DashMap::new()),
            accounts: Arc::new(DashMap::new()),
            posts: Arc::new(DashMap::new()),
            campaigns: Arc::new(DashMap::new()),
            metrics: Arc::new(DashMap::new()),
            audit: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl SocialMarketingState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Networks of one tenant, newest first.
    pub(crate) fn networks_of(&self, tenant_id: &str) -> Vec<SocialNetwork> {
        let mut items: Vec<SocialNetwork> = self
            .networks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|network| network.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Accounts of one tenant, newest first.
    pub(crate) fn accounts_of(&self, tenant_id: &str) -> Vec<SocialAccount> {
        let mut items: Vec<SocialAccount> = self
            .accounts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|account| account.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Accounts of one network, in connect order.
    pub(crate) fn accounts_of_network(
        &self,
        tenant_id: &str,
        network_id: Uuid,
    ) -> Vec<SocialAccount> {
        let mut items: Vec<SocialAccount> = self
            .accounts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|account| account.tenant_id == tenant_id && account.network_id == network_id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// Posts of one tenant, newest first.
    pub(crate) fn posts_of(&self, tenant_id: &str) -> Vec<SocialPost> {
        let mut items: Vec<SocialPost> = self
            .posts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|post| post.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Posts of one campaign, in insert order.
    pub(crate) fn posts_of_campaign(&self, tenant_id: &str, campaign_id: Uuid) -> Vec<SocialPost> {
        let mut items: Vec<SocialPost> = self
            .posts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|post| post.tenant_id == tenant_id && post.campaign_id == Some(campaign_id))
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// Posts targeting one account, newest first.
    pub(crate) fn posts_of_account(&self, tenant_id: &str, account_id: Uuid) -> Vec<SocialPost> {
        let needle = account_id.to_string();
        let mut items: Vec<SocialPost> = self
            .posts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|post| post.tenant_id == tenant_id && post.account_ids.contains(&needle))
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Post status counts for one campaign, computed live
    /// from the post rows.
    pub(crate) fn post_counts_of_campaign(
        &self,
        tenant_id: &str,
        campaign_id: Uuid,
    ) -> CampaignPostCounts {
        let mut counts = CampaignPostCounts::default();
        for post in self.posts_of_campaign(tenant_id, campaign_id) {
            counts.total += 1;
            match post.status {
                PostStatus::Draft => counts.draft += 1,
                PostStatus::Scheduled => counts.scheduled += 1,
                PostStatus::Publishing => counts.publishing += 1,
                PostStatus::Published => counts.published += 1,
                PostStatus::Failed => counts.failed += 1,
                PostStatus::Cancelled => counts.cancelled += 1,
            }
        }
        counts
    }

    /// Campaigns of one tenant, newest first.
    pub(crate) fn campaigns_of(&self, tenant_id: &str) -> Vec<SocialCampaign> {
        let mut items: Vec<SocialCampaign> = self
            .campaigns
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|campaign| campaign.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Metric rows of one tenant, newest first.
    pub(crate) fn metrics_of(&self, tenant_id: &str) -> Vec<SocialMetric> {
        let mut items: Vec<SocialMetric> = self
            .metrics
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|metric| metric.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Metric rows of one post, in insert order.
    pub(crate) fn metrics_of_post(&self, tenant_id: &str, post_id: Uuid) -> Vec<SocialMetric> {
        let mut items: Vec<SocialMetric> = self
            .metrics
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|metric| metric.tenant_id == tenant_id && metric.post_id == post_id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// Summed metrics of one post.
    pub(crate) fn rollup_of_post(&self, tenant_id: &str, post_id: Uuid) -> MetricTotals {
        let pairs: Vec<(String, i64)> = self
            .metrics_of_post(tenant_id, post_id)
            .iter()
            .map(|metric| (metric.metric.clone(), metric.value))
            .collect();
        super::types::sum_metrics(&pairs)
    }

    /// Summed metrics of every post targeting one account.
    pub(crate) fn rollup_of_account(&self, tenant_id: &str, account_id: Uuid) -> MetricTotals {
        let mut totals = MetricTotals::default();
        for post in self.posts_of_account(tenant_id, account_id) {
            let post_totals = self.rollup_of_post(tenant_id, post.id);
            super::types::merge_totals(&mut totals, &post_totals);
        }
        totals
    }

    /// Summed metrics of every post in one campaign.
    pub(crate) fn rollup_of_campaign(&self, tenant_id: &str, campaign_id: Uuid) -> MetricTotals {
        let mut totals = MetricTotals::default();
        for post in self.posts_of_campaign(tenant_id, campaign_id) {
            let post_totals = self.rollup_of_post(tenant_id, post.id);
            super::types::merge_totals(&mut totals, &post_totals);
        }
        totals
    }

    /// How many posts of this network reached `published`
    /// in the last minute (the rate-limit window): the
    /// `published_at` timestamp is the publish time.
    pub(crate) fn publishes_in_last_minute(&self, tenant_id: &str, network_id: Uuid) -> i64 {
        let cutoff =
            chrono::Utc::now() - chrono::Duration::seconds(super::types::RATE_LIMIT_WINDOW_SECS);
        self.posts
            .iter()
            .filter(|entry| {
                let post = entry.value();
                post.tenant_id == tenant_id
                    && post.network_id == network_id
                    && post.status == PostStatus::Published
                    && post
                        .published_at
                        .map(|when| when >= cutoff)
                        .unwrap_or(false)
            })
            .count() as i64
    }

    /// The metric row for one (post, metric, source) triple,
    /// when one exists: re-ingesting the same metric from
    /// the same source replaces the value instead of adding
    /// a second row.
    pub(crate) fn metric_for(
        &self,
        tenant_id: &str,
        post_id: Uuid,
        metric: &str,
        source: &str,
    ) -> Option<SocialMetric> {
        self.metrics
            .iter()
            .map(|entry| entry.value().clone())
            .find(|row| {
                row.tenant_id == tenant_id
                    && row.post_id == post_id
                    && row.metric == metric
                    && row.source == source
            })
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
                "kind": "social_marketing",
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "social_marketing",
                "category": "social_marketing",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}
