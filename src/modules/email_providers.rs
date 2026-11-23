// Email provider registry.

// Split into one file per concern under `src/modules/email_providers/`;
// this root only declares the submodules and re-exports the
// public API so every item stays reachable as `crate::modules::email_providers::Item`.

pub mod events;
pub mod provider_kinds;
pub mod credentials;
pub mod store;
mod http;
pub mod registry;
pub mod adapters;
mod worker;
mod hooks;
pub mod handlers;
pub mod module;

pub use self::events::{EVENT_EMAIL_FAILED, EVENT_EMAIL_PROVIDER_CONNECTED, EVENT_EMAIL_PROVIDER_CONNECTION_CREATED, EVENT_EMAIL_PROVIDER_CONNECTION_DELETED, EVENT_EMAIL_PROVIDER_WEBHOOK_VERIFY, EVENT_EMAIL_SEND_REQUESTED, EVENT_EMAIL_SENT, EVENT_MAIL_SEND_REQUESTED, EVENT_TEMPLATE_RENDER_REQUESTED};
pub(crate) use self::events::{DEFAULT_CONNECT_TIMEOUT_SECS, DEFAULT_TIMEOUT_SECS, MAX_BATCH_RECIPIENTS, MAX_RETRY_AFTER, USER_AGENT};
pub use self::provider_kinds::{ConnectionTestResult, EmailCapabilities, EmailProviderKind, SettingsFieldDef, capabilities_for};
pub(crate) use self::provider_kinds::{settings_fields};
pub use self::credentials::{CreateConnectionRequest, EmailConnection, EmailConnectionView, EmailCredentials, UpdateConnectionRequest};
pub(crate) use self::credentials::{validate_connection};
pub use self::store::{EmailMessage, SendLogEntry, SendResult, default_connection, get_connection, resolve_connection};
pub(crate) use self::store::{CONNECTIONS, DEFAULT_CONNECTION, QueuedSend, SEND_LOG, append_log, connections_store, default_connection_store, first_enabled_connection, send_log_store, update_log};
pub(crate) use self::http::{HmacSha256, SigV4Params, build_client, check_allowlist, hex_encode, hmac_sha256, http_status_error, parse_retry_after, send_json, send_text, sha256_hex, signatures_equal, sigv4_authorization, simulated_result, validate_email_address};
pub use self::registry::{EmailProvider, EmailProviderFactory, EmailProviderRegistry, email_provider_registry};
pub(crate) use self::registry::{EMAIL_HOOKS, REGISTRY, email_hooks, email_hooks_store, emit_event, require_api_key, unsupported};
pub use self::adapters::{BrevoAdapter, MailchimpAdapter, MailgunAdapter, PostmarkAdapter, ResendAdapter, SendGridAdapter, SesAdapter, SmtpAdapter, SparkPostAdapter};
pub(crate) use self::adapters::{xml_tag};
pub(crate) use self::worker::{render_template, run_send_job, spawn_send_worker};
pub(crate) use self::hooks::{on_email_send_requested, on_webhook_verify};
pub use self::handlers::{EmailProviderState};
pub(crate) use self::handlers::{SendBatchInput, SendInput, SetDefaultInput, create_connection, delete_connection, get_connection_handler, get_default_connection, list_catalog, list_connections, list_send_log, paginate_connections, parse_connection_id, send_batch_handler, send_input_to_message, send_mail_handler, set_default_connection, test_connection_handler, update_connection};
pub use self::module::{EmailProvidersModule, register};
pub(crate) use self::module::{TestSendInput, test_connection_send};
