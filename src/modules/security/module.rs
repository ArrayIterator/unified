// security -- Module struct, Module impl and registration.

use unified_core::hardening::shared_state;
use unified_core::hooks::{HookBus, events};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use crate::modules::ai;
use actix_web::web;
use dashmap::DashMap;
use std::sync::Arc;

use super::handlers::*;
use super::handlers_hardening::*;
use super::scan::*;
use super::ai_capabilities::{AI_CAPABILITIES};

pub struct SecurityModule {
    /// Failed-login attempts per IP, fed by the `auth.failed` hook.
    failed_logins: Arc<DashMap<String, u64>>,
}

impl SecurityModule {
    pub fn new() -> Self {
        Self {
            failed_logins: Arc::new(DashMap::new()),
        }
    }

    pub(crate) fn record_failed_login(&self, ip: &str) {
        let mut count = self.failed_logins.entry(ip.to_string()).or_insert(0);
        *count += 1;
    }
}

/// Remove the per-IP failed-login counter for `ip`. Shared by the
/// AUTH_LOGIN hook (successful login) and the brute-force unlock
/// handler, so a cleared IP stops being flagged in GET
/// /failed-logins and in the auth_failures check. Returns true when
/// an entry was present.
pub(crate) fn clear_failed_login_entry(failed_logins: &DashMap<String, u64>, ip: &str) -> bool {
    failed_logins.remove(ip).is_some()
}

impl Default for SecurityModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SecurityModule::new()));
}

impl Module for SecurityModule {
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("security", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "security"
    }

    fn name(&self) -> &'static str {
        "Security"
    }

    fn description(&self) -> &'static str {
        "Runs security checks, exposes the password policy, scans targets for TLS presence, and serves the hardening surface: brute-force status and unlock, rate-limit status, IP allow/block lists, security headers, and failed-auth alerts. Every endpoint requires a valid Auth Manager JWT and a security capability (`security:view` for reads, `security:manage` for writes). Cookie-authenticated writes additionally require a CSRF token from `GET /api/v1/security/csrf-token`."
    }

    fn register_hooks(&self, bus: &HookBus) {
        let failed_logins = self.failed_logins.clone();
        bus.register(
            events::AUTH_FAILED,
            "security",
            Arc::new(move |context| {
                // The auth manager emits `auth.failed` without an IP on the
                // token-verification path and with `ip` / `user_id` on the
                // login paths; feed the hardening engine with whatever the
                // payload carries so repeated failures still cluster.
                let ip = context
                    .get("ip")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                let account = context
                    .get("user_id")
                    .or_else(|| context.get("account_id"))
                    .or_else(|| context.get("email"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                if let Some(ip) = &ip {
                    let mut count = failed_logins.entry(ip.clone()).or_insert(0);
                    *count += 1;
                }
                shared_state().record_login_failure(ip.as_deref(), account.as_deref());
            }),
        );
        // A successful login resets the brute-force counters for the IP
        // and the account, so earlier failures never lock out a
        // legitimate user. The auth manager emits `auth.login` with
        // `user_id` on both the JWT issue path and the Google SSO path.
        let failed_logins_on_login = self.failed_logins.clone();
        bus.register(
            events::AUTH_LOGIN,
            "security",
            Arc::new(move |context| {
                let ip = context
                    .get("ip")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                let account = context
                    .get("user_id")
                    .or_else(|| context.get("account_id"))
                    .or_else(|| context.get("email"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                // Clear this module's own per-IP failed-login counter
                // as well as the hardening counters: without this the
                // counter only ever grew, and an IP that once reached
                // the flag threshold stayed flagged in
                // GET /failed-logins and in the auth_failures check
                // forever (until restart).
                if let Some(ip) = &ip {
                    clear_failed_login_entry(&failed_logins_on_login, ip.as_str());
                }
                shared_state().record_login_success(ip.as_deref(), account.as_deref());
            }),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/security/csrf-token",
                owner: "security",
                description: "CSRF token",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/security/checks",
                owner: "security",
                description: "Checks",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/security/policy",
                owner: "security",
                description: "Policy",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/security/scan",
                owner: "security",
                description: "Scan",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/security/failed-logins",
                owner: "security",
                description: "Failed logins",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/security/hardening/settings",
                owner: "security",
                description: "`GET /api/v1/security/hardening/settings`: the live hardening settings plus live counts (list sizes, current lockouts, flagged IPs, failed-auth clusters).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/security/hardening/settings",
                owner: "security",
                description: "`PUT /api/v1/security/hardening/settings`: partial settings update.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/security/hardening/brute-force",
                owner: "security",
                description: "`GET /api/v1/security/hardening/brute-force`: paginated lockout / attempt status per IP and per account.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/security/hardening/brute-force/unlock",
                owner: "security",
                description: "`POST /api/v1/security/hardening/brute-force/unlock`: manually clear a lockout / attempt record for one IP (`kind: \"ip\"`) or one account (`kind: \"account\"`).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/security/hardening/rate-limit",
                owner: "security",
                description: "`GET /api/v1/security/hardening/rate-limit`: paginated tracked rate-limit windows.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/security/hardening/ip-lists",
                owner: "security",
                description: "`GET /api/v1/security/hardening/ip-lists`: paginated allow/block list entries.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/security/hardening/ip-lists",
                owner: "security",
                description: "`POST /api/v1/security/hardening/ip-lists`: add one CIDR range or bare IP to the allowlist or the blocklist.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/security/hardening/ip-lists",
                owner: "security",
                description: "`DELETE /api/v1/security/hardening/ip-lists?list=allow&cidr=...`: remove one entry from the allowlist or the blocklist.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/security/hardening/headers",
                owner: "security",
                description: "`GET /api/v1/security/hardening/headers`: the configured security header values (empty means that header is disabled).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/security/hardening/headers",
                owner: "security",
                description: "`PUT /api/v1/security/hardening/headers`: partial header update.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/security/hardening/failed-auth-alerts",
                owner: "security",
                description: "`GET /api/v1/security/hardening/failed-auth-alerts`: paginated failed-auth clusters (repeated failures grouped by IP + account), most recently active first.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/security/hardening/reputation",
                owner: "security",
                description: "`GET /api/v1/security/hardening/reputation`: paginated IP reputation records, highest score first.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/security/hardening/reputation/flag",
                owner: "security",
                description: "`POST /api/v1/security/hardening/reputation/flag`: manually flag an IP with a reason.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/security/hardening/reputation/unflag",
                owner: "security",
                description: "`POST /api/v1/security/hardening/reputation/unflag`: manually unflag an IP and reset its score.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/security")
                .app_data(web::Data::new(self.failed_logins.clone()))
                .route("/csrf-token", web::get().to(csrf_token))
                .route("/checks", web::get().to(checks))
                .route("/policy", web::get().to(policy))
                .route("/scan", web::post().to(scan))
                .route("/failed-logins", web::get().to(failed_logins))
                .route("/hardening/settings", web::get().to(get_hardening_settings))
                .route(
                    "/hardening/settings",
                    web::put().to(update_hardening_settings),
                )
                .route(
                    "/hardening/brute-force",
                    web::get().to(list_brute_force_status),
                )
                .route(
                    "/hardening/brute-force/unlock",
                    web::post().to(unlock_brute_force),
                )
                .route(
                    "/hardening/rate-limit",
                    web::get().to(list_rate_limit_status),
                )
                .route("/hardening/ip-lists", web::get().to(list_ip_entries))
                .route("/hardening/ip-lists", web::post().to(add_ip_entry))
                .route("/hardening/ip-lists", web::delete().to(remove_ip_entry))
                .route("/hardening/headers", web::get().to(get_headers_config))
                .route("/hardening/headers", web::put().to(update_headers_config))
                .route(
                    "/hardening/failed-auth-alerts",
                    web::get().to(list_failed_auth_alerts),
                )
                .route("/hardening/reputation", web::get().to(list_reputation))
                .route(
                    "/hardening/reputation/flag",
                    web::post().to(flag_reputation),
                )
                .route(
                    "/hardening/reputation/unflag",
                    web::post().to(unflag_reputation),
                ),
        );
    }
}

// Tests for the per-IP failed-login counter: it must be cleared by a
// successful login and by a manual unlock, never grow forever.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::security::FAILED_LOGIN_FLAG_THRESHOLD;

    fn count(module: &SecurityModule, ip: &str) -> u64 {
        module
            .failed_logins
            .get(ip)
            .map(|entry| *entry)
            .unwrap_or(0)
    }

    #[test]
    fn failed_logins_accumulate_per_ip() {
        let module = SecurityModule::new();
        module.record_failed_login("203.0.113.7");
        module.record_failed_login("203.0.113.7");
        assert_eq!(count(&module, "203.0.113.7"), 2);
        assert_eq!(count(&module, "198.51.100.9"), 0);
    }

    #[test]
    fn successful_login_clears_failed_logins() {
        let module = SecurityModule::new();
        for _ in 0..FAILED_LOGIN_FLAG_THRESHOLD + 2 {
            module.record_failed_login("203.0.113.7");
        }
        assert_eq!(
            count(&module, "203.0.113.7"),
            FAILED_LOGIN_FLAG_THRESHOLD + 2
        );
        // This is the same clearing the AUTH_LOGIN hook performs.
        assert!(clear_failed_login_entry(
            &module.failed_logins,
            "203.0.113.7"
        ));
        assert_eq!(count(&module, "203.0.113.7"), 0);
    }

    #[test]
    fn clearing_unknown_ip_is_a_noop() {
        let module = SecurityModule::new();
        assert!(!clear_failed_login_entry(
            &module.failed_logins,
            "203.0.113.7"
        ));
    }

    #[test]
    fn clearing_one_ip_leaves_other_ips_flagged() {
        let module = SecurityModule::new();
        for _ in 0..FAILED_LOGIN_FLAG_THRESHOLD + 2 {
            module.record_failed_login("203.0.113.7");
        }
        for _ in 0..3 {
            module.record_failed_login("198.51.100.9");
        }
        // This is the same clearing the brute-force unlock handler
        // performs for an IP unlock.
        assert!(clear_failed_login_entry(
            &module.failed_logins,
            "203.0.113.7"
        ));
        assert_eq!(count(&module, "203.0.113.7"), 0);
        assert_eq!(count(&module, "198.51.100.9"), 3);
    }
}
