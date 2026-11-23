//! Module registration: routes, hooks and metadata.

use unified_core::context::{AppContext};
use unified_core::error::{UnifiedResult};
use unified_core::hooks::{HookBus};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::sharing;
use crate::modules::ai;
use actix_web::{web};
use std::sync::{Arc};
use super::types::network_tool_events;
use super::store::{NETWORK_TOOLS_CTX, stash_ctx};
use super::handlers::{blacklist_handler, dns_handler, get_history, http_check_handler, ip_info_handler, list_history, list_tools, ping_handler, port_check_handler, share_history, ssl_check_handler, subnet_handler, traceroute_handler, uptime_check_handler, webhook_test_handler, whois_handler};
use super::handlers_saved::{create_saved_check, delete_saved_check, get_saved_check, list_saved_checks, run_saved_check, update_saved_check};
use super::handlers_monitors::{check_cert_watches_handler, check_uptime_monitor, create_cert_watch, create_uptime_monitor, delete_cert_watch, delete_uptime_monitor, get_uptime_monitor, list_cert_watches, list_uptime_events, list_uptime_monitors, update_uptime_monitor};
use super::hooks::{on_cert_check_requested, on_sharing_allow_check, on_uptime_check_requested};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct NetworkToolsModule;

impl Module for NetworkToolsModule {
    fn id(&self) -> &'static str {
        "network_tools"
    }

    fn name(&self) -> &'static str {
        "Network Tools"
    }

    fn description(&self) -> &'static str {
        "Developer network tools: DNS lookup, ping, traceroute, port check, SSL/TLS checker, WHOIS, HTTP checks, IP info and Geo, subnet calculator, webhook tester, uptime monitors, certificate expiry watch, and DNS blacklist checks, with saved checks, run history, and shareable results."
    }

    fn requires(&self) -> &[&'static str] {
        &["network", "geo", "certificates", "notifications", "api_manager"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &[
            "hickory-resolver",
            "reqwest",
            "tokio",
            "rustls",
            "x509-certificate",
            "chrono",
            "serde",
            "dashmap",
            "uuid",
            "diesel",
        ]
    }

    fn requires_external_libraries(&self) -> &[unified_core::modules::ExternalLibraryRequirement] {
        &[]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("infrastructure")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("network_tools", AI_CAPABILITIES);
        let _ = NETWORK_TOOLS_CTX.set(stash_ctx(ctx));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register(
            network_tool_events::UPTIME_CHECK_REQUESTED,
            "network_tools",
            Arc::new(on_uptime_check_requested),
        );
        bus.register(
            network_tool_events::CERT_CHECK_REQUESTED,
            "network_tools",
            Arc::new(on_cert_check_requested),
        );
        bus.register(
            unified_core::sharing::sharing_events::ALLOW_CHECK,
            "network_tools",
            Arc::new(on_sharing_allow_check),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/network-tools/tools", owner: "network_tools", description: "List available network tools and limits", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/dns", owner: "network_tools", description: "DNS lookup (A/AAAA/MX/TXT/CNAME/NS/SOA)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/ping", owner: "network_tools", description: "Ping with RTT and packet loss", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/traceroute", owner: "network_tools", description: "Traceroute hop list", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/port-check", owner: "network_tools", description: "TCP port check with optional banner grab", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/ssl-check", owner: "network_tools", description: "SSL/TLS checker (chain, expiry, protocol, cipher)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/whois", owner: "network_tools", description: "WHOIS lookup (domain or IP, one referral)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/http-check", owner: "network_tools", description: "HTTP header/status check with timings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/ip-info", owner: "network_tools", description: "IP info + Geo + reverse DNS", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/subnet", owner: "network_tools", description: "Subnet/CIDR calculator (IPv4 + IPv6)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/webhook-test", owner: "network_tools", description: "Webhook tester (POST JSON, no redirect following)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/uptime-check", owner: "network_tools", description: "One-off uptime check", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/blacklist", owner: "network_tools", description: "DNS blacklist check", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/network-tools/history", owner: "network_tools", description: "List tool run history (paginated, filtered)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/network-tools/history/{id}", owner: "network_tools", description: "Get one tool run", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/history/{id}/share", owner: "network_tools", description: "Share a run result via a public link scope", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/network-tools/saved-checks", owner: "network_tools", description: "List saved checks", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/saved-checks", owner: "network_tools", description: "Create a saved check", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/network-tools/saved-checks/{id}", owner: "network_tools", description: "Get one saved check", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/network-tools/saved-checks/{id}", owner: "network_tools", description: "Update a saved check", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/network-tools/saved-checks/{id}", owner: "network_tools", description: "Delete a saved check", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/saved-checks/{id}/run", owner: "network_tools", description: "Run a saved check", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/network-tools/uptime-monitors", owner: "network_tools", description: "List uptime monitors", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/uptime-monitors", owner: "network_tools", description: "Create an uptime monitor", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/network-tools/uptime-monitors/{id}", owner: "network_tools", description: "Get one uptime monitor", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/network-tools/uptime-monitors/{id}", owner: "network_tools", description: "Update an uptime monitor", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/network-tools/uptime-monitors/{id}", owner: "network_tools", description: "Delete an uptime monitor", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/uptime-monitors/{id}/check", owner: "network_tools", description: "Trigger one evaluation of a monitor now", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/network-tools/uptime-monitors/{id}/events", owner: "network_tools", description: "List a monitor's uptime events", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/network-tools/cert-watches", owner: "network_tools", description: "List certificate watches", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/cert-watches", owner: "network_tools", description: "Create a certificate watch", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/network-tools/cert-watches/{id}", owner: "network_tools", description: "Delete a certificate watch", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network-tools/cert-watches/check", owner: "network_tools", description: "Run the certificate expiry check now", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/network-tools")
                .route("/tools", web::get().to(list_tools))
                .route("/dns", web::post().to(dns_handler))
                .route("/ping", web::post().to(ping_handler))
                .route("/traceroute", web::post().to(traceroute_handler))
                .route("/port-check", web::post().to(port_check_handler))
                .route("/ssl-check", web::post().to(ssl_check_handler))
                .route("/whois", web::post().to(whois_handler))
                .route("/http-check", web::post().to(http_check_handler))
                .route("/ip-info", web::post().to(ip_info_handler))
                .route("/subnet", web::post().to(subnet_handler))
                .route("/webhook-test", web::post().to(webhook_test_handler))
                .route("/uptime-check", web::post().to(uptime_check_handler))
                .route("/blacklist", web::post().to(blacklist_handler))
                .route("/history", web::get().to(list_history))
                .route("/history/{id}", web::get().to(get_history))
                .route("/history/{id}/share", web::post().to(share_history))
                .route("/saved-checks", web::get().to(list_saved_checks))
                .route("/saved-checks", web::post().to(create_saved_check))
                .route("/saved-checks/{id}", web::get().to(get_saved_check))
                .route("/saved-checks/{id}", web::put().to(update_saved_check))
                .route("/saved-checks/{id}", web::delete().to(delete_saved_check))
                .route("/saved-checks/{id}/run", web::post().to(run_saved_check))
                .route("/uptime-monitors", web::get().to(list_uptime_monitors))
                .route("/uptime-monitors", web::post().to(create_uptime_monitor))
                .route("/uptime-monitors/{id}", web::get().to(get_uptime_monitor))
                .route("/uptime-monitors/{id}", web::put().to(update_uptime_monitor))
                .route("/uptime-monitors/{id}", web::delete().to(delete_uptime_monitor))
                .route("/uptime-monitors/{id}/check", web::post().to(check_uptime_monitor))
                .route("/uptime-monitors/{id}/events", web::get().to(list_uptime_events))
                .route("/cert-watches", web::get().to(list_cert_watches))
                .route("/cert-watches", web::post().to(create_cert_watch))
                .route("/cert-watches/{id}", web::delete().to(delete_cert_watch))
                .route("/cert-watches/check", web::post().to(check_cert_watches_handler)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(NetworkToolsModule));
}
