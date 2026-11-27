// WhatsApp Business Cloud API connector module for the Unified server.
//
// Outbound messaging through the Meta Graph API, a data-driven template
// registry, a Meta webhook (verification + HMAC-SHA256 signature check),
// a per-tenant message log, and conversation summaries. All state is
// kept in memory; nothing is persisted to the database.
//
// Configuration comes only from environment variables, read at startup
// (`init`) and at module construction:
//
//   WHATSAPP_ACCESS_TOKEN            Meta Graph API access token.
//   WHATSAPP_PHONE_NUMBER_ID         WhatsApp Business phone number id.
//   WHATSAPP_VERIFY_TOKEN            Token Meta sends back on webhook verification.
//   WHATSAPP_APP_SECRET              App secret for X-Hub-Signature-256 verification.
//   WHATSAPP_RATE_LIMIT_PER_MINUTE   Sends per tenant per minute (default 30).
//   WHATSAPP_GRAPH_API_VERSION       Graph API version (default "v19.0").
//   WHATSAPP_GRAPH_API_BASE          Graph API base URL (default
//                                    "https://graph.facebook.com").
//
// Secrets are never hard-coded, never logged, and never returned in any
// response. Every record carries a `tenant_id`: it is required on every
// write and is a mandatory filter on every read. There is no global
// view - a request without `tenant_id` is rejected.
//
// The crate depends only on `unified-core` (plus actix-web, reqwest,
// hmac/sha2, serde, chrono, uuid, tokio for its own handlers). It never
// imports the server crate, other modules, the HTTP layer, the CLI, or
// the gateway. See docs/WORKSPACE_AND_EXTERNAL_MODULES.md sections 3-4
// and crates/example-hello-module for the module contract.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::{SecondsFormat, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use unified_core::auth::RouteAuth;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, events};
use unified_core::json::{error_response, success_response};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use uuid::Uuid;

// Split from the parent file: the implementation now lives
// in the submodules below; the public paths are unchanged
// (this file re-exports everything that used to live here).
pub mod config;
pub mod handlers;
pub mod module;
pub mod state;
pub mod types;
pub(crate) use config::{
    DEFAULT_TEMPLATE_LANGUAGE, HmacSha256, MAX_TEMPLATE_VARIABLES, MAX_TEXT_LEN,
    MAX_WEBHOOK_BODY_BYTES, UNASSIGNED_TENANT, new_wamid, now_rfc3339,
};
pub use config::{
    MESSAGE_RECEIVED_EVENT, MESSAGE_STATUS_EVENT, ORDER_PAID_EVENT, WhatsAppConfig,
    constant_time_eq, count_template_variables, is_valid_e164, is_valid_language_code,
    is_valid_template_name, is_valid_tenant_id, to_hex, verify_hmac_sha256_hex,
};
pub(crate) use handlers::{
    create_template_handler, get_message_handler, list_conversations_handler,
    list_messages_handler, list_templates_handler, send_handler, status_handler,
    webhook_post_handler, webhook_verify_handler,
};
pub use module::{WhatsAppModule, module, register};
pub(crate) use state::attempt_delivery;
pub use state::build_graph_payload;
pub use types::{
    ConversationView, MessageRecord, NotificationIntent, TEMPLATE_CATEGORIES, Template,
    WhatsAppState,
};

// Records and in-memory store

/// Message statuses used by this module.
///
/// Outbound: `queued` (delivery attempted), `pending_config` (no
/// credentials configured, delivery never attempted), `sent`,
/// `delivered`, `read`, `failed`. Inbound: `received`.
pub mod statuses {
    pub const QUEUED: &str = "queued";
    pub const PENDING_CONFIG: &str = "pending_config";
    pub const SENT: &str = "sent";
    pub const DELIVERED: &str = "delivered";
    pub const READ: &str = "read";
    pub const FAILED: &str = "failed";
    pub const RECEIVED: &str = "received";

    /// Every status a list filter may ask for.
    pub const ALL: &[&str] = &[
        QUEUED,
        PENDING_CONFIG,
        SENT,
        DELIVERED,
        READ,
        FAILED,
        RECEIVED,
    ];
}

// Unit tests

#[cfg(test)]
mod tests {
    use super::*;

    fn text_record() -> MessageRecord {
        MessageRecord {
            id: new_wamid(),
            tenant_id: "tenant-a".to_string(),
            direction: "outbound".to_string(),
            phone: "+6281234567890".to_string(),
            kind: "text".to_string(),
            body: Some("Hello".to_string()),
            template_name: None,
            template_language: None,
            template_variables: Vec::new(),
            status: statuses::QUEUED.to_string(),
            status_reason: None,
            external_id: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
        }
    }

    #[test]
    fn e164_accepts_valid_numbers() {
        assert!(is_valid_e164("+6281234567890"));
        assert!(is_valid_e164("+14155552671"));
        assert!(is_valid_e164("+12345678"));
        assert!(is_valid_e164("+123456789012345"));
    }

    #[test]
    fn e164_rejects_invalid_numbers() {
        for bad in [
            "6281234567890",
            "+06281234567890",
            "+12",
            "+1234567890123456",
            "+12a4567890",
            "",
            "+",
            "+ 6281234567890",
        ] {
            assert!(!is_valid_e164(bad), "{bad} should be rejected");
        }
    }

    #[test]
    fn template_variable_counting() {
        assert_eq!(count_template_variables("Hello").unwrap(), 0);
        assert_eq!(
            count_template_variables("Hello {{1}}, your order {{2}} is ready").unwrap(),
            2
        );
        assert_eq!(count_template_variables("{{1}} {{1}}").unwrap(), 1);
        assert_eq!(count_template_variables("{{1}}{{2}}{{3}}").unwrap(), 3);
        assert!(count_template_variables("Hello {{2}}").is_err());
        assert!(count_template_variables("Hello {{0}}").is_err());
        assert!(count_template_variables("Hello {{x}}").is_err());
        assert!(count_template_variables("Hello {{1}").is_err());
        assert!(count_template_variables("Hello {{1}} {{3}}").is_err());
    }

    #[test]
    fn hmac_signature_verification() {
        let secret = b"test-app-secret";
        let body = b"{\"entry\":[]}";
        let mut mac = HmacSha256::new_from_slice(secret).unwrap();
        mac.update(body);
        let hex = to_hex(&mac.finalize().into_bytes());
        assert!(verify_hmac_sha256_hex(secret, body, &hex));
        assert!(verify_hmac_sha256_hex(secret, body, &hex.to_uppercase()));
        assert!(!verify_hmac_sha256_hex(secret, body, "00"));
        assert!(!verify_hmac_sha256_hex(secret, b"other body", &hex));
        assert!(!verify_hmac_sha256_hex(b"wrong-secret", body, &hex));
        assert!(!verify_hmac_sha256_hex(secret, body, "not-hex!!"));
    }

    #[test]
    fn constant_time_compare() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn template_name_and_language_validation() {
        assert!(is_valid_template_name("order_paid"));
        assert!(is_valid_template_name("hello_world_2"));
        assert!(!is_valid_template_name("Order-Paid"));
        assert!(!is_valid_template_name("order paid"));
        assert!(!is_valid_template_name(""));
        assert!(is_valid_language_code("en_US"));
        assert!(is_valid_language_code("id_ID"));
        assert!(!is_valid_language_code("en-us"));
        assert!(!is_valid_language_code("en"));
        assert!(!is_valid_language_code("en_US_extra"));
    }

    #[test]
    fn tenant_id_validation() {
        assert!(is_valid_tenant_id("tenant-a_1.2"));
        assert!(!is_valid_tenant_id(""));
        assert!(!is_valid_tenant_id("tenant a"));
        assert!(!is_valid_tenant_id("tenant/a"));
    }

    #[test]
    fn rate_limit_blocks_after_limit_per_tenant() {
        let mut config = WhatsAppConfig::from_env();
        config.rate_limit_per_minute = 2;
        let state = WhatsAppState::new(config);
        assert!(state.check_rate_limit("tenant-a").is_ok());
        assert!(state.check_rate_limit("tenant-a").is_ok());
        assert!(state.check_rate_limit("tenant-a").is_err());
        // Another tenant has its own window.
        assert!(state.check_rate_limit("tenant-b").is_ok());
    }

    #[test]
    fn graph_payload_for_text_message() {
        let record = text_record();
        let payload = build_graph_payload(&record);
        assert_eq!(payload["messaging_product"], "whatsapp");
        assert_eq!(payload["to"], "+6281234567890");
        assert_eq!(payload["type"], "text");
        assert_eq!(payload["text"]["body"], "Hello");
        assert_eq!(payload["text"]["preview_url"], false);
    }

    #[test]
    fn graph_payload_for_template_message() {
        let mut record = text_record();
        record.kind = "template".to_string();
        record.body = None;
        record.template_name = Some("order_paid".to_string());
        record.template_language = Some("en_US".to_string());
        record.template_variables = vec!["#123".to_string(), "Rp100.000".to_string()];
        let payload = build_graph_payload(&record);
        assert_eq!(payload["type"], "template");
        assert_eq!(payload["template"]["name"], "order_paid");
        assert_eq!(payload["template"]["language"]["code"], "en_US");
        let params = payload["template"]["components"][0]["parameters"]
            .as_array()
            .unwrap();
        assert_eq!(params.len(), 2);
        assert_eq!(params[0]["text"], "#123");
        assert_eq!(params[1]["text"], "Rp100.000");
    }

    #[test]
    fn inbound_tenant_resolution() {
        let state = WhatsAppState::new(WhatsAppConfig::from_env());
        assert_eq!(
            state.resolve_inbound_tenant("+6281234567890"),
            UNASSIGNED_TENANT
        );
        state.insert_message(text_record());
        assert_eq!(state.resolve_inbound_tenant("+6281234567890"), "tenant-a");
        assert_eq!(
            state.resolve_inbound_tenant("+9999999999"),
            UNASSIGNED_TENANT
        );
    }
}
