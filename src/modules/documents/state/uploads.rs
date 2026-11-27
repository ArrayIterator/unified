//! Documents state.

use super::super::types::{
    MAX_CUSTOM_METADATA_KEYS, MAX_METADATA_ALT_TEXT_LEN, MAX_METADATA_CAPTION_LEN,
    MAX_METADATA_DESCRIPTION_LEN, MAX_METADATA_TITLE_LEN, UpdateDocumentMetadataRequest,
};
use super::super::validation::valid_metadata_key;
use super::*;

impl DocumentsState {

    /// Shared tail of every finalize path: allocate the version
    /// number under the per-document lock, insert the version and
    /// the document rows, emit the event, and charge the local
    /// quota. The scan already ran before this call (with the
    /// optimistic version number) so the lock is never held across
    /// a scan or an emit. `target_id` is the document id the
    /// caller already allocated (the scan event and the rows must
    /// agree on it - it is never re-allocated here). `checksum` is
    /// `Some` for the single-shot path (hashed in memory) and
    /// `None` for the chunked path (the lazy-finalize worker fills
    /// it in after hashing the stored object). `visibility` is the
    /// effective visibility the caller computed (scan-gated for new
    /// documents; the existing document's visibility for
    /// re-uploads, which never change it).
    fn insert_version(
        &self,
        target_id: String,
        name: String,
        module_id: String,
        company_id: String,
        owner_id: String,
        scope: String,
        mime: String,
        size: u64,
        checksum: Option<String>,
        storage_key: String,
        scan_status: ScanStatus,
        scan_threat: Option<String>,
        visibility: String,
        folder_id: String,
        existing_document_id: Option<String>,
    ) -> UnifiedResult<DocumentRecord> {
        // Re-uploads serialize on the per-document lock from the
        // version read through the document update: reading
        // `current_version`, adding 1, inserting the version row,
        // and updating the document row happen atomically, so two
        // concurrent re-uploads cannot compute the same version
        // number and collide. The guard is only held across
        // synchronous map work, never across an await or an emit.
        // A brand-new document needs no lock: its id is fresh and
        // no other caller can know it yet.
        let version_lock = existing_document_id
            .as_ref()
            .map(|id| self.document_lock(id));
        let _version_guard = version_lock
            .as_ref()
            .map(|lock| lock.lock().expect("document lock poisoned"));
        let version_number = self.next_version_number(&existing_document_id);
        // Re-check the target still exists while holding the lock,
        // before any row is inserted: a concurrent delete between
        // the early check above and this point must not leave an
        // orphan version row behind.
        let reupload_base = match &existing_document_id {
            Some(id) => Some(self.load_document(id)?),
            None => None,
        };
        let version = DocumentVersion {
            id: new_id(),
            document_id: target_id.clone(),
            version: version_number,
            checksum: checksum.clone(),
            storage_key: storage_key.clone(),
            size,
            mime: mime.clone(),
            uploader_id: owner_id.clone(),
            scan_status: scan_status.as_str().to_string(),
            scan_threat: scan_threat.clone(),
            created_at: now(),
        };
        self.versions.insert(version.id.clone(), version.clone());
        let (record, is_reupload) = match &existing_document_id {
            Some(_) => {
                let mut document = reupload_base
                    .expect("re-upload base was loaded under the lock above");
                document.name = name.clone();
                document.mime = mime.clone();
                document.size = size;
                document.checksum = checksum.clone();
                document.storage_key = storage_key.clone();
                document.current_version = version_number;
                document.version_count += 1;
                document.scan_status = scan_status.as_str().to_string();
                document.scan_threat = scan_threat.clone();
                document.updated_at = now();
                self.documents.insert(document.id.clone(), document.clone());
                (document, true)
            }
            None => {
                let document = DocumentRecord {
                    id: target_id.clone(),
                    name: name.clone(),
                    // A new document's title defaults to the
                    // uploaded file name; the owner renames it
                    // through `PATCH /api/v1/documents/{id}`.
                    // A re-upload never touches the title: the
                    // branch above leaves the stored title alone,
                    // so an explicit rename survives a new version.
                    title: name.clone(),
                    alt_text: None,
                    caption: None,
                    description: None,
                    custom_metadata: serde_json::Map::new(),
                    module_id: module_id.clone(),
                    company_id: company_id.clone(),
                    owner_id: owner_id.clone(),
                    scope,
                    mime: mime.clone(),
                    size,
                    checksum: checksum.clone(),
                    storage_key: storage_key.clone(),
                    current_version: 1,
                    version_count: 1,
                    scan_status: scan_status.as_str().to_string(),
                    scan_threat: scan_threat.clone(),
                    visibility: visibility.clone(),
                    // A re-upload keeps the existing document's
                    // folder: the branch above never assigns this
                    // field, so `folder_id` only applies to new
                    // documents.
                    folder_id: folder_id.clone(),
                    created_at: now(),
                    updated_at: now(),
                };
                self.documents.insert(document.id.clone(), document.clone());
                (document, false)
            }
        };
        drop(_version_guard);
        if is_reupload {
            self.emit(
                events::VERSION_CREATED,
                serde_json::to_value(&version).unwrap_or(Value::Null),
            );
        } else {
            self.emit(
                events::DOCUMENT_UPLOADED,
                serde_json::to_value(&record).unwrap_or(Value::Null),
            );
        }
        // Instant search: the Search module indexes the document
        // from this event (title + description), on create and on
        // re-upload alike.
        self.emit_search_index(&record);
        self.add_local_usage(&owner_id, &company_id, size);
        Ok(record)
    }


    /// Streaming finalize for a completed chunked session: the
    /// assembled file stays on disk for every step - MIME sniffing
    /// reads only the file head, the object is copied into the
    /// store chunk by chunk, and the virus scan reads the file
    /// (skipped without touching it when no scanner is configured).
    /// The whole file is never loaded into RAM. `tenant_id` is the
    /// resolved tenant UUID from `require_tenant` (never optional:
    /// a tenant upload without a resolved tenant is rejected by
    /// the handler before it reaches this function). `kind` is the
    /// storage kind when the caller specifies one explicitly;
    /// `None` derives it from the file name via the extension
    /// registry (known extension -> its kind, unknown -> `others`).
    ///
    /// LAZY FINALIZE: the whole-file hash is NOT computed here.
    /// `expected_sha256` (the client-supplied hash, validated at
    /// session initiation) is only format-validated by the key
    /// builder; the object is stored without a `.sha256` sidecar
    /// and the document row is created with `checksum: None`. The
    /// caller (complete_session) creates a `finalize_jobs` row and
    /// the worker hashes the stored object afterwards. `visibility`
    /// is the requested visibility; the scan gate applies it only
    /// when the scan verdict is `Clean` (a skipped/error/infected
    /// scan lands the document private). Re-uploads keep the
    /// existing document's visibility.
    pub(crate) async fn finalize_upload_from_file(
        &self,
        name: String,
        module_id: String,
        company_id: String,
        owner_id: String,
        scope: String,
        tenant_id: Uuid,
        kind: Option<ContentKind>,
        assembled_path: &Path,
        size: u64,
        expected_sha256: String,
        visibility: String,
        folder_id: String,
        existing_document_id: Option<String>,
    ) -> UnifiedResult<DocumentRecord> {
        // Fail before storing anything when the target document of a
        // re-upload does not exist, so no orphan object is left.
        if let Some(id) = &existing_document_id {
            self.load_document(id)?;
        }
        // MIME sniffing needs only the file head (magic bytes); the
        // extension fallback covers the rest.
        let head = read_file_head(assembled_path, 8192)?;
        let mime = mime_for(&name, &head);
        if size == 0 {
            return Err(UnifiedError::bad_request("Uploaded file is empty"));
        }
        if size > self.settings().max_upload_bytes {
            return Err(UnifiedError::new(
                format!(
                    "Upload exceeds the {} byte size limit",
                    self.settings().max_upload_bytes
                ),
                413,
            ));
        }
        self.check_allowlist(&module_id, &name, &mime, size)?;
        self.check_storage_quota(&owner_id, &company_id, size, &mime)?;
        // Scan BEFORE storing: the verdict decides the area the
        // object lands in (public only when clean), so the store
        // never has to move the object afterwards.
        let target_id = existing_document_id
            .clone()
            .unwrap_or_else(new_id);
        let optimistic_version = self.next_version_number(&existing_document_id);
        let (scan_status, scan_threat, _scanner) =
            self.scan_document_file(&target_id, optimistic_version, assembled_path, size);
        let effective_visibility = match &existing_document_id {
            // A re-upload never changes the document's visibility.
            Some(id) => self.load_document(id)?.visibility.clone(),
            None => {
                if visibility == "public" && matches!(scan_status, ScanStatus::Clean) {
                    "public".to_string()
                } else {
                    "private".to_string()
                }
            }
        };
        let (kind, extension) = match kind {
            Some(kind) => (kind, storage_extension(&name)),
            None => self.extensions().resolve(&name),
        };
        let storage_key = self
            .store_content_from_file(
                StorageScope::Tenant(tenant_id),
                StorageArea::from_visibility(&effective_visibility),
                kind,
                extension.as_deref(),
                &expected_sha256,
                size,
                assembled_path,
            )
            .await?;
        self.insert_version(
            target_id,
            name,
            module_id,
            company_id,
            owner_id,
            scope,
            mime,
            size,
            None,
            storage_key,
            scan_status,
            scan_threat,
            effective_visibility,
            folder_id,
            existing_document_id,
        )
    }


    /// Shared finalize path for every upload route: validate the
    /// allowlist and quota, scan, store the bytes,
    /// then create the document (or a new version of
    /// `existing_document_id`). Emits `document.uploaded` or
    /// `document.version.created`. `tenant_id` is the resolved
    /// tenant UUID from `require_tenant` (never optional: a tenant
    /// upload without a resolved tenant is rejected by the handler
    /// before it reaches this function). `kind` is the storage kind
    /// when the caller specifies one explicitly; `None` derives it
    /// from the file name via the extension registry (known
    /// extension -> its kind, unknown -> `others`). Unlike the
    /// chunked path this one hashes synchronously (the whole file
    /// is already in memory, capped at 100 MiB), so the document
    /// row carries its checksum immediately and no finalize job is
    /// created. `visibility` is the requested visibility; the scan
    /// gate applies it only when the verdict is `Clean`.
    /// Re-uploads keep the existing document's visibility.
    pub(crate) async fn finalize_upload(
        &self,
        name: String,
        module_id: String,
        company_id: String,
        owner_id: String,
        scope: String,
        tenant_id: Uuid,
        kind: Option<ContentKind>,
        bytes: Vec<u8>,
        visibility: String,
        folder_id: String,
        existing_document_id: Option<String>,
    ) -> UnifiedResult<DocumentRecord> {
        // Fail before storing anything when the target document of a
        // re-upload does not exist, so no orphan object is left.
        if let Some(id) = &existing_document_id {
            self.load_document(id)?;
        }
        let mime = mime_for(&name, &bytes);
        let size = bytes.len() as u64;
        if size == 0 {
            return Err(UnifiedError::bad_request("Uploaded file is empty"));
        }
        if size > self.settings().max_upload_bytes {
            return Err(UnifiedError::new(
                format!(
                    "Upload exceeds the {} byte size limit",
                    self.settings().max_upload_bytes
                ),
                413,
            ));
        }
        self.check_allowlist(&module_id, &name, &mime, size)?;
        self.check_storage_quota(&owner_id, &company_id, size, &mime)?;
        let checksum = sha256_hex(&bytes);
        // Scan BEFORE storing: the verdict decides the area the
        // object lands in (public only when clean).
        let target_id = existing_document_id
            .clone()
            .unwrap_or_else(new_id);
        let optimistic_version = self.next_version_number(&existing_document_id);
        let (scan_status, scan_threat, _scanner) =
            self.scan_document_bytes(&target_id, optimistic_version, &bytes);
        let effective_visibility = match &existing_document_id {
            Some(id) => self.load_document(id)?.visibility.clone(),
            None => {
                if visibility == "public" && matches!(scan_status, ScanStatus::Clean) {
                    "public".to_string()
                } else {
                    "private".to_string()
                }
            }
        };
        let (kind, extension) = match kind {
            Some(kind) => (kind, storage_extension(&name)),
            None => self.extensions().resolve(&name),
        };
        let storage_key = self
            .store_content(
                StorageScope::Tenant(tenant_id),
                StorageArea::from_visibility(&effective_visibility),
                kind,
                extension.as_deref(),
                &bytes,
            )
            .await?;
        self.insert_version(
            target_id,
            name,
            module_id,
            company_id,
            owner_id,
            scope,
            mime,
            size,
            Some(checksum),
            storage_key,
            scan_status,
            scan_threat,
            effective_visibility,
            folder_id,
            existing_document_id,
        )
    }


    /// Load a document or return a 404.
    pub(crate) fn load_document(&self, id: &str) -> UnifiedResult<DocumentRecord> {
        self.documents
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Document not found"))
    }


    /// Delete a document and all of its versions, deleting each
    /// version's stored copy and refunding the local quota usage.
    /// The quota refund is the sum of ALL version sizes, not just
    /// the current version's: every finalized version charged its
    /// size to the local quota (see `finalize_upload`), so
    /// refunding only the current version would leak the older
    /// versions' usage. `scope` is the request's resolved scope
    /// (the tenant the caller was authorized in): stored keys are
    /// scope-free, so the provider root is re-resolved from it
    /// here - a delete can never address another scope's tree.
    /// Version objects are deleted from the document's visibility
    /// area (every version lives in the same area).
    pub(crate) async fn delete_document(
        &self,
        scope: StorageScope,
        id: &str,
    ) -> UnifiedResult<DocumentRecord> {
        let document = self.load_document(id)?;
        let area = StorageArea::from_visibility(&document.visibility);
        let version_ids: Vec<String> = self
            .versions
            .iter()
            .filter(|entry| entry.value().document_id == id)
            .map(|entry| entry.key().clone())
            .collect();
        let mut total_size: u64 = 0;
        for version_id in version_ids {
            if let Some((_, version)) = self.versions.remove(&version_id) {
                total_size = total_size.saturating_add(version.size);
                // Each version owns its copy: deleting the version
                // deletes its key. Two versions may point at the
                // same key (a restore reuses the old version's
                // bytes); the second delete then fails harmlessly
                // and is ignored.
                if let Ok(provider) = self.provider() {
                    let _ = provider
                        .delete(&scope.provider_key_for_area(area, &version.storage_key))
                        .await;
                }
            }
        }
        self.remove_local_usage(&document.owner_id, &document.company_id, total_size);
        self.documents.remove(id);
        self.emit(
            events::DOCUMENT_DELETED,
            json!({
                "id": document.id,
                "name": document.name,
                "owner_id": document.owner_id,
                "company_id": document.company_id,
            }),
        );
        Ok(document)
    }

    /// Find a document by a stored (scope-free) key: the current
    /// version's key matches the document row directly; an old
    /// version's key resolves through its version row. Used by the
    /// `/files/private/` route, which is addressed by key, not by
    /// document id.
    pub(crate) fn find_by_storage_key(&self, storage_key: &str) -> Option<DocumentRecord> {
        if let Some(document) = self
            .documents
            .iter()
            .find(|entry| entry.value().storage_key == storage_key)
            .map(|entry| entry.value().clone())
        {
            return Some(document);
        }
        let document_id = self
            .versions
            .iter()
            .find(|entry| entry.value().storage_key == storage_key)
            .map(|entry| entry.value().document_id.clone())?;
        self.documents.get(&document_id).map(|entry| entry.value().clone())
    }

    /// Flip a document's visibility: move EVERY version's object
    /// between the public and private areas, then update the row.
    /// The stored keys never change, so the file URL never
    /// changes. All versions move because the public route serves
    /// ANY key under the public area with zero checks - a
    /// flipped-to-private document must not leave an old version
    /// behind there. A flip to `public` is refused unless the
    /// document's scan is clean (the scan gate); a flip to
    /// `private` is always allowed. Flipping to the current
    /// visibility is a no-op. When a move fails midway, the moves
    /// that already landed are rolled back and the row keeps the
    /// old visibility; a copy left behind by a failed rollback is
    /// an orphan for the storage sweeper (residual risk of the
    /// copy+delete move on remote providers - the local rename is
    /// atomic). Emits `document.visibility.flipped`.
    pub(crate) async fn flip_visibility(
        &self,
        scope: StorageScope,
        id: &str,
        visibility: String,
        actor: &str,
    ) -> UnifiedResult<DocumentRecord> {
        let document = self.load_document(id)?;
        if visibility == "public" && document.scan_status != "clean" {
            return Err(UnifiedError::bad_request(
                "Only a document with a clean scan can be made public",
            ));
        }
        if document.visibility == visibility {
            return Ok(document);
        }
        let previous = document.visibility.clone();
        let from_area = StorageArea::from_visibility(&previous);
        let to_area = StorageArea::from_visibility(&visibility);
        // Two versions may share one key (a restore reuses the old
        // version's bytes): move each distinct key once.
        let mut version_keys: Vec<String> = self
            .versions
            .iter()
            .filter(|entry| entry.value().document_id == id)
            .map(|entry| entry.value().storage_key.clone())
            .collect();
        version_keys.sort();
        version_keys.dedup();
        let provider = self.provider()?;
        let mut moved: Vec<(String, String)> = Vec::new();
        for key in &version_keys {
            let from = scope.provider_key_for_area(from_area, key);
            let to = scope.provider_key_for_area(to_area, key);
            match provider.move_object(&from, &to).await {
                Ok(()) => moved.push((from, to)),
                Err(error) => {
                    for (from, to) in moved.iter().rev() {
                        let _ = provider.move_object(to, from).await;
                    }
                    return Err(error);
                }
            }
        }
        let mut document = document;
        document.visibility = visibility.clone();
        document.updated_at = now();
        self.documents.insert(document.id.clone(), document.clone());
        self.emit(
            events::VISIBILITY_FLIPPED,
            json!({
                "document_id": document.id,
                "visibility": visibility,
                "previous": previous,
                "actor": actor,
            }),
        );
        Ok(document)
    }

    /// Publish one document to the Search module's index
    /// (`search.index`): the display title plus the description
    /// (falling back to the caption, then the alt text) as the
    /// snippet, and the document API URL. The Search module
    /// re-indexes the same id instead of duplicating it, so this
    /// is safe to call on every create and metadata update.
    pub(crate) fn emit_search_index(&self, document: &DocumentRecord) {
        let snippet = document
            .description
            .clone()
            .or(document.caption.clone())
            .or(document.alt_text.clone())
            .unwrap_or_default();
        self.emit(
            events::SEARCH_INDEX,
            json!({
                "id": document.id,
                "kind": "documents.document",
                "title": document.title,
                "snippet": snippet,
                "url": format!("/api/v1/documents/{}", document.id),
                "updated_at": document.updated_at.to_rfc3339(),
            }),
        );
    }

    /// Apply a metadata PATCH to one document: validate + merge
    /// under the per-document lock (so two concurrent PATCHes
    /// cannot interleave their read-modify-write), then publish
    /// the updated record to the Search module. A patch that
    /// changes nothing returns the record untouched (no
    /// `updated_at` bump, no re-index).
    pub(crate) fn update_document_metadata(
        &self,
        id: &str,
        patch: UpdateDocumentMetadataRequest,
    ) -> UnifiedResult<DocumentRecord> {
        let lock = self.document_lock(id);
        let _guard = lock.lock().expect("document lock poisoned");
        let mut document = self.load_document(id)?;
        if !apply_metadata_patch(&mut document, &patch)? {
            return Ok(document);
        }
        self.documents.insert(document.id.clone(), document.clone());
        drop(_guard);
        self.emit_search_index(&document);
        Ok(document)
    }
}

/// Merge a metadata PATCH into a loaded document record.
/// Validation runs BEFORE any field is assigned, so a rejected
/// patch leaves the record untouched. Returns true when any field
/// actually changed (an empty patch, or a patch that restates the
/// current values, is a no-op and does not bump `updated_at`).
/// `title` must be non-empty when present; an empty string clears
/// an optional text field back to `None` (same convention as the
/// settings `spool_dir` override). `custom_metadata` merges by
/// key: a JSON null removes the key, any other value sets it.
pub(crate) fn apply_metadata_patch(
    document: &mut DocumentRecord,
    patch: &UpdateDocumentMetadataRequest,
) -> UnifiedResult<bool> {
    // Validate everything first: the record is only assigned
    // below, once the whole patch is known-good.
    let title = match &patch.title {
        Some(value) => Some(require_text(value, "title", MAX_METADATA_TITLE_LEN)?),
        None => None,
    };
    let alt_text = match &patch.alt_text {
        Some(value) => Some(optional_metadata_text(
            value,
            "alt_text",
            MAX_METADATA_ALT_TEXT_LEN,
        )?),
        None => None,
    };
    let caption = match &patch.caption {
        Some(value) => Some(optional_metadata_text(
            value,
            "caption",
            MAX_METADATA_CAPTION_LEN,
        )?),
        None => None,
    };
    let description = match &patch.description {
        Some(value) => Some(optional_metadata_text(
            value,
            "description",
            MAX_METADATA_DESCRIPTION_LEN,
        )?),
        None => None,
    };
    let custom_metadata = match &patch.custom_metadata {
        Some(incoming) => {
            let mut merged = document.custom_metadata.clone();
            for (key, value) in incoming {
                let key = valid_metadata_key(key)?;
                if value.is_null() {
                    merged.remove(&key);
                } else {
                    merged.insert(key, value.clone());
                }
            }
            if merged.len() > MAX_CUSTOM_METADATA_KEYS {
                return Err(UnifiedError::bad_request(format!(
                    "custom_metadata must hold at most {MAX_CUSTOM_METADATA_KEYS} keys"
                )));
            }
            Some(merged)
        }
        None => None,
    };
    let mut changed = false;
    if let Some(value) = title {
        if document.title != value {
            document.title = value;
            changed = true;
        }
    }
    if let Some(value) = alt_text {
        if document.alt_text != value {
            document.alt_text = value;
            changed = true;
        }
    }
    if let Some(value) = caption {
        if document.caption != value {
            document.caption = value;
            changed = true;
        }
    }
    if let Some(value) = description {
        if document.description != value {
            document.description = value;
            changed = true;
        }
    }
    if let Some(merged) = custom_metadata {
        if document.custom_metadata != merged {
            document.custom_metadata = merged;
            changed = true;
        }
    }
    if changed {
        document.updated_at = now();
    }
    Ok(changed)
}

/// Validate one optional metadata text field: trims; an empty
/// string clears the field (`None`); otherwise the value must fit
/// `max` characters.
fn optional_metadata_text(value: &str, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(Some(trimmed.to_string()))
}
