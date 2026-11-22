// Core modules.
//
// Every module in this directory is registered in the binary, is fully
// hookable, and exposes its HTTP routes under `/api/v1/<module-id>`.
// Each module file exposes `register(registry)` and a `*Module` struct
// implementing `crate::core::modules::Module`.
//
// Interconnection between modules happens through the hook/event bus — no
// module calls another module directly. See `crate::core::hooks::events`
// for the well-known event names.

pub mod ai_support;
pub mod api_manager;
pub mod app_sync;
pub mod audit;
pub mod auth_manager;
pub mod billing;
pub mod chat;
pub mod company;
pub mod content;
pub mod crm;
pub mod domains;
pub mod downloader;
pub mod email_templates;
pub mod extensions;
pub mod files;
pub mod geo;
pub mod i18n;
pub mod knowledgebase;
pub mod licensing;
pub mod mail;
pub mod network;
pub mod notifications;
pub mod roles;
pub mod saas;
pub mod scheduler;
pub mod security;
pub mod seo;
pub mod service_monitor;
pub mod settings;
pub mod site_builder;
pub mod software;
pub mod store;
pub mod theme;
pub mod tickets;
pub mod users;

use crate::core::modules::ModuleRegistry;

/// Register every core module, in dependency order.
pub fn register_all(registry: &ModuleRegistry) {
    // Foundation.
    settings::register(registry);
    users::register(registry);
    roles::register(registry);
    security::register(registry);
    extensions::register(registry);
    service_monitor::register(registry);
    audit::register(registry);
    scheduler::register(registry);
    notifications::register(registry);
    // Identity and access.
    auth_manager::register(registry);
    // Content and presentation.
    content::register(registry);
    seo::register(registry);
    i18n::register(registry);
    theme::register(registry);
    site_builder::register(registry);
    saas::register(registry);
    knowledgebase::register(registry);
    mail::register(registry);
    email_templates::register(registry);
    // Commerce.
    store::register(registry);
    licensing::register(registry);
    software::register(registry);
    billing::register(registry);
    // Operations.
    company::register(registry);
    domains::register(registry);
    network::register(registry);
    geo::register(registry);
    downloader::register(registry);
    app_sync::register(registry);
    // People and support.
    crm::register(registry);
    tickets::register(registry);
    chat::register(registry);
    ai_support::register(registry);
    files::register(registry);
    // Public API surface.
    api_manager::register(registry);
}
