//! Documents state (split from state.rs; moved verbatim).

use super::*;

impl DocumentsState {

    /// Allowlist rule for `module_id`, if one exists.
    pub(crate) fn allowlist_rule(&self, module_id: &str) -> Option<AllowlistRule> {
        self.allowlist
            .iter()
            .find(|entry| entry.value().module_id == module_id)
            .map(|entry| entry.value().clone())
    }


    /// Validate one file against the per-module allowlist: extension
    /// and MIME must be allowed and the size must fit the rule.
    /// A module with no rule is rejected (fail closed).
    pub(crate) fn check_allowlist(
        &self,
        module_id: &str,
        name: &str,
        mime: &str,
        size: u64,
    ) -> UnifiedResult<()> {
        let rule = self.allowlist_rule(module_id).ok_or_else(|| {
            UnifiedError::unprocessable(format!(
                "No allowlist rule exists for module \"{module_id}\"; an admin must create one before files can be uploaded for that module"
            ))
        })?;
        let extension = Path::new(name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            .to_lowercase();
        if extension.is_empty() || !rule.extensions.iter().any(|allowed| allowed == &extension) {
            return Err(UnifiedError::unprocessable(format!(
                "File extension \".{extension}\" is not allowed for module \"{module_id}\""
            )));
        }
        if !rule.mime_types.is_empty()
            && !rule.mime_types.iter().any(|allowed| allowed == mime)
        {
            return Err(UnifiedError::unprocessable(format!(
                "MIME type \"{mime}\" is not allowed for module \"{module_id}\""
            )));
        }
        if size > rule.max_bytes {
            return Err(UnifiedError::new(
                format!(
                    "File size {size} bytes exceeds the allowlist limit of {} bytes for module \"{module_id}\"",
                    rule.max_bytes
                ),
                413,
            ));
        }
        Ok(())
    }


    /// True when `caller` may read `document`: the owner, or a
    /// caller holding `documents:admin`, or a company/module scope
    /// match. Company and module scope matches need the caller's
    /// company, which the request headers do not carry, so a
    /// non-owner read of a company/module-scoped document requires
    /// the admin capability; the scope is still recorded so a future
    /// company-aware caller can enforce it precisely.
    pub(crate) fn can_read(
        &self,
        ctx: &AppContext,
        caller: &Caller,
        document: &DocumentRecord,
    ) -> bool {
        if document.owner_id == caller.actor {
            return true;
        }
        if check_capability(ctx, &caller.role, PERM_ADMIN) {
            return true;
        }
        false
    }


    /// Validate a signed download token. Returns the document id
    /// and the scope it was issued for on success; rejects expired
    /// or tampered tokens. The token form is
    /// `"<document-id>.<scope>.<expiry-unix>.<hmac-hex>"`, where
    /// `<scope>` is the tenant UUID or the literal `system`. The
    /// scope travels inside the HMAC-signed token because the
    /// redemption route has no tenant context of its own: the
    /// stored key is scope-free, so the provider root must be
    /// re-resolved from a scope the caller cannot forge. The
    /// version is resolved from the document at redemption time, so
    /// the token always downloads the current version.
    pub(crate) fn verify_signed_token(&self, token: &str) -> UnifiedResult<(String, StorageScope)> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 4 {
            return Err(UnifiedError::forbidden("Invalid download token"));
        }
        let document_id = parts[0].to_string();
        let scope = StorageScope::from_token(parts[1])
            .ok_or_else(|| UnifiedError::forbidden("Invalid download token"))?;
        let expiry: i64 = parts[2]
            .parse()
            .map_err(|_| UnifiedError::forbidden("Invalid download token"))?;
        if expiry <= Utc::now().timestamp() {
            return Err(UnifiedError::forbidden("Download token has expired"));
        }
        let expected = self.sign_token(&document_id, &scope.token(), expiry);
        if !constant_time_eq(expected.as_bytes(), parts[3].as_bytes()) {
            return Err(UnifiedError::forbidden("Download token is invalid"));
        }
        Ok((document_id, scope))
    }


    /// HMAC-SHA-256 signature hex for a document id + scope +
    /// expiry. The scope is part of the signed payload, so a token
    /// issued for one tenant cannot be replayed against another
    /// scope's tree.
    pub(crate) fn sign_token(&self, document_id: &str, scope: &str, expiry: i64) -> String {
        let key = *self.signing_key.read().expect("signing key lock poisoned");
        let payload = format!("{document_id}:{scope}:{expiry}");
        let mut mac = Hmac::<Sha256>::new_from_slice(&key)
            .expect("HMAC key is always a valid length");
        mac.update(payload.as_bytes());
        let tag = mac.finalize().into_bytes();
        {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut out = String::with_capacity(tag.len() * 2);
            for &byte in tag.iter() {
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
            out
        }
    }


    /// Issue a signed download token for `document_id` in `scope`.
    /// The scope is embedded in the token (see
    /// `verify_signed_token`): the redemption route has no tenant
    /// context, so the token itself must carry the scope the
    /// provider root is resolved from.
    pub(crate) fn issue_signed_token(
        &self,
        document_id: &str,
        scope: StorageScope,
        expires_secs: u64,
    ) -> (String, DateTime<Utc>) {
        let expires_at = Utc::now() + chrono::Duration::seconds(expires_secs as i64);
        let expiry = expires_at.timestamp();
        let scope_token = scope.token();
        let signature = self.sign_token(document_id, &scope_token, expiry);
        (
            format!("{document_id}.{scope_token}.{expiry}.{signature}"),
            expires_at,
        )
    }


    /// Enforce the download scan gate. Returns `Ok(())` when the
    /// download may proceed; otherwise a 403 that names the reason.
    /// `Skipped` is allowed only when the admin setting explicitly
    /// permits unscanned downloads (default: not permitted).
    pub(crate) fn download_gate(&self, document: &DocumentRecord) -> UnifiedResult<()> {
        match ScanStatus::from_str(&document.scan_status) {
            ScanStatus::Clean => Ok(()),
            ScanStatus::Infected => Err(UnifiedError::forbidden(
                "Download blocked: the file failed the virus scan",
            )),
            ScanStatus::Pending => Err(UnifiedError::forbidden(
                "Download blocked: the file has not finished scanning yet",
            )),
            ScanStatus::Error => Err(UnifiedError::forbidden(
                "Download blocked: the virus scan failed; rescan the file first",
            )),
            ScanStatus::Skipped => {
                if self.settings().allow_unscanned_downloads {
                    Ok(())
                } else {
                    Err(UnifiedError::forbidden(
                        "Download blocked: the file has not been virus-scanned and unscanned downloads are not permitted",
                    ))
                }
            }
        }
    }


    /// Record one download (audit trail + event).
    pub(crate) fn log_download(&self, document: &DocumentRecord, user_id: &str, via: &str) {
        let record = DownloadRecord {
            id: new_id(),
            document_id: document.id.clone(),
            version: document.current_version,
            user_id: user_id.to_string(),
            via: via.to_string(),
            scan_status: document.scan_status.clone(),
            bytes: document.size,
            downloaded_at: now(),
        };
        self.downloads.insert(record.id.clone(), record);
        self.emit(
            events::DOCUMENT_DOWNLOADED,
            json!({
                "document_id": document.id,
                "version": document.current_version,
                "user_id": user_id,
                "via": via,
                "scan_status": document.scan_status,
                "bytes": document.size,
            }),
        );
    }


    /// Next version number for a document (current + 1, or 1 for a
    /// brand-new document). Used as the optimistic version for the
    /// pre-lock scan; the authoritative number is computed again
    /// under the per-document lock in [`insert_version`].
    pub(crate) fn next_version_number(&self, existing_document_id: &Option<String>) -> u32 {
        match existing_document_id {
            Some(id) => self
                .documents
                .get(id)
                .map(|entry| entry.current_version + 1)
                .unwrap_or(1),
            None => 1,
        }
    }
}
