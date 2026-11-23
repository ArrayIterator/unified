//! Auth Manager - In-memory state and its constructors.

use crate::auth::{password as auth_password, totp as auth_totp};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hardening::{shared_state, trusted_client_ip_for_http_request};
use unified_core::hooks::events;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::db::models::User;
use unified_core::db::DbPool;
use actix_web::http::header;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::Sha256;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use uuid::Uuid;
use super::{providers::*, jwt::*, handlers_providers::*, login::*, tokens::*, recovery::*, totp::*, google::*, module::*};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// One revoked token: when it was revoked and when the token itself
/// expires. The expiry is stored alongside the revocation time so
/// entries whose token has expired can be pruned instead of growing
/// unboundedly for the process lifetime.
#[derive(Debug, Clone, Copy)]
pub struct RevokedToken {
    pub revoked_at: DateTime<Utc>,
    pub expires_at: i64,
}

/// Placeholder secrets shipped in `config.example.yaml`, imported
/// from the single definition in `unified_core::config` so the
/// config-validation layer and this signing layer can never drift
/// apart. They must never sign a real token: `signing_key`
/// rejects them, and config validation rejects them at startup.
pub(crate) use unified_core::config::{PLACEHOLDER_SALT_KEY, PLACEHOLDER_SECRET_KEY};

/// Minimum lengths for the signing keys. Enforced here (fail-closed
/// at signing/verification time) as well as by config validation at
/// startup, so a key that slips past validation can never silently
/// sign tokens.
pub(crate) const MIN_SECRET_KEY_LEN: usize = 32;
pub(crate) const MIN_SALT_KEY_LEN: usize = 16;

/// One revoked-token row from the `auth_revoked_tokens` table
/// (migration `2026-11-22-000109`). Loaded with a bound-parameter
/// `sql_query` so revocation persistence does not depend on the
/// generated Diesel schema.
#[derive(Debug, QueryableByName)]
pub(crate) struct RevokedTokenRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub(crate) jti: String,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub(crate) revoked_at: DateTime<Utc>,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub(crate) expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct AuthManagerState {
    pub enabled: Arc<DashMap<String, bool>>,
    pub settings: Arc<DashMap<String, Map<String, Value>>>,
    /// Revoked token ids (jti) with revocation time and token expiry.
    pub revoked_tokens: Arc<DashMap<String, RevokedToken>>,
    /// True once the revocation table has been loaded into
    /// `revoked_tokens` for this process (see
    /// `ensure_revoked_tokens_hydrated`).
    pub(crate) revocations_hydrated: Arc<AtomicBool>,
    /// Google OAuth tokens, keyed by Google account id (`sub`).
    /// Tokens are never serialized or returned by any endpoint.
    pub google_tokens: Arc<DashMap<String, GoogleTokenRecord>>,
    /// Pending Google OAuth authorization flows, keyed by `state`.
    pub google_oauth_states: Arc<DashMap<String, GoogleOAuthState>>,
    /// Google Drive storage targets linked through the connector.
    pub google_drive_links: Arc<DashMap<Uuid, GoogleDriveLink>>,
    /// Two-factor recovery codes, keyed by user id. Only Argon2id
    /// PHC hashes are stored; the plaintext codes are shown exactly once,
    /// at generation time, and never again.
    pub recovery_codes: Arc<DashMap<String, Vec<RecoveryCodeRecord>>>,
    /// Pending TOTP enrolments, keyed by user id: a generated secret
    /// that is not active until the user proves possession with a
    /// valid code (`totp.confirm`). Pending secrets expire after
    /// `PENDING_TOTP_TTL_MINUTES` and are never written to the users
    /// table until confirmed.
    pub pending_totp: Arc<DashMap<String, PendingTotp>>,
    /// Last password-reset request per normalized email, for the
    /// per-address cooldown in `reset::request_password_reset`:
    /// at most one reset email per address per cooldown window, so
    /// the endpoint cannot be used to flood one inbox.
    pub(crate) password_reset_requests: Arc<DashMap<String, DateTime<Utc>>>,
}

impl Default for AuthManagerState {
    fn default() -> Self {
        let enabled = DashMap::new();
        for (id, _, _, _, _) in provider_definitions() {
            enabled.insert(id.clone(), default_enabled(&id));
        }
        Self {
            enabled: Arc::new(enabled),
            settings: Arc::new(DashMap::new()),
            revoked_tokens: Arc::new(DashMap::new()),
            google_tokens: Arc::new(DashMap::new()),
            google_oauth_states: Arc::new(DashMap::new()),
            google_drive_links: Arc::new(DashMap::new()),
            recovery_codes: Arc::new(DashMap::new()),
            pending_totp: Arc::new(DashMap::new()),
            revocations_hydrated: Arc::new(AtomicBool::new(false)),
            password_reset_requests: Arc::new(DashMap::new()),
        }
    }
}

impl AuthManagerState {
    /// Signing key for JWTs: the configured application secret and
    /// salt, joined as `"{secret_key}:{salt_key}"`.
    ///
    /// There is deliberately no fallback: when the keys are unset,
    /// too short, or still the shipped placeholders, signing and
    /// verification fail loudly instead of minting tokens under a
    /// per-process random secret that would not survive a restart
    /// (and that the `security` verifier, which refuses empty keys,
    /// would reject). Config validation enforces the same rule at
    /// startup; this is the fail-closed second layer.
    pub(crate) fn signing_key(&self, ctx: &AppContext) -> UnifiedResult<Vec<u8>> {
        let config = ctx.config.read().expect("config lock poisoned");
        let secret_key = config.app.secret_key.trim();
        let salt_key = config.app.salt_key.trim();
        if secret_key.is_empty() || salt_key.is_empty() {
            return Err(UnifiedError::internal(
                "Token signing is not configured: app.secret_key and app.salt_key must both be set",
            ));
        }
        if secret_key.len() < MIN_SECRET_KEY_LEN {
            return Err(UnifiedError::internal(
                "Token signing is not configured: app.secret_key must be at least 32 characters",
            ));
        }
        if salt_key.len() < MIN_SALT_KEY_LEN {
            return Err(UnifiedError::internal(
                "Token signing is not configured: app.salt_key must be at least 16 characters",
            ));
        }
        if secret_key == PLACEHOLDER_SECRET_KEY || salt_key == PLACEHOLDER_SALT_KEY {
            return Err(UnifiedError::internal(
                "Token signing is not configured: app.secret_key / app.salt_key still hold the placeholder value from config.example.yaml; generate real random keys",
            ));
        }
        Ok(format!("{secret_key}:{salt_key}").into_bytes())
    }

    /// True when `jti` has a still-valid row in the
    /// `auth_revoked_tokens` table (migration `2026-11-22-000109`).
    /// This is the durable source of truth the global auth
    /// middleware and the token-verify path consult directly, so a
    /// revocation written by any process (logout, another
    /// instance) is enforced immediately — the in-memory map is
    /// only a cache of this table and can go stale for rows it did
    /// not write itself. Any database error propagates: callers
    /// fail closed on it (a revocation status that cannot be
    /// determined must never be treated as "not revoked").
    pub(crate) fn is_jti_revoked_in_db(pool: &DbPool, jti: &str) -> UnifiedResult<bool> {
        use diesel::sql_types::{Text, Timestamptz};
        let mut connection = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot borrow a database connection: {error}"))
        })?;
        let row = diesel::sql_query(
            "SELECT jti, revoked_at, expires_at FROM auth_revoked_tokens \
             WHERE jti = $1 AND expires_at > $2 LIMIT 1",
        )
        .bind::<Text, _>(jti)
        .bind::<Timestamptz, _>(Utc::now())
        .get_result::<RevokedTokenRow>(&mut connection)
        .optional()
        .map_err(UnifiedError::from)?;
        Ok(row.is_some())
    }

    /// Load the still-valid revocations from the
    /// `auth_revoked_tokens` table (migration `2026-11-22-000109`).
    /// Synchronous: callers on async paths use
    /// `ensure_revoked_tokens_hydrated` (which runs this through
    /// `run_blocking`) instead.
    pub(crate) fn load_revoked_tokens(pool: &DbPool) -> UnifiedResult<Vec<RevokedTokenRow>> {
        use diesel::sql_types::Timestamptz;
        let mut connection = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot borrow a database connection: {error}"))
        })?;
        diesel::sql_query(
            "SELECT jti, revoked_at, expires_at FROM auth_revoked_tokens WHERE expires_at > $1",
        )
        .bind::<Timestamptz, _>(Utc::now())
        .load::<RevokedTokenRow>(&mut connection)
        .map_err(UnifiedError::from)
    }

    /// Persist one revocation to the `auth_revoked_tokens` table.
    /// `ON CONFLICT (jti) DO NOTHING` keeps a re-delivered revoke
    /// idempotent. Best-effort by design: callers ignore the error
    /// because the in-memory map already records the revocation for
    /// this process.
    pub(crate) fn persist_revocation(
        pool: &DbPool,
        jti: &str,
        user_id: &str,
        revoked_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> UnifiedResult<()> {
        use diesel::sql_types::{Text, Timestamptz};
        let mut connection = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot borrow a database connection: {error}"))
        })?;
        diesel::sql_query(
            "INSERT INTO auth_revoked_tokens (jti, user_id, revoked_at, expires_at) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (jti) DO NOTHING",
        )
        .bind::<Text, _>(jti)
        .bind::<Text, _>(user_id)
        .bind::<Timestamptz, _>(revoked_at)
        .bind::<Timestamptz, _>(expires_at)
        .execute(&mut connection)
        .map_err(UnifiedError::from)?;
        Ok(())
    }

    /// Load the persisted revocations into the in-memory map once
    /// per process, so a token revoked before a restart stays
    /// revoked. Best-effort: when the database is unavailable (or
    /// the table does not exist yet) the flag stays unset and the
    /// next call retries, while verification keeps using whatever
    /// the in-memory map already holds.
    pub(crate) async fn ensure_revoked_tokens_hydrated(&self, pool: DbPool) {
        if self.revocations_hydrated.load(Ordering::SeqCst) {
            return;
        }
        let loaded = run_blocking(pool, |connection| {
            use diesel::sql_types::Timestamptz;
            diesel::sql_query(
                "SELECT jti, revoked_at, expires_at FROM auth_revoked_tokens WHERE expires_at > $1",
            )
            .bind::<Timestamptz, _>(Utc::now())
            .load::<RevokedTokenRow>(connection)
            .map_err(UnifiedError::from)
        })
        .await;
        if let Ok(rows) = loaded {
            let now_unix = Utc::now().timestamp();
            for row in rows {
                self.revoked_tokens.insert(
                    row.jti,
                    RevokedToken {
                        revoked_at: row.revoked_at,
                        expires_at: row.expires_at.timestamp(),
                    },
                );
            }
            self.prune_revoked_tokens(now_unix);
            self.revocations_hydrated.store(true, Ordering::SeqCst);
        }
    }

    /// Persist one revocation without blocking the async worker:
    /// the in-memory insert has already happened; the database row
    /// is what makes the revocation survive a restart. Failures are
    /// swallowed (best-effort) so a database hiccup never turns a
    /// successful in-process revoke into an error response.
    pub(crate) async fn persist_revocation_async(
        &self,
        pool: DbPool,
        jti: String,
        user_id: String,
        revoked_at: DateTime<Utc>,
        expires_at: i64,
    ) {
        let expires_at_dt = DateTime::from_timestamp(expires_at, 0).unwrap_or_else(Utc::now);
        let _ = run_blocking(pool, move |connection| {
            use diesel::sql_types::{Text, Timestamptz};
            diesel::sql_query(
                "INSERT INTO auth_revoked_tokens (jti, user_id, revoked_at, expires_at) \
                 VALUES ($1, $2, $3, $4) ON CONFLICT (jti) DO NOTHING",
            )
            .bind::<Text, _>(&jti)
            .bind::<Text, _>(&user_id)
            .bind::<Timestamptz, _>(revoked_at)
            .bind::<Timestamptz, _>(expires_at_dt)
            .execute(connection)
            .map_err(UnifiedError::from)?;
            Ok(())
        })
        .await;
    }

    pub(crate) fn is_enabled(&self, provider_id: &str) -> bool {
        self.enabled
            .get(provider_id)
            .map(|entry| *entry.value())
            .unwrap_or(false)
    }

    /// Drop revoked-token entries whose token has already expired
    /// (`expires_at <= now_unix`). A revoked token cannot be used
    /// after its expiry anyway, so keeping its id only wastes memory.
    /// Called on every token verification and revocation, so the map
    /// stays bounded by the number of live revoked tokens.
    pub(crate) fn prune_revoked_tokens(&self, now_unix: i64) {
        self.revoked_tokens
            .retain(|_, record| record.expires_at > now_unix);
    }

    /// True when `jti` is revoked, after first pruning entries whose
    /// tokens have expired.
    pub(crate) fn is_token_revoked(&self, jti: &str, now_unix: i64) -> bool {
        self.prune_revoked_tokens(now_unix);
        self.revoked_tokens.contains_key(jti)
    }

    pub(crate) fn provider_info(&self, id: &str) -> Option<ProviderInfo> {
        provider_definitions()
            .into_iter()
            .find(|(provider_id, _, _, _, _)| provider_id == id)
            .map(|(id, name, kind, description, schema)| {
                let mut settings = Map::new();
                for field in &schema {
                    if let Some(default) = &field.default {
                        settings.insert(field.key.clone(), default.clone());
                    }
                }
                if let Some(stored) = self.settings.get(&id) {
                    for (key, value) in stored.value().iter() {
                        settings.insert(key.clone(), value.clone());
                    }
                }
                ProviderInfo {
                    enabled: self.is_enabled(&id),
                    settings_schema: schema,
                    settings,
                    id,
                    name,
                    kind,
                    description,
                }
            })
    }
}

impl AuthManagerState {
    /// Google connector configuration from the `google` provider
    /// settings: (client_id, client_secret, redirect_uri, scopes,
    /// hosted_domain).
    pub(crate) fn google_config(&self) -> (String, String, String, Vec<String>, Option<String>) {
        let info = self.provider_info("google");
        let get = |key: &str| {
            info.as_ref()
                .and_then(|info| info.settings.get(key))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let scopes = parse_google_scopes(&get("scopes"))
            .unwrap_or_else(|_| vec!["openid".into(), "email".into(), "profile".into()]);
        let hosted_domain = get("hosted_domain");
        (
            get("client_id"),
            get("client_secret"),
            get("redirect_uri"),
            scopes,
            if hosted_domain.is_empty() {
                None
            } else {
                Some(hosted_domain)
            },
        )
    }

    pub(crate) fn google_configured(&self) -> bool {
        let (client_id, client_secret, _, _, _) = self.google_config();
        !client_id.is_empty() && !client_secret.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revoked(expires_at: i64) -> RevokedToken {
        RevokedToken {
            revoked_at: Utc::now(),
            expires_at,
        }
    }

    #[test]
    fn prune_revoked_tokens_drops_only_expired_entries() {
        let state = AuthManagerState::default();
        state
            .revoked_tokens
            .insert("expired".to_string(), revoked(1_000));
        state
            .revoked_tokens
            .insert("live".to_string(), revoked(9_999_999_999));
        state.prune_revoked_tokens(2_000);
        assert!(!state.revoked_tokens.contains_key("expired"));
        assert!(state.revoked_tokens.contains_key("live"));
        assert_eq!(state.revoked_tokens.len(), 1);
    }

    #[test]
    fn is_token_revoked_prunes_before_checking() {
        let state = AuthManagerState::default();
        state
            .revoked_tokens
            .insert("expired".to_string(), revoked(1_000));
        state
            .revoked_tokens
            .insert("live".to_string(), revoked(9_999_999_999));
        assert!(!state.is_token_revoked("expired", 2_000));
        assert!(state.is_token_revoked("live", 2_000));
        // The expired entry is gone, so the map no longer grows.
        assert!(!state.revoked_tokens.contains_key("expired"));
    }
}
