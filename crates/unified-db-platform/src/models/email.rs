// Diesel models for the email domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Email and newsletter (2026-11-22-000016_email_newsletter).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = email_connections)]
pub struct EmailConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub domain: Option<String>,
    pub region: Option<String>,
    pub sender_email: Option<String>,
    pub sender_name: Option<String>,
    pub webhook_url: Option<String>,
    #[serde(skip_serializing)]
    pub credentials: Value,
    pub test_mode: bool,
    pub enabled: bool,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = email_connections)]
pub struct NewEmailConnection {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub domain: Option<String>,
    pub region: Option<String>,
    pub sender_email: Option<String>,
    pub sender_name: Option<String>,
    pub webhook_url: Option<String>,
    pub last_tested_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = email_send_log)]
pub struct EmailSendLog {
    pub id: Uuid,
    pub connection_id: Option<Uuid>,
    pub provider: String,
    pub to: String,
    pub subject: String,
    pub status: String,
    pub provider_message_id: Option<String>,
    pub test_mode: bool,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = email_send_log)]
pub struct NewEmailSendLog {
    pub id: Uuid,
    pub connection_id: Option<Uuid>,
    pub provider: String,
    pub to: String,
    pub subject: String,
    pub status: String,
    pub provider_message_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mail_smtp_settings)]
pub struct MailSmtpSettings {
    pub key: String,
    pub host: String,
    pub port: i32,
    pub tls: bool,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_encrypted: String,
    pub sender_name: String,
    pub sender_email: String,
    pub connector: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mail_smtp_settings)]
pub struct NewMailSmtpSettings {
    pub key: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = mail_log)]
pub struct MailLog {
    pub id: Uuid,
    pub to: String,
    pub subject: String,
    pub status: String,
    pub connector: String,
    pub template_id: Option<Uuid>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = mail_log)]
pub struct NewMailLog {
    pub id: Uuid,
    pub to: String,
    pub subject: String,
    pub status: String,
    pub connector: String,
    pub template_id: Option<Uuid>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = email_templates)]
pub struct EmailTemplate {
    pub id: Uuid,
    pub name: String,
    pub subject: String,
    pub category: String,
    pub html: String,
    pub json: Value,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = email_templates)]
pub struct NewEmailTemplate {
    pub id: Uuid,
    pub name: String,
    pub subject: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = email_template_versions)]
pub struct EmailTemplateVersion {
    pub id: Uuid,
    pub template_id: Uuid,
    pub version: i32,
    pub subject: String,
    pub html: String,
    pub json: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = email_template_versions)]
pub struct NewEmailTemplateVersion {
    pub id: Uuid,
    pub template_id: Uuid,
    pub version: i32,
    pub subject: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = newsletter_lists)]
pub struct NewsletterList {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub default_connection_id: Option<Uuid>,
    pub double_opt_in: bool,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub base_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = newsletter_lists)]
pub struct NewNewsletterList {
    pub id: Uuid,
    pub name: String,
    pub default_connection_id: Option<Uuid>,
    pub sender_name: Option<String>,
    pub sender_email: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = newsletter_subscribers)]
pub struct NewsletterSubscriber {
    pub id: Uuid,
    pub list_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub status: String,
    pub metadata: Value,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub unsubscribed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = newsletter_subscribers)]
pub struct NewNewsletterSubscriber {
    pub id: Uuid,
    pub list_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub status: String,
    pub confirmed_at: Option<DateTime<Utc>>,
    pub unsubscribed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = newsletter_campaigns)]
pub struct NewsletterCampaign {
    pub id: Uuid,
    pub list_id: Uuid,
    pub name: String,
    pub template_id: Option<Uuid>,
    pub subject: String,
    pub preview_text: Option<String>,
    pub connection_id: Option<Uuid>,
    pub status: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub stats_sent: i64,
    pub stats_delivered: i64,
    pub stats_opened: i64,
    pub stats_clicked: i64,
    pub stats_bounced: i64,
    pub stats_unsubscribed: i64,
    pub stats_complaints: i64,
    pub stats_unique_opened: i64,
    pub stats_unique_clicked: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = newsletter_campaigns)]
pub struct NewNewsletterCampaign {
    pub id: Uuid,
    pub list_id: Uuid,
    pub name: String,
    pub template_id: Option<Uuid>,
    pub subject: String,
    pub preview_text: Option<String>,
    pub connection_id: Option<Uuid>,
    pub status: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = newsletter_tracking_events)]
pub struct NewsletterTrackingEvent {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub subscriber_id: Uuid,
    pub kind: String,
    pub url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = newsletter_tracking_events)]
pub struct NewNewsletterTrackingEvent {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub subscriber_id: Uuid,
    pub kind: String,
    pub url: Option<String>,
}
