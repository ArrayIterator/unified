// network module -- Module definition, registration, and route table.

use unified_core::hooks::HookBus;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;

use super::*;

pub struct NetworkModule;

impl Module for NetworkModule {
    fn id(&self) -> &'static str {
        "network"
    }

    fn name(&self) -> &'static str {
        "Network"
    }

    fn description(&self) -> &'static str {
        "Network information, active connections, Geo lookups, and the DNS verifier library (A/AAAA/CNAME/PTR/MX/TXT, SPF, DKIM, DMARC, domain syntax with IDNA, IPv4/IPv6 preference, resolver health)."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["geo"]
    }

    fn register_hooks(&self, bus: &HookBus) {
        bus.register("dns.verify", "network", Arc::new(on_dns_verify));
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/network/info", owner: "network", description: "Network info", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network/geo-lookup", owner: "network", description: "Geo lookup handler", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/network/connections", owner: "network", description: "Connections handler", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/network/dns/check", owner: "network", description: "Dns check handler", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/network")
                .route("/info", web::get().to(network_info))
                .route("/geo-lookup", web::post().to(geo_lookup_handler))
                .route("/connections", web::get().to(connections_handler))
                .route("/dns/check", web::post().to(dns_check_handler)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(NetworkModule));
}
