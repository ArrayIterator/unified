//! In-memory announcement and changelog store.

use unified_core::hooks::{HookBus};
use unified_core::json;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::types::{AUDIENCE_ADMINS, AUDIENCE_ALL, AUDIENCE_DEPARTMENT, AUDIENCE_ROLE, AUDIENCE_SEGMENT, AUDIENCE_USER, AUDIENCE_USERS, Announcement, ChangelogEntry, ReadReceipt, STATUS_PUBLISHED};
use super::handlers_announcements::publish_due;

// State

pub struct AnnouncementsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) announcements: DashMap<Uuid, Announcement>,
    pub(crate) changelog: DashMap<Uuid, ChangelogEntry>,
    /// Keyed by `{announcement_id}:{user_id}`.
    pub(crate) reads: DashMap<String, ReadReceipt>,
}

impl AnnouncementsState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            announcements: DashMap::new(),
            changelog: DashMap::new(),
            reads: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// True when an announcement is visible on the public list right now.
    pub(crate) fn is_visible_now(announcement: &Announcement, now: DateTime<Utc>) -> bool {
        if announcement.status != STATUS_PUBLISHED {
            return false;
        }
        if let Some(publish_at) = announcement.publish_at {
            if now < publish_at {
                return false;
            }
        }
        if let Some(expires_at) = announcement.expires_at {
            if now >= expires_at {
                return false;
            }
        }
        true
    }

    pub(crate) fn publish(&self, announcement: &Announcement) {
        self.emit(
            "announcement.published",
            json!({
                "announcement_id": announcement.id,
                "title": announcement.title,
                "audience": announcement.audience,
                "audience_value": announcement.audience_value,
                "channels": announcement.channels,
                "published_at": announcement.published_at,
            }),
        );
        self.emit("notification.requested", notification_payload(announcement));
    }

    /// Publish one announcement whose time has come: stamp
    /// `published_at` and fan it out. Returns false when the
    /// announcement is not due yet (future `publish_at`), already
    /// published, or already expired -- in all three cases nothing is
    /// emitted, so a scheduled announcement can never leak early.
    pub(crate) fn maybe_publish(&self, announcement: &Announcement) -> bool {
        if announcement.published_at.is_some() {
            return false;
        }
        if !Self::is_visible_now(announcement, Utc::now()) {
            return false;
        }
        let mut updated = announcement.clone();
        updated.published_at = Some(Utc::now());
        updated.updated_at = Utc::now();
        self.announcements
            .insert(updated.id, updated.clone());
        self.publish(&updated);
        true
    }

    /// Publish every announcement whose publish time has arrived and
    /// which has not been fanned out yet. Returns the announcements
    /// that were published by this call.
    pub(crate) fn publish_due(&self) -> Vec<Announcement> {
        let now = Utc::now();
        let due: Vec<Announcement> = self
            .announcements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|announcement| {
                announcement.status == STATUS_PUBLISHED
                    && announcement.published_at.is_none()
                    && Self::is_visible_now(announcement, now)
            })
            .collect();
        let mut published = Vec::new();
        for announcement in due {
            if self.maybe_publish(&announcement) {
                published.push(announcement);
            }
        }
        published
    }
}

/// Build the `notification.requested` payload for an announcement.
///
/// Fan out through the Notifications module with the full audience,
/// so the centre only ever delivers the announcement to its
/// addressees: `all` / `users` broadcast, `admins` through the
/// registered `role:admin` audience, `role` / `department` /
/// `segment` through their registered audiences, and `user` through
/// the explicit user ids. Email addresses ride along for the email
/// channel, aligned with the user ids by index.
///
/// The singular `user_id` is set only when the audience names exactly
/// one user; with several user ids only the `user_ids` list is set,
/// so no recipient is silently dropped to a single first id.
pub(crate) fn notification_payload(announcement: &Announcement) -> Value {
    let mut payload = json!({
        "title": announcement.title,
        "body": announcement.body,
        "kind": "announcement",
        "category": "announcements",
        "channels": announcement.channels,
        "audience": announcement.audience,
    });
    match announcement.audience.as_str() {
        AUDIENCE_ALL | AUDIENCE_USERS => {
            payload["user_id"] = Value::String("all".to_string());
            payload["audience"] = Value::String("all".to_string());
        }
        AUDIENCE_ADMINS => {
            payload["audience"] = Value::String("role:admin".to_string());
        }
        AUDIENCE_ROLE | AUDIENCE_DEPARTMENT | AUDIENCE_SEGMENT => {
            if let Some(value) = &announcement.audience_value {
                payload["audience"] = Value::String(format!(
                    "{}:{value}",
                    announcement.audience
                ));
            }
        }
        AUDIENCE_USER => {
            if announcement.audience_user_ids.len() == 1 {
                if let Some(only) = announcement.audience_user_ids.first() {
                    payload["user_id"] = Value::String(only.clone());
                }
            }
        }
        _ => {}
    }
    if !announcement.audience_user_ids.is_empty() {
        payload["user_ids"] = serde_json::to_value(&announcement.audience_user_ids)
            .unwrap_or(Value::Array(Vec::new()));
    }
    if !announcement.audience_emails.is_empty() {
        payload["emails"] = serde_json::to_value(&announcement.audience_emails)
            .unwrap_or(Value::Array(Vec::new()));
    }
    if let Some(template_id) = &announcement.email_template_id {
        payload["email_template_id"] = Value::String(template_id.clone());
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_announcement(audience: &str, user_ids: &[&str]) -> Announcement {
        let now = Utc::now();
        Announcement {
            id: Uuid::new_v4(),
            title: "Title".to_string(),
            body: "Body".to_string(),
            audience: audience.to_string(),
            audience_value: None,
            audience_user_ids: user_ids.iter().map(|id| id.to_string()).collect(),
            audience_emails: Vec::new(),
            channels: vec!["in_app".to_string()],
            email_template_id: None,
            status: STATUS_PUBLISHED.to_string(),
            publish_at: None,
            expires_at: None,
            pinned: false,
            created_at: now,
            updated_at: now,
            published_at: None,
        }
    }

    #[test]
    fn notification_payload_keeps_every_user_id_for_multi_user_audience() {
        let announcement = make_announcement("user", &["u1", "u2", "u3"]);
        let payload = notification_payload(&announcement);
        // No singular `user_id`: the full list is the address.
        assert!(payload.get("user_id").is_none());
        assert_eq!(payload.get("user_ids"), Some(&json!(["u1", "u2", "u3"])));
    }

    #[test]
    fn notification_payload_sets_singular_user_id_only_for_one_user() {
        let announcement = make_announcement("user", &["u1"]);
        let payload = notification_payload(&announcement);
        assert_eq!(payload.get("user_id"), Some(&json!("u1")));
        assert_eq!(payload.get("user_ids"), Some(&json!(["u1"])));
        let broadcast = make_announcement("all", &[]);
        let payload = notification_payload(&broadcast);
        assert_eq!(payload.get("user_id"), Some(&json!("all")));
        assert_eq!(payload.get("audience"), Some(&json!("all")));
    }
}
