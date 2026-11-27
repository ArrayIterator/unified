//! In-memory SMS marketing store.

use super::types::{
    AuditRecord, SmsCampaign, SmsContact, SmsContactList, SmsMessage, SmsOptOut, SmsOptOutKeyword,
    SmsProvider, SmsRateLimit, SmsTemplate,
};
use dashmap::DashMap;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// Shared state

/// In-memory stores for the SMS marketing module. The hook
/// bus is attached in `init` (the bus lives in
/// `AppContext`); handlers tolerate a missing bus by
/// skipping event emission. Durable rows for these types
/// are defined by migration
/// `2026-11-22-000121_sms_marketing`; until that migration
/// is executed, SMS data does not survive a restart.
#[derive(Clone)]
pub struct SmsMarketingState {
    pub providers: Arc<DashMap<Uuid, SmsProvider>>,
    pub templates: Arc<DashMap<Uuid, SmsTemplate>>,
    pub contact_lists: Arc<DashMap<Uuid, SmsContactList>>,
    pub contacts: Arc<DashMap<Uuid, SmsContact>>,
    pub campaigns: Arc<DashMap<Uuid, SmsCampaign>>,
    pub messages: Arc<DashMap<Uuid, SmsMessage>>,
    pub opt_outs: Arc<DashMap<Uuid, SmsOptOut>>,
    pub opt_out_keywords: Arc<DashMap<Uuid, SmsOptOutKeyword>>,
    pub rate_limits: Arc<DashMap<Uuid, SmsRateLimit>>,
    pub audit: Arc<DashMap<Uuid, AuditRecord>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for SmsMarketingState {
    fn default() -> Self {
        Self {
            providers: Arc::new(DashMap::new()),
            templates: Arc::new(DashMap::new()),
            contact_lists: Arc::new(DashMap::new()),
            contacts: Arc::new(DashMap::new()),
            campaigns: Arc::new(DashMap::new()),
            messages: Arc::new(DashMap::new()),
            opt_outs: Arc::new(DashMap::new()),
            opt_out_keywords: Arc::new(DashMap::new()),
            rate_limits: Arc::new(DashMap::new()),
            audit: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl SmsMarketingState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Providers of one tenant, newest first.
    pub(crate) fn providers_of(&self, tenant_id: &str) -> Vec<SmsProvider> {
        let mut items: Vec<SmsProvider> = self
            .providers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|provider| provider.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Templates of one tenant, newest first.
    pub(crate) fn templates_of(&self, tenant_id: &str) -> Vec<SmsTemplate> {
        let mut items: Vec<SmsTemplate> = self
            .templates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|template| template.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Contact lists of one tenant, newest first.
    pub(crate) fn contact_lists_of(&self, tenant_id: &str) -> Vec<SmsContactList> {
        let mut items: Vec<SmsContactList> = self
            .contact_lists
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|list| list.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Contacts of one list, in insert order.
    pub(crate) fn contacts_of_list(&self, tenant_id: &str, list_id: Uuid) -> Vec<SmsContact> {
        let mut items: Vec<SmsContact> = self
            .contacts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|contact| contact.tenant_id == tenant_id && contact.list_id == list_id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// Campaigns of one tenant, newest first.
    pub(crate) fn campaigns_of(&self, tenant_id: &str) -> Vec<SmsCampaign> {
        let mut items: Vec<SmsCampaign> = self
            .campaigns
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|campaign| campaign.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Messages of one tenant, newest first.
    pub(crate) fn messages_of(&self, tenant_id: &str) -> Vec<SmsMessage> {
        let mut items: Vec<SmsMessage> = self
            .messages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|message| message.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Messages of one campaign, in send order.
    pub(crate) fn messages_of_campaign(
        &self,
        tenant_id: &str,
        campaign_id: Uuid,
    ) -> Vec<SmsMessage> {
        let mut items: Vec<SmsMessage> = self
            .messages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|message| message.tenant_id == tenant_id && message.campaign_id == campaign_id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// How many messages of this tenant were created in the
    /// last minute (the rate-limit window): messages in
    /// `sent` or `delivered` state count as sends; queued,
    /// failed, skipped, and undelivered rows do not.
    pub(crate) fn sends_in_last_minute(&self, tenant_id: &str) -> i64 {
        let cutoff =
            chrono::Utc::now() - chrono::Duration::seconds(super::types::RATE_LIMIT_WINDOW_SECS);
        self.messages
            .iter()
            .filter(|entry| {
                let message = entry.value();
                message.tenant_id == tenant_id
                    && message.created_at >= cutoff
                    && matches!(
                        message.status,
                        super::types::MessageStatus::Sent | super::types::MessageStatus::Delivered
                    )
            })
            .count() as i64
    }

    /// How many messages went through one provider in the
    /// last minute (same counting rule as
    /// [`Self::sends_in_last_minute`]).
    pub(crate) fn provider_sends_in_last_minute(&self, tenant_id: &str, provider_id: Uuid) -> i64 {
        let cutoff =
            chrono::Utc::now() - chrono::Duration::seconds(super::types::RATE_LIMIT_WINDOW_SECS);
        self.messages
            .iter()
            .filter(|entry| {
                let message = entry.value();
                message.tenant_id == tenant_id
                    && message.provider_id == provider_id
                    && message.created_at >= cutoff
                    && matches!(
                        message.status,
                        super::types::MessageStatus::Sent | super::types::MessageStatus::Delivered
                    )
            })
            .count() as i64
    }

    /// True when `phone_number` is on the tenant's opt-out
    /// list. Sends check this at send time; the list is the
    /// single source of truth, mirroring the newsletter
    /// module's unsubscribe handling.
    pub(crate) fn is_opted_out(&self, tenant_id: &str, phone_number: &str) -> bool {
        self.opt_outs.iter().any(|entry| {
            entry.value().tenant_id == tenant_id && entry.value().phone_number == phone_number
        })
    }

    /// The opt-out row for one number, when one exists.
    pub(crate) fn opt_out_for(&self, tenant_id: &str, phone_number: &str) -> Option<SmsOptOut> {
        self.opt_outs
            .iter()
            .map(|entry| entry.value().clone())
            .find(|row| row.tenant_id == tenant_id && row.phone_number == phone_number)
    }

    /// Opt-outs of one tenant, newest first.
    pub(crate) fn opt_outs_of(&self, tenant_id: &str) -> Vec<SmsOptOut> {
        let mut items: Vec<SmsOptOut> = self
            .opt_outs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Enabled keyword rows of one tenant, in insert order.
    pub(crate) fn keywords_of(&self, tenant_id: &str) -> Vec<SmsOptOutKeyword> {
        let mut items: Vec<SmsOptOutKeyword> = self
            .opt_out_keywords
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// The tenant's rate-limit row, when one exists.
    pub(crate) fn rate_limit_of(&self, tenant_id: &str) -> Option<SmsRateLimit> {
        self.rate_limits
            .iter()
            .map(|entry| entry.value().clone())
            .find(|row| row.tenant_id == tenant_id)
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
                "kind": "sms_marketing",
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "sms_marketing",
                "category": "sms_marketing",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}
