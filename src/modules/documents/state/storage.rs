//! Documents state.

use super::*;

impl DocumentsState {

    /// Store `bytes` under a fresh UUIDv7 file name. Returns the
    /// storage key. There is NO cross-record deduplication: every
    /// upload stores its own copy (storage is the tenant's own
    /// quota). Each upload mints a fresh UUIDv7, so the same bytes
    /// uploaded twice are two distinct objects - the "upload
    /// freely" semantics - and the name is known at upload start,
    /// before any byte is hashed. The SHA-256 is still computed
    /// here (and by the caller for streamed uploads) and stored in
    /// the DB metadata as the integrity value; it is simply not
    /// part of the key. The returned key is SCOPE-FREE
    /// (`<kind>/YYYY/MM/DD/<uuidv7>[.<ext>]`): `scope` selects the
    /// provider root and `area` selects the public/private area
    /// (`StorageScope::provider_key_for_area`) at runtime, so the
    /// provider only ever sees the rooted (effective) key while
    /// the DB only ever stores the scope-free key. `kind` selects
    /// the content kind; `extension` is the validated original
    /// extension (`[a-z0-9]{1,10}`, lowercased) or `None`.
    pub(crate) async fn store_content(
        &self,
        scope: StorageScope,
        area: StorageArea,
        kind: ContentKind,
        extension: Option<&str>,
        bytes: &[u8],
    ) -> UnifiedResult<String> {
        let checksum = sha256_hex(bytes);
        let key = build_object_key(scope.tenant_id(), kind, Utc::now(), &checksum, extension)?;
        let provider = self.provider()?;
        provider
            .upload(&scope.provider_key_for_area(area, &key), bytes.to_vec())
            .await?;
        Ok(key)
    }


    /// Path of the object for a PROVIDER (effective) key under the
    /// local storage root, guaranteeing the result stays inside it.
    /// Callers pass `scope.provider_key(&stored_key)` - never the
    /// bare stored key, which carries no scope root.
    fn object_path(&self, key: &str) -> UnifiedResult<PathBuf> {
        validate_key(key)?;
        let mut path = self.storage_dir.clone();
        for segment in key.split('/') {
            path.push(segment);
        }
        if !path.starts_with(&self.storage_dir) {
            return Err(UnifiedError::forbidden(
                "Storage key escapes the local storage directory",
            ));
        }
        Ok(path)
    }

    /// Local filesystem path of the object for a PROVIDER
    /// (effective) key, for the `/files/*` handlers to stream
    /// directly from disk. Only the local provider supports it:
    /// any other provider kind is an error, and the handler falls
    /// back to the provider's ranged read.
    pub(crate) fn local_object_path(&self, provider_key: &str) -> UnifiedResult<PathBuf> {
        let provider = self.provider()?;
        if !matches!(provider.kind(), StorageKind::Local) {
            return Err(UnifiedError::bad_request(
                "Direct file streaming is only available for local storage",
            ));
        }
        self.object_path(provider_key)
    }

    /// Record a verified checksum for an already-stored object by
    /// writing its `.sha256` sidecar (the durable record the
    /// local provider prefers when it verifies a download). Used
    /// by the lazy-finalize worker after it hashes the stored
    /// object: the object was stored without a sidecar (its hash
    /// was not known yet), and the sidecar closes that gap. For
    /// non-local providers this is a no-op (they have no sidecar
    /// concept; the checksum lives in the DB record).
    pub(crate) fn record_object_checksum(
        &self,
        provider_key: &str,
        checksum: &str,
    ) -> UnifiedResult<()> {
        let provider = self.provider()?;
        if !matches!(provider.kind(), StorageKind::Local) {
            return Ok(());
        }
        let path = self.object_path(provider_key)?;
        self.write_sidecar(&path, checksum)
    }


    /// Stream a file into the object store under a fresh UUIDv7
    /// file name. Returns the scope-free storage key (see
    /// `store_content`): the provider only ever sees the rooted
    /// (effective) key.
    /// Local provider: the file is copied in bounded chunks
    /// straight into the object path (temporary file + rename -
    /// NO `.sha256` sidecar yet, because the whole-file hash is
    /// computed later by the lazy-finalize worker, which writes
    /// the sidecar through `record_object_checksum` once the hash
    /// is verified) - the whole file is never loaded into RAM.
    /// Any other provider: the file is read into memory and stored
    /// through the provider (bounded by the upload size cap, same
    /// as the single-shot path). No deduplication: every call
    /// stores its own copy (see `store_content`).
    /// `expected_sha256` is the client-supplied whole-file hash:
    /// it is only format-validated here (the key builder requires
    /// a 64-hex string; the key itself is a UUIDv7 and never
    /// embeds it) - the real verification happens in the finalize
    /// worker, not here.
    pub(crate) async fn store_content_from_file(
        &self,
        scope: StorageScope,
        area: StorageArea,
        kind: ContentKind,
        extension: Option<&str>,
        expected_sha256: &str,
        size: u64,
        source: &Path,
    ) -> UnifiedResult<String> {
        let key = build_object_key(scope.tenant_id(), kind, Utc::now(), expected_sha256, extension)?;
        let provider_key = scope.provider_key_for_area(area, &key);
        let provider = self.provider()?;
        if matches!(provider.kind(), StorageKind::Local) {
            self.write_object_file(&provider_key, None, source)?;
        } else {
            // The provider API takes whole bytes: refuse instead of
            // loading a file larger than the in-RAM cap. Local
            // storage streams, so it is unaffected.
            let cap = self.settings().max_chunk_ram_bytes;
            if size > cap {
                return Err(UnifiedError::new(
                    format!(
                        "File is {size} bytes, exceeding the {cap} byte in-RAM cap for non-local storage; use local storage or raise documents.max_chunk_ram_bytes"
                    ),
                    413,
                ));
            }
            let bytes = std::fs::read(source).map_err(|error| {
                UnifiedError::internal(format!("Cannot read assembled file: {error}"))
            })?;
            provider.upload(&provider_key, bytes).await?;
        }
        Ok(key)
    }


    /// Copy `source` into the object path for `key`, chunk by
    /// chunk, then - when `checksum` is `Some` - write the
    /// `.sha256` sidecar. The result is the same on-disk layout
    /// `LocalProvider::upload` produces, without ever holding the
    /// file in RAM. A failed sidecar write removes the object
    /// again rather than leaving it behind without an integrity
    /// record. `checksum` is `None` for lazy-finalize stores (the
    /// hash is not known yet; the worker writes the sidecar later
    /// through `record_object_checksum`).
    fn write_object_file(
        &self,
        key: &str,
        checksum: Option<&str>,
        source: &Path,
    ) -> UnifiedResult<()> {
        let path = self.object_path(key)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                UnifiedError::internal(format!(
                    "Cannot create storage directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "object".to_string());
        let temp_name = format!(".{file_name}.tmp-{}", Uuid::new_v4());
        let temp_path = path
            .parent()
            .map(|parent| parent.join(&temp_name))
            .unwrap_or_else(|| PathBuf::from(temp_name));
        let copy_result = (|| -> UnifiedResult<()> {
            let mut input = std::fs::File::open(source).map_err(|error| {
                UnifiedError::internal(format!("Cannot open assembled file: {error}"))
            })?;
            let mut output = std::fs::File::create(&temp_path).map_err(|error| {
                UnifiedError::internal(format!("Cannot create temporary file: {error}"))
            })?;
            std::io::copy(&mut input, &mut output).map_err(|error| {
                UnifiedError::internal(format!("Cannot copy assembled file: {error}"))
            })?;
            output.sync_all().map_err(|error| {
                UnifiedError::internal(format!("Cannot flush temporary file: {error}"))
            })?;
            drop(output);
            std::fs::rename(&temp_path, &path).map_err(|error| {
                UnifiedError::internal(format!(
                    "Cannot move uploaded object into place at {}: {error}",
                    path.display()
                ))
            })?;
            Ok(())
        })();
        if let Err(error) = copy_result {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
        // No checksum yet (lazy finalize): skip the sidecar; the
        // worker writes it through `record_object_checksum` once
        // the hash is verified.
        if let Some(checksum) = checksum {
            if let Err(error) = self.write_sidecar(&path, checksum) {
                let _ = std::fs::remove_file(&path);
                return Err(error);
            }
        }
        Ok(())
    }


    /// Write the `.sha256` sidecar next to an object file
    /// (temporary file in the same directory, then rename into
    /// place). The durable sidecar is what `LocalProvider` prefers
    /// when it verifies a download, so an object written here is
    /// integrity-checked exactly like one written through
    /// `LocalProvider::upload`.
    fn write_sidecar(&self, path: &Path, checksum: &str) -> UnifiedResult<()> {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(".sha256");
        let sidecar = PathBuf::from(sidecar);
        let temp_name = format!(
            ".{}.tmp-{}",
            sidecar
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "checksum".to_string()),
            Uuid::new_v4()
        );
        let temp_path = sidecar
            .parent()
            .map(|parent| parent.join(&temp_name))
            .unwrap_or_else(|| PathBuf::from(temp_name));
        let result = (|| -> UnifiedResult<()> {
            let mut file = std::fs::File::create(&temp_path).map_err(|error| {
                UnifiedError::internal(format!("Cannot create checksum sidecar: {error}"))
            })?;
            file.write_all(checksum.as_bytes()).map_err(|error| {
                UnifiedError::internal(format!("Cannot write checksum sidecar: {error}"))
            })?;
            file.sync_all().map_err(|error| {
                UnifiedError::internal(format!("Cannot flush checksum sidecar: {error}"))
            })?;
            drop(file);
            std::fs::rename(&temp_path, &sidecar).map_err(|error| {
                UnifiedError::internal(format!(
                    "Cannot move checksum sidecar into place at {}: {error}",
                    sidecar.display()
                ))
            })?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        result
    }


    /// Ask the Storage Management module whether `bytes` may be
    /// uploaded for `owner` / `company_id`, through the
    /// `storage.object.upload_requested` event. Every hook result
    /// is consulted and the most restrictive answer wins: ANY
    /// `quota_exceeded` answer rejects the upload with 409 (fail
    /// closed), even when another handler answered `resolved`.
    /// Returns `Ok(())` when at least one handler resolved the
    /// request and none reported an exceeded quota, or when no
    /// handler answers at all (then the local quotas below decide).
    /// A `quota_exceeded` answer emits `document.quota.exceeded`.
    pub(crate) fn check_storage_quota(
        &self,
        owner: &str,
        company_id: &str,
        bytes: u64,
        mime: &str,
    ) -> UnifiedResult<()> {
        let context = self.bus().emit(
            "storage.object.upload_requested",
            json!({
                "module": "documents",
                "owner": owner,
                "company_id": if company_id.is_empty() { Value::Null } else { Value::String(company_id.to_string()) },
                "key_hint": "documents",
                "bytes": bytes,
                "mime": mime,
            }),
        );
        if context.results.is_empty() {
            // No Storage Management handler answered: fall through
            // to the local quota check.
            return self.check_local_quota(owner, company_id, bytes);
        }
        let mut exceeded: Option<Value> = None;
        let mut any_resolved = false;
        for answer in &context.results {
            if answer
                .get("quota_exceeded")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                && exceeded.is_none()
            {
                exceeded = Some(answer.clone());
            }
            if answer.get("resolved").and_then(|v| v.as_bool()).unwrap_or(false) {
                any_resolved = true;
            }
        }
        if let Some(answer) = exceeded {
            let detail = answer
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("Storage quota exceeded")
                .to_string();
            self.emit(
                events::QUOTA_EXCEEDED,
                json!({
                    "scope": answer.get("scope").cloned().unwrap_or(Value::Null),
                    "scope_id": answer.get("scope_id").cloned().unwrap_or(Value::Null),
                    "module": "documents",
                    "user_id": owner,
                    "company_id": company_id,
                    "bytes": bytes,
                    "detail": detail,
                }),
            );
            return Err(UnifiedError::new(
                format!("Upload rejected by storage quota: {detail}"),
                409,
            ));
        }
        if any_resolved {
            return Ok(());
        }
        // The module answered but did not resolve (no binding, or an
        // error): do not treat that as a quota rejection; the local
        // quotas still apply.
        self.check_local_quota(owner, company_id, bytes)
    }


    /// Local per-company / per-user quota check (fallback). A scope
    /// with no quota row is unlimited locally.
    fn check_local_quota(
        &self,
        owner: &str,
        company_id: &str,
        bytes: u64,
    ) -> UnifiedResult<()> {
        for (scope, scope_id) in [("company", company_id), ("user", owner)] {
            if scope_id.is_empty() {
                continue;
            }
            let key = format!("{scope}:{scope_id}");
            if let Some(quota) = self.quotas.get(&key) {
                if let Some(limit) = quota.bytes_limit {
                    if quota.used_bytes.saturating_add(bytes) > limit {
                        let detail = format!(
                            "Local {scope} quota exceeded for \"{scope_id}\": used {} + {bytes} exceeds the limit of {limit} bytes",
                            quota.used_bytes
                        );
                        self.emit(
                            events::QUOTA_EXCEEDED,
                            json!({
                                "scope": scope,
                                "scope_id": scope_id,
                                "module": "documents",
                                "user_id": owner,
                                "company_id": company_id,
                                "bytes": bytes,
                                "detail": detail,
                            }),
                        );
                        return Err(UnifiedError::new(
                            format!("Upload rejected by quota: {detail}"),
                            409,
                        ));
                    }
                }
            }
        }
        Ok(())
    }


    /// Add `bytes` to the local quota counters for the company and
    /// the user (rows are created lazily with no limit).
    pub(crate) fn add_local_usage(&self, owner: &str, company_id: &str, bytes: u64) {
        for (scope, scope_id) in [("company", company_id), ("user", owner)] {
            if scope_id.is_empty() {
                continue;
            }
            let key = format!("{scope}:{scope_id}");
            if let Some(mut quota) = self.quotas.get_mut(&key) {
                quota.used_bytes = quota.used_bytes.saturating_add(bytes);
                quota.updated_at = now();
            } else {
                let quota = DocumentQuota {
                    id: new_id(),
                    scope: scope.to_string(),
                    scope_id: scope_id.to_string(),
                    bytes_limit: None,
                    used_bytes: bytes,
                    created_at: now(),
                    updated_at: now(),
                };
                self.quotas.insert(key, quota);
            }
        }
    }


    /// Subtract `bytes` from the local quota counters (never below 0).
    pub(crate) fn remove_local_usage(&self, owner: &str, company_id: &str, bytes: u64) {
        for (scope, scope_id) in [("company", company_id), ("user", owner)] {
            if scope_id.is_empty() {
                continue;
            }
            let key = format!("{scope}:{scope_id}");
            if let Some(mut quota) = self.quotas.get_mut(&key) {
                quota.used_bytes = quota.used_bytes.saturating_sub(bytes);
                quota.updated_at = now();
            }
        }
    }
}
