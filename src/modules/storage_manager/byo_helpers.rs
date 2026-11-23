// BYO helpers.

use unified_core::config::AppConfig;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::storage::{
    provider_from_config,
    validate_key,
    StorageProvider,
    StorageTargetConfig,
};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// BYO helpers: loaders (404 on cross-owner access), provider build,
// validation probe, health checks, prefix scoping, rule checks.
// ---------------------------------------------------------------------------

pub(crate) fn load_byo_connection(
    state: &StorageManagerState,
    raw_id: &str,
) -> UnifiedResult<ByoConnection> {
    let id = parse_uuid(raw_id, "BYO connection")?;
    state
        .byo_connections
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("BYO connection not found: {id}")))
}

/// Load a BYO connection as one caller may see it. A non-admin
/// caller who does not own the connection gets the same 404 as for
/// a missing connection, so existence never leaks across owners
/// (the per-user 404 pattern the todos module uses).
pub(crate) fn load_byo_connection_for(
    state: &StorageManagerState,
    raw_id: &str,
    actor: &str,
    is_admin: bool,
) -> UnifiedResult<ByoConnection> {
    let connection = load_byo_connection(state, raw_id)?;
    if is_admin || connection.owner_user == actor {
        Ok(connection)
    } else {
        Err(UnifiedError::not_found(format!(
            "BYO connection not found: {raw_id}"
        )))
    }
}

pub(crate) fn load_byo_terms(
    state: &StorageManagerState,
    raw_id: &str,
) -> UnifiedResult<ByoTerms> {
    let id = parse_uuid(raw_id, "BYO terms")?;
    state
        .byo_terms
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("BYO terms not found: {id}")))
}

pub(crate) fn current_byo_terms(state: &StorageManagerState) -> Option<ByoTerms> {
    state
        .byo_terms
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|terms| terms.is_current && terms.enabled)
        .max_by(|a, b| a.created_at.cmp(&b.created_at))
}

pub(crate) fn has_accepted_terms(
    state: &StorageManagerState,
    user_id: &str,
    terms_version: &str,
) -> bool {
    state.byo_acceptances.iter().any(|entry| {
        let acceptance = entry.value();
        acceptance.user_id == user_id && acceptance.terms_version == terms_version
    })
}

pub(crate) fn validate_extensions(values: &Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for value in values.clone().unwrap_or_default() {
        let cleaned = value
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase();
        if cleaned.is_empty() || cleaned.len() > 32 {
            return Err(UnifiedError::bad_request(
                "Allowed extensions must be 1..=32 characters",
            ));
        }
        if !cleaned
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(UnifiedError::bad_request(
                "Allowed extensions must contain only letters, digits, '-' or '_'",
            ));
        }
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    Ok(out)
}

pub(crate) fn validate_mime_types(values: &Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for value in values.clone().unwrap_or_default() {
        let cleaned = value.trim().to_ascii_lowercase();
        if cleaned.is_empty() || cleaned.len() > 128 {
            return Err(UnifiedError::bad_request(
                "Allowed MIME types must be 1..=128 characters",
            ));
        }
        if !cleaned.contains('/') {
            return Err(UnifiedError::bad_request(
                "Allowed MIME types must look like \"type/subtype\"",
            ));
        }
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    Ok(out)
}

/// Build the provider target config for a BYO connection,
/// decrypting its stored credentials. Same shape as
/// `target_config_for_bucket`; the config lives in memory only.
pub(crate) fn target_config_for_byo(
    config: &RwLock<AppConfig>,
    connection: &ByoConnection,
) -> UnifiedResult<StorageTargetConfig> {
    let mut target = StorageTargetConfig {
        name: format!("storage-manager:byo:{}", connection.id),
        kind: connection.provider_kind,
        endpoint: connection.endpoint.clone(),
        region: connection.region.clone(),
        bucket: connection.container.clone(),
        base_dir: connection.path.clone(),
        sftp_host: connection.sftp_host.clone(),
        sftp_port: connection.sftp_port,
        sftp_username: connection.sftp_username.clone(),
        sftp_auth_kind: connection.sftp_auth_kind.clone(),
        sftp_keyfile: connection.sftp_keyfile.clone(),
        sftp_base_path: connection.path.clone(),
        ..StorageTargetConfig::default()
    };
    if let Some(encrypted) = &connection.credentials_encrypted {
        let secret = encryption_secret_from(config);
        let plaintext = decrypt_secret_value(&secret, encrypted)?;
        let value: Value = serde_json::from_str(&plaintext).map_err(|_| {
            UnifiedError::internal("Stored BYO credentials cannot be decoded")
        })?;
        let get = |field: &str| {
            value
                .get(field)
                .and_then(|item| item.as_str())
                .unwrap_or("")
                .to_string()
        };
        target.access_key = get("access_key");
        target.secret_key = get("secret_key");
        target.client_id = get("client_id");
        target.client_secret = get("client_secret");
        target.refresh_token = get("refresh_token");
        target.access_token = get("access_token");
        target.token_url = get("token_url");
        target.sftp_password = get("sftp_password");
    }
    Ok(target)
}

/// Build a live provider for a BYO connection. Refused unless the
/// connection is active: a draft, suspended, or revoked connection
/// can never move bytes, even with valid credentials.
pub(crate) fn provider_for_byo(
    config: &RwLock<AppConfig>,
    connection: &ByoConnection,
) -> UnifiedResult<Arc<dyn StorageProvider>> {
    if connection.status != "active" {
        return Err(UnifiedError::conflict(format!(
            "BYO connection \"{}\" is {} and cannot be used",
            connection.name, connection.status
        )));
    }
    let target = target_config_for_byo(config, connection)?;
    provider_from_config(&target, &connection.allowed_hosts)
}

/// Build a provider for validation / health probes, which must run
/// before activation. Status is deliberately not checked here: the
/// caller decides which states may be probed.
pub(crate) fn provider_for_byo_probe(
    config: &RwLock<AppConfig>,
    connection: &ByoConnection,
) -> UnifiedResult<Arc<dyn StorageProvider>> {
    let target = target_config_for_byo(config, connection)?;
    provider_from_config(&target, &connection.allowed_hosts)
}

/// Validate a BYO connection: connect, list the test prefix,
/// write + read + delete a probe object under the connection
/// prefix, then remove it. A failed step blocks activation and the
/// reason is recorded on the connection (never any credential or
/// provider error body beyond the short message). Reuses the same
/// probe shape as the bucket health check.
pub(crate) async fn run_byo_validation(
    state: &StorageManagerState,
    config: &RwLock<AppConfig>,
    connection: &ByoConnection,
) -> UnifiedResult<()> {
    let outcome: UnifiedResult<()> = (|| async {
        let provider = provider_for_byo_probe(config, connection)?;
        provider.test_connection().await?;
        let _ = provider.list(&connection.path_prefix).await;
        let probe_key = format!(
            "{}/.byo-probe-{}/probe.txt",
            connection.path_prefix,
            uuid::Uuid::now_v7()
        );
        let probe_body = b"storage-manager-byo-probe".to_vec();
        provider.upload(&probe_key, probe_body.clone()).await?;
        let read_back = provider.download(&probe_key).await;
        let delete_result = provider.delete(&probe_key).await;
        let bytes = read_back?;
        if bytes != probe_body {
            let _ = delete_result;
            return Err(UnifiedError::internal(
                "BYO validation probe read back different bytes than were written",
            ));
        }
        delete_result?;
        Ok(())
    })()
    .await;
    let now = Utc::now();
    match &outcome {
        Ok(()) => {
            if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
                stored.last_validated_at = Some(now);
                stored.last_validation_ok = Some(true);
                stored.last_validation_error = None;
                if stored.status == "draft" {
                    stored.status = "validated".to_string();
                }
                stored.updated_at = now;
            }
            state.emit(
                events::BYO_VALIDATED,
                json!({
                    "connection_id": connection.id.to_string(),
                    "name": connection.name,
                    "owner_user": connection.owner_user,
                    "company_id": connection.company_id,
                    "provider_kind": connection.provider_kind.as_str(),
                    "validated_at": now.to_rfc3339(),
                }),
            );
            Ok(())
        }
        Err(error) => {
            let detail = error.message.clone();
            if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
                stored.last_validated_at = Some(now);
                stored.last_validation_ok = Some(false);
                stored.last_validation_error = Some(detail.clone());
                stored.updated_at = now;
            }
            state.emit(
                events::BYO_VALIDATION_FAILED,
                json!({
                    "connection_id": connection.id.to_string(),
                    "name": connection.name,
                    "owner_user": connection.owner_user,
                    "error": detail,
                }),
            );
            Err(error.clone())
        }
    }
}

/// Run one BYO health check: connectivity plus a write / read /
/// delete probe on a scratch key under the connection prefix, then
/// cleanup. Mirrors `run_health_check` for buckets.
pub(crate) async fn run_byo_health_check(
    state: &StorageManagerState,
    config: &RwLock<AppConfig>,
    connection: &ByoConnection,
) -> UnifiedResult<ByoHealthCheck> {
    let started = std::time::Instant::now();
    let outcome: UnifiedResult<()> = (|| async {
        let provider = provider_for_byo_probe(config, connection)?;
        provider.test_connection().await?;
        let probe_key = format!(
            "{}/.byo-healthcheck-{}/probe.txt",
            connection.path_prefix,
            uuid::Uuid::now_v7()
        );
        let probe_body = b"storage-manager-byo-health-probe".to_vec();
        provider.upload(&probe_key, probe_body.clone()).await?;
        let read_back = provider.download(&probe_key).await;
        let delete_result = provider.delete(&probe_key).await;
        let bytes = read_back?;
        if bytes != probe_body {
            let _ = delete_result;
            return Err(UnifiedError::internal(
                "BYO health probe read back different bytes than were written",
            ));
        }
        delete_result?;
        Ok(())
    })()
    .await;
    let latency_ms = started.elapsed().as_millis() as i64;
    let now = Utc::now();
    let (ok, detail) = match &outcome {
        Ok(()) => (true, "Connectivity and write/read/delete probe passed.".to_string()),
        Err(error) => (false, error.message.clone()),
    };
    let check = ByoHealthCheck {
        id: Uuid::now_v7(),
        connection_id: connection.id,
        ok,
        latency_ms,
        detail,
        checked_at: now,
    };
    state.byo_health_checks.insert(check.id, check.clone());
    if let Some(mut stored) = state.byo_connections.get_mut(&connection.id) {
        stored.last_health_at = Some(now);
        stored.last_health_ok = Some(ok);
        stored.updated_at = now;
    }
    if ok {
        state.emit(
            events::BYO_HEALTH_OK,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "latency_ms": latency_ms,
                "checked_at": now.to_rfc3339(),
            }),
        );
    } else {
        state.emit(
            events::BYO_HEALTH_FAILED,
            json!({
                "connection_id": connection.id.to_string(),
                "name": connection.name,
                "error": check.detail.clone(),
                "latency_ms": latency_ms,
                "checked_at": now.to_rfc3339(),
            }),
        );
    }
    match outcome {
        Ok(()) => Ok(check),
        Err(error) => Err(error),
    }
}

/// Join a caller-supplied relative key onto the connection prefix.
/// The result is always under the prefix: `..`, absolute paths,
/// backslashes, and empty segments are rejected by `validate_key`.
pub(crate) fn byo_full_key(connection: &ByoConnection, relative_key: &str) -> UnifiedResult<String> {
    validate_key(relative_key)?;
    Ok(format!("{}/{}", connection.path_prefix, relative_key))
}

/// Check one object operation against the connection rules:
/// status, allowed operation, prefix scoping, file-type allowlists,
/// and quotas. The 409 quota refusal mirrors the bucket quota path.
pub(crate) fn check_byo_operation(
    state: &StorageManagerState,
    connection: &ByoConnection,
    operation: &str,
    relative_key: &str,
    mime: Option<&str>,
    delta_bytes: i64,
    delta_files: i64,
) -> UnifiedResult<String> {
    if connection.status != "active" {
        return Err(UnifiedError::conflict(format!(
            "BYO connection \"{}\" is {} and cannot be used",
            connection.name, connection.status
        )));
    }
    let allowed = match operation {
        "read" => connection.allow_read,
        "write" => connection.allow_write,
        "delete" => connection.allow_delete,
        _ => false,
    };
    if !allowed {
        return Err(UnifiedError::forbidden(format!(
            "BYO connection \"{}\" does not allow {operation} operations",
            connection.name
        )));
    }
    let full_key = byo_full_key(connection, relative_key)?;
    if !connection.allowed_extensions.is_empty() {
        let extension = relative_key
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let has_extension = relative_key.contains('.') && !extension.is_empty();
        if !has_extension || !connection.allowed_extensions.contains(&extension) {
            return Err(UnifiedError::bad_request(format!(
                "File extension is not allowed for BYO connection \"{}\"",
                connection.name
            )));
        }
    }
    if !connection.allowed_mime_types.is_empty() {
        let mime = mime.unwrap_or("application/octet-stream");
        if !connection
            .allowed_mime_types
            .iter()
            .any(|allowed| allowed == mime)
        {
            return Err(UnifiedError::bad_request(format!(
                "MIME type \"{mime}\" is not allowed for BYO connection \"{}\"",
                connection.name
            )));
        }
    }
    if operation == "write" {
        if let Some(limit) = connection.max_bytes {
            if connection.used_bytes + delta_bytes > limit {
                state.emit(
                    events::BYO_QUOTA_EXCEEDED,
                    json!({
                        "connection_id": connection.id.to_string(),
                        "owner_user": connection.owner_user,
                        "used_bytes": connection.used_bytes,
                        "bytes": delta_bytes,
                        "max_bytes": limit,
                    }),
                );
                return Err(UnifiedError::conflict(format!(
                    "BYO connection quota exceeded: used {} + {delta_bytes} exceeds the limit of {limit} bytes",
                    connection.used_bytes
                )));
            }
        }
        if let Some(limit) = connection.max_files {
            if connection.used_files + delta_files > limit {
                state.emit(
                    events::BYO_QUOTA_EXCEEDED,
                    json!({
                        "connection_id": connection.id.to_string(),
                        "owner_user": connection.owner_user,
                        "used_files": connection.used_files,
                        "max_files": limit,
                    }),
                );
                return Err(UnifiedError::conflict(format!(
                    "BYO connection quota exceeded: used {} + {delta_files} exceeds the limit of {limit} files",
                    connection.used_files
                )));
            }
        }
    }
    Ok(full_key)
}

pub(crate) fn byo_object_key(connection_id: Uuid, key: &str) -> String {
    format!("{connection_id}:{key}")
}

/// True when a BYO connection's last validation is still fresh
/// enough to activate it: it passed (`last_validation_ok`) and it
/// ran within `BYO_MAX_VALIDATION_AGE_SECS` (24h). A validation
/// from arbitrarily long ago does not activate a connection whose
/// credentials or endpoint may have changed since. Re-validation
/// is via POST `/byo/connections/{id}/validate`; activation does
/// not re-validate inline because the probe is async and the
/// activation path is synchronous.
pub(crate) fn validation_is_fresh(
    connection: &ByoConnection,
    now: chrono::DateTime<Utc>,
) -> bool {
    if connection.last_validation_ok != Some(true) {
        return false;
    }
    match connection.last_validated_at {
        Some(validated_at) => {
            now.signed_duration_since(validated_at).num_seconds()
                <= BYO_MAX_VALIDATION_AGE_SECS
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_extensions_normalizes_and_dedupes() {
        assert_eq!(validate_extensions(&None).unwrap(), Vec::<String>::new());
        assert_eq!(
            validate_extensions(&Some(vec![".JPG".to_string(), "png".to_string(), "jpg".to_string()])).unwrap(),
            vec!["jpg".to_string(), "png".to_string()]
        );
        assert!(validate_extensions(&Some(vec!["".to_string()])).is_err());
        assert!(validate_extensions(&Some(vec!["a".repeat(33)])).is_err());
        assert!(validate_extensions(&Some(vec!["a/b".to_string()])).is_err());
    }

    #[test]
    fn validate_mime_types_requires_type_subtype_shape() {
        assert_eq!(validate_mime_types(&None).unwrap(), Vec::<String>::new());
        assert_eq!(
            validate_mime_types(&Some(vec!["Image/PNG".to_string(), "image/png".to_string()])).unwrap(),
            vec!["image/png".to_string()]
        );
        assert!(validate_mime_types(&Some(vec!["png".to_string()])).is_err());
        assert!(validate_mime_types(&Some(vec!["".to_string()])).is_err());
    }

    #[test]
    fn byo_object_key_joins_connection_and_key() {
        let id = uuid::Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000").unwrap();
        assert_eq!(
            byo_object_key(id, "prefix/a.jpg"),
            "123e4567-e89b-12d3-a456-426614174000:prefix/a.jpg"
        );
    }

    fn fresh_connection(validated_at: Option<chrono::DateTime<Utc>>, ok: Option<bool>) -> ByoConnection {
        let now = Utc::now();
        ByoConnection {
            id: Uuid::now_v7(),
            name: "byo".to_string(),
            provider_kind: unified_core::storage::StorageKind::S3,
            s3_compatible: false,
            endpoint: String::new(),
            region: String::new(),
            container: String::new(),
            path: String::new(),
            sftp_host: String::new(),
            sftp_port: 0,
            sftp_username: String::new(),
            sftp_auth_kind: String::new(),
            sftp_keyfile: String::new(),
            allowed_hosts: Vec::new(),
            credentials_encrypted: None,
            key_fingerprint: None,
            path_prefix: String::new(),
            owner_user: "u-1".to_string(),
            company_id: None,
            status: "validated".to_string(),
            status_reason: None,
            allow_read: true,
            allow_write: true,
            allow_delete: true,
            max_bytes: None,
            max_files: None,
            used_bytes: 0,
            used_files: 0,
            encryption_required: false,
            allowed_extensions: Vec::new(),
            allowed_mime_types: Vec::new(),
            terms_version: None,
            terms_accepted_at: None,
            last_validated_at: validated_at,
            last_validation_ok: ok,
            last_validation_error: None,
            last_health_at: None,
            last_health_ok: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn validation_freshness_requires_a_recent_pass() {
        let now = Utc::now();
        // A pass 1 hour ago is fresh.
        assert!(validation_is_fresh(
            &fresh_connection(Some(now - chrono::Duration::hours(1)), Some(true)),
            now
        ));
        // A pass 25 hours ago is stale, even though it passed.
        assert!(!validation_is_fresh(
            &fresh_connection(Some(now - chrono::Duration::hours(25)), Some(true)),
            now
        ));
        // Exactly at the 24h boundary is still fresh.
        assert!(validation_is_fresh(
            &fresh_connection(Some(now - chrono::Duration::seconds(BYO_MAX_VALIDATION_AGE_SECS)), Some(true)),
            now
        ));
        // A failed validation is never fresh, and neither is a pass
        // with no timestamp.
        assert!(!validation_is_fresh(
            &fresh_connection(Some(now), Some(false)),
            now
        ));
        assert!(!validation_is_fresh(&fresh_connection(None, Some(true)), now));
        assert!(!validation_is_fresh(&fresh_connection(None, None), now));
    }
}
