//! eSignature external module (split from lib.rs; moved verbatim).

use super::*;


// ---------------------------------------------------------------------------
// Module implementation
// ---------------------------------------------------------------------------

/// The e-signature module. One struct holding the shared in-memory
/// state; all behaviour is in the `Module` implementation below.
pub struct EsignatureModule {
    state: Arc<EsignatureState>,
}

impl EsignatureModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(EsignatureState::new()),
        }
    }

    pub fn state(&self) -> Arc<EsignatureState> {
        self.state.clone()
    }
}

impl Default for EsignatureModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for EsignatureModule {
    fn id(&self) -> &'static str {
        MODULE_ID
    }

    fn name(&self) -> &'static str {
        "E-Signature"
    }

    fn version(&self) -> &'static str {
        "0.1.0"
    }

    fn description(&self) -> &'static str {
        "E-signature workflow, evidence, and audit layer: signature requests with ordered signers, HMAC-signed signing links, a hash-chained audit trail, and completion certificates. Does not render or modify PDF bytes."
    }

    fn is_core(&self) -> bool {
        false
    }

    /// The `contracts.signed` link-back needs the contracts module.
    /// Everything else in this module works without it: when the
    /// contracts module is absent the listener simply never fires.
    fn requires(&self) -> &[&'static str] {
        &["contracts"]
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_CONTRACTS_SIGNED,
            MODULE_ID,
            Arc::new(move |ctx: &mut HookContext| {
                let contract_id = ctx
                    .get("contract_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let content_hash = ctx
                    .get("content_hash")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if contract_id.is_empty() && content_hash.is_empty() {
                    return;
                }
                let linked = state.link_completed_requests(&contract_id, &content_hash);
                ctx.add_result(json!({
                    "module": "esignature",
                    "event": "contracts.signed",
                    "linked_requests": linked,
                }));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/esignature")
                .route("/requests", web::post().to(create_request_handler))
                .route("/requests", web::get().to(list_requests_handler))
                .route("/requests/{id}", web::get().to(get_request_handler))
                .route("/requests/{id}/send", web::post().to(send_request_handler))
                .route(
                    "/requests/{id}/remind",
                    web::post().to(remind_handler),
                )
                .route("/requests/{id}/audit", web::get().to(audit_handler))
                .route(
                    "/requests/{id}/audit/verify",
                    web::get().to(verify_audit_handler),
                )
                .route(
                    "/requests/{id}/certificate",
                    web::get().to(certificate_handler),
                )
                .route(
                    "/verify/{certificate_hash}",
                    web::get().to(verify_certificate_handler),
                )
                .route("/sign/{token}", web::get().to(ceremony_handler))
                .route("/sign/{token}", web::post().to(sign_handler))
                .route(
                    "/sign/{token}/decline",
                    web::post().to(decline_handler),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        // Auth split: the tenant-scoped `/requests/*` routes are
        // `Authenticated`; the token-gated signing routes and the
        // certificate-hash verification are `Public` — their
        // credential is the HMAC signing token / the certificate
        // hash itself, because external signers have no account and
        // no token to present. (Parent decision: sign/verify GETs
        // are public token-gated; POST sign/decline are token-gated
        // the same way.)
        vec![
            RouteInfo {
                method: "POST",
                path: "/api/v1/esignature/requests",
                owner: MODULE_ID,
                description: "Create a draft signature request with ordered signers.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esignature/requests",
                owner: MODULE_ID,
                description: "List signature requests for one tenant.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esignature/requests/{id}",
                owner: MODULE_ID,
                description: "Get one signature request (expiry computed on read).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esignature/requests/{id}/send",
                owner: MODULE_ID,
                description: "Send a draft request and issue one signing link per signer.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esignature/requests/{id}/remind",
                owner: MODULE_ID,
                description: "Record a reminder for a sent request (max one per hour).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esignature/requests/{id}/audit",
                owner: MODULE_ID,
                description: "Return the hash-chained audit trail for a request.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esignature/requests/{id}/audit/verify",
                owner: MODULE_ID,
                description: "Verify the integrity of a request's audit chain.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esignature/requests/{id}/certificate",
                owner: MODULE_ID,
                description: "Return the certificate of completion for a completed request.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esignature/verify/{certificate_hash}",
                owner: MODULE_ID,
                description: "Verify a completion certificate against its stored hash.",
                auth: unified_core::auth::RouteAuth::Public, // token-gated: the token/hash in the URL is the credential; signers are not logged in.
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/esignature/sign/{token}",
                owner: MODULE_ID,
                description: "Resolve a signing token to the signing ceremony payload.",
                auth: unified_core::auth::RouteAuth::Public, // token-gated: the token/hash in the URL is the credential; signers are not logged in.
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esignature/sign/{token}",
                owner: MODULE_ID,
                description: "Record a signature from a signing token.",
                auth: unified_core::auth::RouteAuth::Public, // token-gated: the token/hash in the URL is the credential; signers are not logged in.
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/esignature/sign/{token}/decline",
                owner: MODULE_ID,
                description: "Decline a signature request from a signing token.",
                auth: unified_core::auth::RouteAuth::Public, // token-gated: the token/hash in the URL is the credential; signers are not logged in.
            },
        ]
    }

    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        Ok(())
    }
}

/// The one function the server calls. Same shape as every built-in
/// module's `register` function.
///
/// Wiring (done by the server, not by this crate): the root
/// `Cargo.toml` gains `external-esignature = { path =
/// "crates/external-esignature" }` and `crates/external-esignature`
/// is added to the root `[workspace] members` (removing the
/// temporary standalone `[workspace]` table in this crate's
/// `Cargo.toml`), and `src/modules/mod.rs::register_all` gains one
/// line in its external-modules section:
/// `external_esignature::register(registry);`
pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(EsignatureModule::default()));
}

/// Convenience accessor for tests and for embedding.
pub fn module() -> Arc<dyn Module> {
    Arc::new(EsignatureModule::default())
}
