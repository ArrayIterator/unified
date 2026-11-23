// Core modules.
//
// Every module in this directory is registered in the binary, is fully
// hookable, and exposes its HTTP routes under `/api/v1/<module-id>`.
// Each module file exposes `register(registry)` and a `*Module` struct
// implementing `unified_core::modules::Module`.
//
// Interconnection between modules happens through the hook/event bus — no
// module calls another module directly. See `unified_core::hooks::events`
// for the well-known event names.

pub mod academic;
pub mod admissions;
pub mod advisor;
pub mod affiliates;
pub mod ai;
pub mod ai_support;
pub mod analytics;
pub mod announcements;
pub mod api_manager;
pub mod apikeys;
pub mod app_sync;
pub mod appointments;
pub mod approvals;
pub mod assets;
pub mod audit;
pub mod auditors;
pub mod auth_manager;
pub mod backup;
pub mod bank_transfer;
pub mod bi;
pub mod billing;
pub mod cache;
pub mod calendar;
pub mod cards;
pub mod certificates;
pub mod chat;
pub mod cloudflare;
pub mod clusters;
pub mod community;
pub mod company;
pub mod compliance;
pub mod content;
pub mod contracts;
pub mod crm;
pub mod curriculum;
pub mod dashboards;
pub mod data_tools;
pub mod delivery_notes;
pub mod departments;
pub mod documents;
pub mod domain_verification;
pub mod domains;
pub mod downloader;
pub mod elearning;
pub mod email_providers;
pub mod email_templates;
pub mod entitlements;
pub mod esg;
pub mod events;
pub mod exams;
pub mod expenses;
pub mod extensions;
pub mod field_service;
pub mod files;
pub mod forms;
pub mod forums;
pub mod frontend;
pub mod gamification;
pub mod geo;
pub mod holidays;
pub mod hosting;
pub mod hr;
pub mod i18n;
pub mod import_export;
pub mod inbox;
pub mod incidents;
pub mod knowledgebase;
pub mod kyc;
pub mod library;
pub mod licensing;
pub mod load_balancer;
pub mod logistics;
pub mod logs;
pub mod loyalty;
pub mod mail;
pub mod maintenance;
pub mod manufacturing;
pub mod marketing;
pub mod mcp;
pub mod meetings;
pub mod network;
pub mod network_tools;
pub mod newsletter;
pub mod notifications;
pub mod okrs;
pub mod panels;
pub mod payments;
pub mod phone;
pub mod planner;
pub mod plm;
pub mod portals;
pub mod pos;
pub mod presets;
pub mod pricing;
pub mod pricing_tables;
pub mod printing;
pub mod privacy;
pub mod procurement;
pub mod products;
pub mod projects;
pub mod properties;
pub mod providers;
pub mod quality;
pub mod records;
pub mod recruitment;
pub mod referral;
pub mod rental;
pub mod risk;
pub mod roles;
pub mod routes;
pub mod saas;
pub mod saas_domains;
pub mod saas_packages;
pub mod saved_views;
pub mod scheduler;
pub mod search;
pub mod security;
pub mod seo;
pub mod service_monitor;
pub mod settings;
pub mod sign;
pub mod site_builder;
pub mod sms_marketing;
pub mod social_marketing;
pub mod software;
pub mod spreadsheets;
pub mod statuspage;
pub mod storage;
pub mod storage_manager;
pub mod store;
pub mod studio;
pub mod supervision;
pub mod surveys;
pub mod tasks;
pub mod tax;
pub mod tenant_destroy;
pub mod tenants;
pub mod theme;
pub mod tickets;
pub mod timeline;
pub mod timetracker;
pub mod todos;
pub mod users;
pub mod vault;
pub mod vcs;
pub mod web3;
pub mod webhooks;
pub mod webinars;
pub mod whitelabel;
pub mod widgets;
pub mod workflows;

use unified_core::modules::ModuleRegistry;

/// Register every core module, in dependency order.
pub fn register_all(registry: &ModuleRegistry) {
    // Foundation.
    settings::register(registry);
    cache::register(registry);
    users::register(registry);
    roles::register(registry);
    security::register(registry);
    vault::register(registry);
    extensions::register(registry);
    service_monitor::register(registry);
    audit::register(registry);
    scheduler::register(registry);
    notifications::register(registry);
    privacy::register(registry);
    // Identity and access.
    auth_manager::register(registry);
    tenants::register(registry);
    tenant_destroy::register(registry);
    // Infrastructure: storage, observability, scheduling, and the
    // platform services the other groups build on.
    storage::register(registry);
    storage_manager::register(registry);
    logs::register(registry);
    tasks::register(registry);
    todos::register(registry);
    records::register(registry);
    search::register(registry);
    timeline::register(registry);
    saved_views::register(registry);
    backup::register(registry);
    // Data Tools answers the scheduler's `cleanup.requested` hook
    // and owns the cleanup rules + runs; it reaches other modules'
    // tables only through the `data_tools.cleanup.<target>` hook.
    data_tools::register(registry);
    clusters::register(registry);
    load_balancer::register(registry);
    hosting::register(registry);
    panels::register(registry);
    providers::register(registry);
    cloudflare::register(registry);
    maintenance::register(registry);
    meetings::register(registry);
    phone::register(registry);
    calendar::register(registry);
    appointments::register(registry);
    holidays::register(registry);
    routes::register(registry);
    // Widgets is a standalone core module: dashboards, the front
    // page, and the tenant dashboard consume it, so it registers
    // before them.
    widgets::register(registry);
    dashboards::register(registry);
    // Public pages (home and auth) live next to the dashboards they
    // lead into; their paths come from the same route registry.
    frontend::register(registry);
    analytics::register(registry);
    bi::register(registry);
    planner::register(registry);
    // Project Management registers here: it builds on the planner's
    // capacity data for scheduling and on the store/billing modules
    // for billable time.
    projects::register(registry);
    timetracker::register(registry);
    // Content and presentation.
    content::register(registry);
    seo::register(registry);
    i18n::register(registry);
    theme::register(registry);
    presets::register(registry);
    whitelabel::register(registry);
    site_builder::register(registry);
    studio::register(registry);
    saas::register(registry);
    saas_domains::register(registry);
    saas_packages::register(registry);
    knowledgebase::register(registry);
    documents::register(registry);
    forms::register(registry);
    events::register(registry);
    webinars::register(registry);
    mail::register(registry);
    email_templates::register(registry);
    email_providers::register(registry);
    newsletter::register(registry);
    marketing::register(registry);
    sms_marketing::register(registry);
    social_marketing::register(registry);
    // Education and learning.
    academic::register(registry);
    admissions::register(registry);
    advisor::register(registry);
    curriculum::register(registry);
    elearning::register(registry);
    exams::register(registry);
    library::register(registry);
    // Commerce.
    store::register(registry);
    licensing::register(registry);
    entitlements::register(registry);
    software::register(registry);
    billing::register(registry);
    tax::register(registry);
    products::register(registry);
    pricing::register(registry);
    pricing_tables::register(registry);
    payments::register(registry);
    bank_transfer::register(registry);
    cards::register(registry);
    affiliates::register(registry);
    expenses::register(registry);
    pos::register(registry);
    loyalty::register(registry);
    rental::register(registry);
    // Business operations.
    company::register(registry);
    contracts::register(registry);
    sign::register(registry);
    spreadsheets::register(registry);
    kyc::register(registry);
    assets::register(registry);
    field_service::register(registry);
    logistics::register(registry);
    delivery_notes::register(registry);
    manufacturing::register(registry);
    plm::register(registry);
    procurement::register(registry);
    properties::register(registry);
    printing::register(registry);
    unified_calculator::register(registry);
    okrs::register(registry);
    supervision::register(registry);
    domains::register(registry);
    domain_verification::register(registry);
    certificates::register(registry);
    network::register(registry);
    network_tools::register(registry);
    geo::register(registry);
    downloader::register(registry);
    vcs::register(registry);
    app_sync::register(registry);
    statuspage::register(registry);
    incidents::register(registry);
    // Governance, risk and compliance.
    compliance::register(registry);
    esg::register(registry);
    auditors::register(registry);
    risk::register(registry);
    quality::register(registry);
    // Workflow and approvals.
    workflows::register(registry);
    approvals::register(registry);
    // People and support.
    crm::register(registry);
    departments::register(registry);
    hr::register(registry);
    recruitment::register(registry);
    referral::register(registry);
    tickets::register(registry);
    chat::register(registry);
    inbox::register(registry);
    community::register(registry);
    forums::register(registry);
    portals::register(registry);
    gamification::register(registry);
    surveys::register(registry);
    ai_support::register(registry);
    ai::register(registry);
    files::register(registry);
    announcements::register(registry);
    // Platform: data movement and inbound webhooks.
    import_export::register(registry);
    webhooks::register(registry);
    web3::register(registry);
    // Public API surface.
    api_manager::register(registry);
    apikeys::register(registry);
    mcp::register(registry);

    // External modules (see docs/WORKSPACE_AND_EXTERNAL_MODULES.md section 4).
    // Each line is one external crate's `register` function. An external
    // module is compiled into the binary; adding one is a code change,
    // a rebuild, and a restart -- never a runtime install.
    // The TypeScript scripting host registers itself here; the `.ts`
    // modules it loads register through it at startup, not here.
    unified_scripting::register(registry);
}
