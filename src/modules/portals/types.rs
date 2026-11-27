//! Portals - portal, menu, record and request types.
//!
//! Part of the `portals` module split; see `src/modules/portals.rs`.

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

// Types

/// Lifecycle of a portal: `draft` -> `active`; `disabled` hides the
/// portal from public and client routes until reactivated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortalStatus {
    Draft,
    Active,
    Disabled,
}

impl Default for PortalStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl PortalStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }
}

/// One branded client portal, owned by one company.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Portal {
    pub id: Uuid,
    pub company_id: String,
    pub name: String,
    pub slug: String,
    /// Custom domain requested for this portal (for example
    /// `portal.example.com`). Provisioning and verification belong
    /// to the domains module; see `domain_status`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_domain: Option<String>,
    /// `none` (no domain requested), `requested`, `verified`, or
    /// `rejected`. A requested domain moves past `requested` only
    /// through the verification write-back endpoint
    /// (`PATCH /{id}/domain-status`, `portals.manage` capability),
    /// which the domains module - or an operator acting on its
    /// verification result - calls; there is no listener on
    /// `portals.domain.requested` yet to do this automatically.
    pub domain_status: String,
    /// Storage module reference (target + key) for the portal logo.
    /// Metadata only: no image bytes live in this module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_storage_key: Option<String>,
    /// Brand colours, validated `#RGB` / `#RRGGBB` hex.
    pub primary_color: String,
    pub secondary_color: String,
    pub accent_color: String,
    pub background_color: String,
    pub text_color: String,
    /// Visible sections, in display order. Every entry is one of
    /// `SECTION_NAMES`; duplicates are rejected.
    #[serde(default)]
    pub sections: Vec<String>,
    pub status: PortalStatus,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One menu entry of a portal. `target_kind` is `section` (the
/// `target` is a section name from `SECTION_NAMES`) or `url` (the
/// `target` is an `https://` / `http://` URL). `roles` limits the
/// entry to client roles; an empty list means every client role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalMenu {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub label: String,
    pub target_kind: String,
    pub target: String,
    pub position: i64,
    pub visible: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roles: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One row of the read-only client-data projection: a display view
/// of a record owned by the billing / tickets / store / licensing
/// modules. The owning module stays the source of truth; this row
/// carries only what the portal renders, and this module never
/// writes back to the source record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalRecord {
    /// Source record id, as announced on the hook bus.
    pub id: String,
    /// `invoice`, `ticket`, `download`, `subscription`, or
    /// `license`.
    pub kind: String,
    pub company_id: String,
    pub user_id: String,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// Owning module id (`billing`, `tickets`, `store`,
    /// `licensing`, `downloader`).
    pub source_module: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recorded portal page view (public portal render or client
/// home). No secrets are stored: only the viewer id when the
/// caller was authenticated, the client IP, and the user agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalView {
    pub id: Uuid,
    pub portal_id: Uuid,
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    pub viewed_at: DateTime<Utc>,
}

// Request bodies

#[derive(Debug, Deserialize)]
pub struct CreatePortalRequest {
    pub company_id: String,
    pub name: String,
    pub slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo_storage_key: Option<String>,
    pub primary_color: Option<String>,
    pub secondary_color: Option<String>,
    pub accent_color: Option<String>,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
    #[serde(default)]
    pub sections: Vec<String>,
    pub status: Option<PortalStatus>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePortalRequest {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub custom_domain: Option<Option<String>>,
    pub logo_storage_target: Option<Option<String>>,
    pub logo_storage_key: Option<Option<String>>,
    pub primary_color: Option<String>,
    pub secondary_color: Option<String>,
    pub accent_color: Option<String>,
    pub background_color: Option<String>,
    pub text_color: Option<String>,
    pub sections: Option<Vec<String>>,
    pub status: Option<PortalStatus>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDomainStatusRequest {
    /// Verification outcome: `verified` or `rejected`.
    pub status: String,
    /// The domain the result is for; when given it must match the
    /// portal's requested custom domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Optional human-readable reason (rejection cause, verifier
    /// note); announced on the emitted event, not stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateMenuRequest {
    pub label: String,
    pub target_kind: String,
    pub target: String,
    pub position: Option<i64>,
    #[serde(default = "default_visible")]
    pub visible: bool,
    #[serde(default)]
    pub roles: Vec<String>,
}

fn default_visible() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct UpdateMenuRequest {
    pub label: Option<String>,
    pub target_kind: Option<String>,
    pub target: Option<String>,
    pub position: Option<i64>,
    pub visible: Option<bool>,
    pub roles: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ReorderMenusRequest {
    pub menu_ids: Vec<Uuid>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_status_lifecycle() {
        assert_eq!(PortalStatus::default(), PortalStatus::Draft);
        assert_eq!(PortalStatus::Draft.as_str(), "draft");
        assert_eq!(PortalStatus::Active.as_str(), "active");
        assert_eq!(PortalStatus::Disabled.as_str(), "disabled");
    }

    #[test]
    fn portal_status_serde() {
        assert_eq!(
            serde_json::to_string(&PortalStatus::Active).unwrap(),
            "\"active\""
        );
        assert_eq!(
            serde_json::from_str::<PortalStatus>("\"disabled\"").unwrap(),
            PortalStatus::Disabled
        );
    }

    #[test]
    fn default_visible_is_true() {
        assert!(default_visible());
    }
}
