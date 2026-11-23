// notifications module -- In-memory state and small access helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use super::*;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct NotificationsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) notifications: DashMap<String, Notification>,
    pub(crate) preferences: DashMap<String, NotificationPreferences>,
    pub(crate) templates: DashMap<String, NotificationTemplate>,
    pub(crate) deliveries: DashMap<String, DeliveryAttempt>,
    pub(crate) audiences: DashMap<String, AudienceMembership>,
}

impl NotificationsState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            notifications: DashMap::new(),
            preferences: DashMap::new(),
            templates: DashMap::new(),
            deliveries: DashMap::new(),
            audiences: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    pub(crate) fn create_notification(&self, input: CreateNotification) -> UnifiedResult<Notification> {
        if input.user_id.trim().is_empty() {
            return Err(UnifiedError::bad_request("user_id must not be empty"));
        }
        if input.title.trim().is_empty() {
            return Err(UnifiedError::bad_request("Title must not be empty"));
        }
        let kind = input.kind.unwrap_or_else(|| "general".to_string());
        let notification = Notification {
            id: new_id(),
            user_id: input.user_id.trim().to_string(),
            title: input.title.trim().to_string(),
            body: input.body,
            category: input
                .category
                .filter(|value| NOTIFICATION_CATEGORIES.contains(&value.as_str()))
                .unwrap_or_else(|| category_for_kind(&kind).to_string()),
            module: input
                .module
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| module_for_kind(&kind).to_string()),
            priority: normalize_priority(input.priority.as_deref().unwrap_or("normal")),
            link: input.link.filter(|value| !value.trim().is_empty()),
            audience: input
                .audience
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| {
                    if input.user_id.trim() == "all" {
                        "all".to_string()
                    } else {
                        "user".to_string()
                    }
                }),
            kind,
            read: false,
            created_at: now(),
        };
        self.notifications
            .insert(notification.id.clone(), notification.clone());
        Ok(notification)
    }

    pub(crate) fn preferences_for(&self, user_id: &str) -> NotificationPreferences {
        self.preferences
            .get(user_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| NotificationPreferences {
                user_id: user_id.to_string(),
                channels: vec!["in_app".to_string()],
                categories: default_categories(),
                channel_by_category: HashMap::new(),
                digest: default_digest(),
                updated_at: now(),
            })
    }

    /// The template for an event and locale, falling back to the
    /// default locale (`en`) and then to any template for the event.
    /// The fallback is deterministic: when several non-English
    /// locales exist for the event, the alphabetically smallest
    /// locale wins, so the choice never depends on map iteration
    /// order.
    pub(crate) fn template_for(&self, event: &str, locale: &str) -> Option<NotificationTemplate> {
        let mut en_fallback: Option<NotificationTemplate> = None;
        let mut other_fallback: Option<NotificationTemplate> = None;
        for entry in self.templates.iter() {
            let template = entry.value();
            if template.event != event {
                continue;
            }
            if template.locale == locale {
                return Some(template.clone());
            }
            if template.locale == "en" {
                if en_fallback.is_none() {
                    en_fallback = Some(template.clone());
                }
            } else if other_fallback
                .as_ref()
                .is_none_or(|best| template.locale < best.locale)
            {
                other_fallback = Some(template.clone());
            }
        }
        en_fallback.or(other_fallback)
    }

    /// Create a notification for a failure event, but only when the
    /// target user's category preferences include `category`. Returns
    /// `None` (without storing anything) when the user opted out of
    /// the category. This guard applies to the event-driven failure
    /// listeners below, not to explicit `notification.requested`
    /// calls, which are always stored.
    pub(crate) fn notify_for_category(
        &self,
        user_id: &str,
        category: &str,
        title: String,
        body: String,
        kind: &str,
    ) -> Option<Notification> {
        if !self
            .preferences_for(user_id)
            .categories
            .iter()
            .any(|subscribed| subscribed == category)
        {
            return None;
        }
        self.create_notification(CreateNotification {
            user_id: user_id.to_string(),
            user_ids: Vec::new(),
            emails: Vec::new(),
            audience: None,
            title,
            body,
            kind: Some(kind.to_string()),
            category: Some(category.to_string()),
            module: None,
            priority: None,
            link: None,
            channels: Vec::new(),
            email_template_id: None,
            email: None,
        })
        .ok()
    }

    /// Store one notification for an event-driven listener, applying
    /// the target user's category preferences first. Returns `None`
    /// (storing nothing) when the user opted out of the category.
    /// Explicit `notification.requested` calls do NOT go through here:
    /// they are always stored.
    pub(crate) fn store_notification(
        &self,
        user_id: &str,
        title: String,
        body: String,
        kind: &str,
        category: &str,
        priority: &str,
        link: Option<String>,
        audience: &str,
    ) -> Option<Notification> {
        if !self
            .preferences_for(user_id)
            .categories
            .iter()
            .any(|subscribed| subscribed == category)
        {
            return None;
        }
        self.create_notification(CreateNotification {
            user_id: user_id.to_string(),
            user_ids: Vec::new(),
            emails: Vec::new(),
            audience: Some(audience.to_string()),
            title,
            body,
            kind: Some(kind.to_string()),
            category: Some(category.to_string()),
            module: None,
            priority: Some(priority.to_string()),
            link,
            channels: Vec::new(),
            email_template_id: None,
            email: None,
        })
        .ok()
    }

    /// True when a notification addressed to `notification.user_id`
    /// may be shown to `user_id`. Broadcast notifications
    /// (`user_id` "all") are visible to every user; anything else is
    /// visible only to its own recipient. This is the server-side
    /// audience rule for list, unread-count and mark-all-read.
    pub(crate) fn is_visible_to(notification: &Notification, user_id: &str) -> bool {
        notification.user_id == user_id || notification.user_id == "all"
    }

    /// Effective channels for a user and category: the per-category
    /// override wins when one is set, otherwise the global list.
    pub(crate) fn effective_channels(&self, user_id: &str, category: &str) -> Vec<String> {
        let preferences = self.preferences_for(user_id);
        preferences
            .channel_by_category
            .get(category)
            .cloned()
            .unwrap_or(preferences.channels)
    }

    /// Registered members of a role / department / segment audience,
    /// or `None` when the audience was never registered.
    pub(crate) fn audience_members(&self, audience: &str) -> Option<Vec<AudienceMember>> {
        self.audiences
            .get(audience)
            .map(|entry| entry.value().members.clone())
    }

    /// Record one delivery attempt and return the stored copy.
    pub(crate) fn record_delivery(
        &self,
        notification_id: Option<String>,
        channel: &str,
        status: &str,
        provider: Option<String>,
        job_id: Option<String>,
        to: Option<String>,
        subject: Option<String>,
        email_template_id: Option<String>,
        error: Option<String>,
        attempts: u32,
    ) -> DeliveryAttempt {
        let attempt = DeliveryAttempt {
            id: new_id(),
            notification_id,
            channel: channel.to_string(),
            status: status.to_string(),
            provider,
            provider_message_id: None,
            to,
            subject,
            email_template_id,
            job_id,
            error,
            attempts,
            max_attempts: MAX_DELIVERY_ATTEMPTS,
            next_retry_at: if status == "failed" {
                next_retry_at(attempts)
            } else {
                None
            },
            created_at: now(),
            updated_at: now(),
        };
        self.deliveries
            .insert(attempt.id.clone(), attempt.clone());
        attempt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_template(event: &str, locale: &str) -> NotificationTemplate {
        NotificationTemplate {
            id: format!("{event}-{locale}"),
            event: event.to_string(),
            locale: locale.to_string(),
            title_template: "Title".to_string(),
            body_template: "Body".to_string(),
            channels: vec!["in_app".to_string()],
            email_connection_id: None,
            email_template_id: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn state_with_locales(event: &str, locales: &[&str]) -> NotificationsState {
        let state = NotificationsState::new();
        for locale in locales {
            let template = test_template(event, locale);
            state.templates.insert(template.id.clone(), template);
        }
        state
    }

    #[test]
    fn template_for_prefers_the_exact_locale() {
        let state = state_with_locales("order.paid", &["en", "id", "de"]);
        assert_eq!(
            state.template_for("order.paid", "id").unwrap().locale,
            "id"
        );
    }

    #[test]
    fn template_for_prefers_english_over_other_locales() {
        // "ar" sorts before "en", but the English fallback still wins.
        let state = state_with_locales("order.paid", &["ar", "en", "id"]);
        assert_eq!(
            state.template_for("order.paid", "ja").unwrap().locale,
            "en"
        );
    }

    #[test]
    fn template_for_fallback_is_deterministic_without_english() {
        let state = state_with_locales("order.paid", &["id", "fr", "de"]);
        for _ in 0..50 {
            assert_eq!(
                state.template_for("order.paid", "ja").unwrap().locale,
                "de",
                "the alphabetically smallest locale must always win"
            );
        }
    }

    #[test]
    fn template_for_returns_none_for_unknown_events() {
        let state = state_with_locales("order.paid", &["en"]);
        assert!(state.template_for("invoice.paid", "en").is_none());
    }
}
