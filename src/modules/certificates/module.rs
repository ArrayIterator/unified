//! Certificates - module struct, route table and registration.
//!
//! Part of the `certificates` module split; see `src/modules/certificates.rs`.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::hooks::HookContext;
use unified_core::hooks::events;
use unified_core::modules::CliCommandDef;
use unified_core::modules::CliRegistry;
use unified_core::modules::Module;
use unified_core::modules::ModuleRegistry;
use unified_core::modules::RouteInfo;
use crate::modules::ai;
use actix_web::web;
use clap::Arg;
use clap::ArgAction;
use clap::Command;
use std::sync::Arc;

use super::cert_events;
use super::cli::cli_check_expiry;
use super::cli::cli_list;
use super::cli::cli_renew;
use super::cli::cli_renew_all;
use super::cli::cli_revoke;
use super::cli::cli_validate;
use super::handlers::check_expiry_handler;
use super::handlers::force_renew_certificate;
use super::handlers::get_certificate;
use super::handlers::get_config;
use super::handlers::get_settings;
use super::handlers::install_certificate_handler;
use super::handlers::list_certificates;
use super::handlers::list_history;
use super::handlers::renew_all_handler;
use super::handlers::renew_certificate_handler;
use super::handlers::revoke_certificate_handler;
use super::handlers::rollback_certificate_handler;
use super::handlers::swap_certificate_handler;
use super::handlers::update_settings;
use super::handlers::validate_all;
use super::handlers::validate_certificate;
use super::hooks::on_cloudflare_recheck;
use super::hooks::on_ssl_completion;
use super::hooks::on_ssl_renew;
use super::mode_handlers::detect_cloudflare_handler;
use super::mode_handlers::get_cert_mode;
use super::mode_handlers::get_cf_ranges;
use super::mode_handlers::get_cloudflare_detection;
use super::mode_handlers::list_cert_modes;
use super::mode_handlers::list_mode_definitions;
use super::mode_handlers::recheck_cloudflare_handler;
use super::mode_handlers::refresh_cf_ranges_handler;
use super::mode_handlers::set_cert_mode;
use super::mode_handlers::update_mode_definitions;
use super::mode_handlers::upload_custom_cert;
use super::state::CertificatesState;
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

pub struct CertificatesModule {
    state: Arc<CertificatesState>,
}

impl CertificatesModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(CertificatesState::new()),
        }
    }
}

impl Default for CertificatesModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CertificatesModule {
    fn id(&self) -> &'static str {
        "certificates"
    }

    fn name(&self) -> &'static str {
        "Certificates"
    }

    fn description(&self) -> &'static str {
        "Advanced SSL certificate management: config-first ACME certificates (multi-provider, EAB, HTTP-01), custom certificate install, swap with rollback, revocation, renewal history, and expiry alerts. Per-domain certificate modes from an admin-extensible registry (letsencrypt / custom / cloudflare), Cloudflare detection (DNS ranges, live probe, API check) with Flexible-mode warnings, and dynamic SSL swap without restart."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["scheduler", "notifications", "domains"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("certificates", AI_CAPABILITIES);
        let config = ctx.config.read().expect("config lock poisoned").clone();
        // Prefer the shared storage the HTTP server attached to the
        // context; `ctx.tls()` is `None` at init time in the normal
        // startup order (module init runs before `run_server` builds
        // the storage), so seeding falls back to its own lenient
        // storage there — see `seed_from_config`.
        self.state
            .seed_from_config(&config, ctx.hooks.clone(), ctx.tls());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            events::SSL_RENEW,
            "certificates",
            Arc::new(move |ctx: &mut HookContext| on_ssl_renew(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            cert_events::SSL_CF_RECHECK,
            "certificates",
            Arc::new(move |ctx: &mut HookContext| on_cloudflare_recheck(state.clone(), ctx)),
        );
        for event in [
            events::SSL_SWAP,
            cert_events::SSL_RENEWED,
            cert_events::SSL_SWAPPED,
        ] {
            let state = self.state.clone();
            bus.register(
                event,
                "certificates",
                Arc::new(move |ctx: &mut HookContext| on_ssl_completion(state.clone(), ctx)),
            );
        }
        // Answer the cloud module's SSL-provider catalogue hook with
        // the ACME providers (plus the Cloudflare origin mode, which
        // is not an ACME provider and carries no directory URL).
        // Read-only; extraction of this module to external "cloud
        // panel" modules is later work.
        bus.register(
            crate::modules::cloud::hooks::EVENT_CLOUD_SSL_PROVIDER_DEFINITIONS,
            "certificates",
            Arc::new(|ctx: &mut HookContext| {
                ctx.add_result(serde_json::json!([
                    {
                        "id": "letsencrypt",
                        "display_name": "Let's Encrypt",
                        "acme_directory_url": crate::http::acme::LETS_ENCRYPT_URL
                    },
                    {
                        "id": "google",
                        "display_name": "Google Trust Services",
                        "acme_directory_url": crate::http::acme::GOOGLE_URL
                    },
                    {
                        "id": "zerossl",
                        "display_name": "ZeroSSL",
                        "acme_directory_url": crate::http::acme::ZEROSSL_URL
                    },
                    {
                        "id": "buypass",
                        "display_name": "BuyPass",
                        "acme_directory_url": crate::http::acme::BUYPASS_URL
                    },
                    { "id": "custom", "display_name": "Custom ACME" },
                    {
                        "id": "cloudflare",
                        "display_name": "Cloudflare (origin certificate)"
                    },
                ]));
            }),
        );
    }

    fn register_cli(&self, registry: &mut CliRegistry) {
        registry.register(CliCommandDef {
            name: "cert-list",
            about: "List certificates with expiry, issuer, SANs, and status",
            command: Command::new("cert-list")
                .about("List certificates with expiry, issuer, SANs, and status"),
            handler: Arc::new(|matches| cli_list(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-validate",
            about: "Validate one certificate (or every configured certificate)",
            command: Command::new("cert-validate")
                .about("Validate one certificate (or every configured certificate)")
                .arg(
                    Arg::new("domain")
                        .value_name("DOMAIN")
                        .required(false)
                        .help("Domain to validate (validates every configured certificate when omitted)"),
                ),
            handler: Arc::new(|matches| cli_validate(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-renew",
            about: "Renew one domain certificate now (ACME)",
            command: Command::new("cert-renew")
                .about("Renew one domain certificate now (ACME)")
                .arg(
                    Arg::new("domain")
                        .value_name("DOMAIN")
                        .required(true)
                        .help("Domain whose certificate to renew"),
                )
                .arg(
                    Arg::new("force")
                        .long("force")
                        .action(ArgAction::SetTrue)
                        .help("Renew regardless of expiry"),
                ),
            handler: Arc::new(|matches| cli_renew(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-renew-all",
            about: "Renew every certificate that is due (ACME)",
            command: Command::new("cert-renew-all")
                .about("Renew every certificate that is due (ACME)")
                .arg(
                    Arg::new("force")
                        .long("force")
                        .action(ArgAction::SetTrue)
                        .help("Renew every certificate regardless of expiry"),
                ),
            handler: Arc::new(|matches| cli_renew_all(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-revoke",
            about: "Revoke a certificate (stops serving it; falls back to self-signed)",
            command: Command::new("cert-revoke")
                .about("Revoke a certificate (stops serving it; falls back to self-signed)")
                .arg(
                    Arg::new("domain")
                        .value_name("DOMAIN")
                        .required(true)
                        .help("Domain whose certificate to revoke"),
                )
                .arg(
                    Arg::new("reason")
                        .long("reason")
                        .value_name("REASON")
                        .required(false)
                        .help("Reason for the revocation"),
                ),
            handler: Arc::new(|matches| cli_revoke(matches)),
        });
        registry.register(CliCommandDef {
            name: "cert-check-expiry",
            about: "Run the expiry check now (emits expiry alerts)",
            command: Command::new("cert-check-expiry")
                .about("Run the expiry check now (emits expiry alerts)"),
            handler: Arc::new(|matches| cli_check_expiry(matches)),
        });
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/certificates")
                .route("", web::get().to(list_certificates))
                .route("/config", web::get().to(get_config))
                .route("/validate", web::get().to(validate_all))
                .route("/history", web::get().to(list_history))
                .route("/install", web::post().to(install_certificate_handler))
                .route("/renew-all", web::post().to(renew_all_handler))
                .route("/check-expiry", web::post().to(check_expiry_handler))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/cert-modes", web::get().to(list_cert_modes))
                .route(
                    "/cert-modes/definitions",
                    web::get().to(list_mode_definitions),
                )
                .route(
                    "/cert-modes/definitions",
                    web::put().to(update_mode_definitions),
                )
                .route("/cloudflare-ranges", web::get().to(get_cf_ranges))
                .route(
                    "/cloudflare-ranges/refresh",
                    web::post().to(refresh_cf_ranges_handler),
                )
                .route("/{domain}", web::get().to(get_certificate))
                .route("/{domain}/validate", web::post().to(validate_certificate))
                .route("/{domain}/renew", web::post().to(renew_certificate_handler))
                .route(
                    "/{domain}/force-renew",
                    web::post().to(force_renew_certificate),
                )
                .route(
                    "/{domain}/revoke",
                    web::post().to(revoke_certificate_handler),
                )
                .route("/{domain}/swap", web::post().to(swap_certificate_handler))
                .route(
                    "/{domain}/rollback",
                    web::post().to(rollback_certificate_handler),
                )
                .route("/{domain}/cert-mode", web::get().to(get_cert_mode))
                .route("/{domain}/cert-mode", web::put().to(set_cert_mode))
                .route(
                    "/{domain}/cert-mode/custom-upload",
                    web::post().to(upload_custom_cert),
                )
                .route(
                    "/{domain}/detect-cloudflare",
                    web::post().to(detect_cloudflare_handler),
                )
                .route(
                    "/{domain}/cloudflare-detection",
                    web::get().to(get_cloudflare_detection),
                )
                .route(
                    "/{domain}/cloudflare-detection/recheck",
                    web::post().to(recheck_cloudflare_handler),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/certificates",
                owner: "certificates",
                description: "List certificates with expiry, issuer, SANs, and status",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/certificates/config",
                owner: "certificates",
                description: "Effective certificate config, provider matrix, and validation",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/certificates/validate",
                owner: "certificates",
                description: "Validate every configured certificate",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/certificates/history",
                owner: "certificates",
                description: "Renewal history",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/install",
                owner: "certificates",
                description: "Install a custom certificate (validated first)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/renew-all",
                owner: "certificates",
                description: "Renew every certificate that is due",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/check-expiry",
                owner: "certificates",
                description: "Run the expiry check now",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/certificates/settings",
                owner: "certificates",
                description: "Runtime certificate settings (secrets redacted)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/certificates/settings",
                owner: "certificates",
                description: "Update runtime certificate settings",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/certificates/{domain}",
                owner: "certificates",
                description: "One certificate: detail plus recent history",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/{domain}/validate",
                owner: "certificates",
                description: "Test and validate one certificate",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/{domain}/renew",
                owner: "certificates",
                description: "Renew one certificate now",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/{domain}/force-renew",
                owner: "certificates",
                description: "Force-renew one certificate",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/{domain}/revoke",
                owner: "certificates",
                description: "Revoke one certificate",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/{domain}/swap",
                owner: "certificates",
                description: "Swap the live certificate (validated first, rollback on failure)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/{domain}/rollback",
                owner: "certificates",
                description: "Restore the previous live certificate",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/certificates/cert-modes",
                owner: "certificates",
                description: "List per-domain certificate modes (paginated, tenant-scoped)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/certificates/cert-modes/definitions",
                owner: "certificates",
                description: "Certificate-mode registry definitions",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/certificates/cert-modes/definitions",
                owner: "certificates",
                description: "Replace the certificate-mode registry (admin)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/certificates/cloudflare-ranges",
                owner: "certificates",
                description: "Stored Cloudflare IP-range snapshot with fetch date",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/cloudflare-ranges/refresh",
                owner: "certificates",
                description: "Refresh the Cloudflare IP-range snapshot from the published lists",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/certificates/{domain}/cert-mode",
                owner: "certificates",
                description: "One domain's certificate mode plus mode history",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/certificates/{domain}/cert-mode",
                owner: "certificates",
                description: "Set a domain's certificate mode (ACME issuance for letsencrypt modes)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/{domain}/cert-mode/custom-upload",
                owner: "certificates",
                description: "Upload a custom certificate for a domain (validated first, rollback on failure)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/{domain}/detect-cloudflare",
                owner: "certificates",
                description: "Detect Cloudflare usage for a domain (DNS ranges, live probe, API check)",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/certificates/{domain}/cloudflare-detection",
                owner: "certificates",
                description: "Stored Cloudflare detection result for a domain",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/certificates/{domain}/cloudflare-detection/recheck",
                owner: "certificates",
                description: "Re-run Cloudflare detection for a domain",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CertificatesModule::new()));
}
