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

pub mod ai;
pub mod ai_support;
pub mod analytics;
pub mod announcements;
pub mod api_manager;
pub mod apikeys;
pub mod app_sync;
pub mod audit;
pub mod auth_manager;
pub mod backup;
pub mod bank_transfer;
pub mod billing;
pub mod cache;
pub mod certificates;
pub mod chat;
pub mod cloudflare;
pub mod clusters;
pub mod company;
pub mod content;
pub mod crm;
pub mod dashboards;
pub mod departments;
pub mod domains;
pub mod downloader;
pub mod email_providers;
pub mod email_templates;
pub mod extensions;
pub mod files;
pub mod geo;
pub mod hosting;
pub mod i18n;
pub mod import_export;
pub mod knowledgebase;
pub mod licensing;
pub mod load_balancer;
pub mod logs;
pub mod mail;
pub mod mcp;
pub mod maintenance;
pub mod meetings;
pub mod network;
pub mod newsletter;
pub mod notifications;
pub mod panels;
pub mod payments;
pub mod planner;
pub mod products;
pub mod providers;
pub mod records;
pub mod roles;
pub mod routes;
pub mod saas;
pub mod saved_views;
pub mod scheduler;
pub mod search;
pub mod security;
pub mod seo;
pub mod service_monitor;
pub mod settings;
pub mod site_builder;
pub mod software;
pub mod statuspage;
pub mod storage;
pub mod store;
pub mod tasks;
pub mod theme;
pub mod tickets;
pub mod timeline;
pub mod users;
pub mod vcs;
pub mod webhooks;

use crate::core::modules::ModuleRegistry;

/// Register every core module, in dependency order.
pub fn register_all(registry: &ModuleRegistry) {
    // Foundation.
    settings::register(registry);
    cache::register(registry);
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
    // Infrastructure: storage, observability, scheduling, and the
    // platform services the other groups build on.
    storage::register(registry);
    logs::register(registry);
    tasks::register(registry);
    records::register(registry);
    search::register(registry);
    timeline::register(registry);
    saved_views::register(registry);
    backup::register(registry);
    clusters::register(registry);
    load_balancer::register(registry);
    hosting::register(registry);
    panels::register(registry);
    providers::register(registry);
    cloudflare::register(registry);
    maintenance::register(registry);
    meetings::register(registry);
    routes::register(registry);
    dashboards::register(registry);
    analytics::register(registry);
    planner::register(registry);
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
    email_providers::register(registry);
    newsletter::register(registry);
    // Commerce.
    store::register(registry);
    licensing::register(registry);
    software::register(registry);
    billing::register(registry);
    products::register(registry);
    payments::register(registry);
    bank_transfer::register(registry);
    // Operations.
    company::register(registry);
    domains::register(registry);
    certificates::register(registry);
    network::register(registry);
    geo::register(registry);
    downloader::register(registry);
    vcs::register(registry);
    app_sync::register(registry);
    statuspage::register(registry);
    // People and support.
    crm::register(registry);
    departments::register(registry);
    tickets::register(registry);
    chat::register(registry);
    ai_support::register(registry);
    ai::register(registry);
    files::register(registry);
    announcements::register(registry);
    // Platform: data movement and inbound webhooks.
    import_export::register(registry);
    webhooks::register(registry);
    // Public API surface.
    api_manager::register(registry);
    apikeys::register(registry);
    mcp::register(registry);
}
