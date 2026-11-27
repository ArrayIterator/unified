// Newsletter module - shared types and request/response shapes.
//
// Split from `src/modules/newsletter.rs`; the module root re-exports the public API of this module.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// Types.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsletterList {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Default email provider connection for this list's campaigns.
    /// A campaign may override it with its own connection id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_connection_id: Option<String>,
    pub double_opt_in: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_email: Option<String>,
    /// Public base URL used to build confirmation, unsubscribe, and
    /// tracking links (for example "https://example.com"). When empty,
    /// relative links are returned instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscriber {
    pub id: String,
    pub list_id: String,
    pub email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `pending`, `confirmed`, `unsubscribed`, `bounced`, `complained`.
    pub status: String,
    pub metadata: HashMap<String, Value>,
    pub confirmed_at: Option<String>,
    pub unsubscribed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CampaignStats {
    pub sent: u64,
    pub delivered: u64,
    pub opened: u64,
    pub clicked: u64,
    pub bounced: u64,
    pub unsubscribed: u64,
    pub complaints: u64,
    /// Unique opens (one per subscriber) as opposed to total pixel hits.
    pub unique_opened: u64,
    pub unique_clicked: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CampaignStatsView {
    #[serde(flatten)]
    pub stats: CampaignStats,
    pub open_rate: f64,
    pub click_rate: f64,
    pub bounce_rate: f64,
    pub unsubscribe_rate: f64,
    pub complaint_rate: f64,
}

impl CampaignStats {
    pub(crate) fn view(&self) -> CampaignStatsView {
        let rate = |count: u64| {
            if self.sent == 0 {
                0.0
            } else {
                (count as f64 / self.sent as f64 * 10000.0).round() / 100.0
            }
        };
        CampaignStatsView {
            stats: self.clone(),
            open_rate: rate(self.unique_opened),
            click_rate: rate(self.unique_clicked),
            bounce_rate: rate(self.bounced),
            unsubscribe_rate: rate(self.unsubscribed),
            complaint_rate: rate(self.complaints),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Campaign {
    pub id: String,
    pub list_id: String,
    pub name: String,
    /// GrapeJS email template id (from the Email Templates module).
    pub template_id: String,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_text: Option<String>,
    /// Explicit provider connection for this campaign. Falls back to
    /// the list's default connection, then to the global default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection_id: Option<String>,
    /// `draft`, `scheduled`, `sending`, `sent`, `completed`, `failed`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<String>,
    pub stats: CampaignStats,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackingEvent {
    pub id: String,
    pub campaign_id: String,
    pub subscriber_id: String,
    /// `open` or `click`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub created_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn campaign_stats_view_computes_rates() {
        let stats = CampaignStats {
            sent: 100,
            delivered: 90,
            opened: 40,
            clicked: 15,
            bounced: 5,
            unsubscribed: 2,
            complaints: 1,
            unique_opened: 25,
            unique_clicked: 10,
        };
        let view = stats.view();
        assert_eq!(view.open_rate, 25.0);
        assert_eq!(view.click_rate, 10.0);
        assert_eq!(view.bounce_rate, 5.0);
        assert_eq!(view.unsubscribe_rate, 2.0);
        assert_eq!(view.complaint_rate, 1.0);
    }

    #[test]
    fn campaign_stats_view_is_zero_without_sends() {
        let stats = CampaignStats {
            sent: 0,
            unique_opened: 5,
            ..Default::default()
        };
        let view = stats.view();
        assert_eq!(view.open_rate, 0.0);
        assert_eq!(view.click_rate, 0.0);
    }

    #[test]
    fn campaign_stats_view_rounds_to_two_decimals() {
        let stats = CampaignStats {
            sent: 3,
            unique_opened: 1,
            ..Default::default()
        };
        assert_eq!(stats.view().open_rate, 33.33);
    }
}
