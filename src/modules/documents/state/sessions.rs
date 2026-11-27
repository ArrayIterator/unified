//! Documents state.

use super::*;

impl DocumentsState {

    /// Effective spool root for NEW sessions: the admin override
    /// (`settings.spool_dir`) when set, else
    /// `<storage>/temps/uploads` under the local storage root.
    /// Session directories hang under it by UTC date, then session
    /// id: `<root>/YYYY/MM/DD/<session-id>`.
    pub(crate) fn effective_spool_root(&self) -> PathBuf {
        match self.settings().spool_dir.as_deref() {
            Some(dir) if !dir.trim().is_empty() => PathBuf::from(dir.trim()),
            _ => self
                .storage_dir
                .join("temps")
                .join("uploads"),
        }
    }


    /// Date path for a UTC date (`YYYY/MM/DD`), appended under a
    /// spool root.
    fn date_path(date: DateTime<Utc>) -> PathBuf {
        PathBuf::from(format!("{}", date.format("%Y")))
            .join(format!("{}", date.format("%m")))
            .join(format!("{}", date.format("%d")))
    }


    /// Spool directory for a NEW session id, validating the id
    /// first so a session id can never escape the spool root as a
    /// path (ids are generated server-side, but the value also
    /// arrives in URL paths). The date is the session creation date
    /// (UTC), so parts live at
    /// `<root>/YYYY/MM/DD/<session-id>`.
    pub(crate) fn session_spool_dir(
        &self,
        session_id: &str,
        created_at: DateTime<Utc>,
    ) -> UnifiedResult<PathBuf> {
        if session_id.is_empty()
            || session_id == "."
            || session_id == ".."
            || session_id.contains('/')
            || session_id.contains('\\')
        {
            return Err(UnifiedError::bad_request(
                "Upload session id is not a valid spool directory name",
            ));
        }
        Ok(self
            .effective_spool_root()
            .join(Self::date_path(created_at))
            .join(session_id))
    }


    /// Spool directory recorded on a session (captured at creation,
    /// so a later settings change never moves an in-flight
    /// session's parts). Falls back to the effective root when the
    /// session carries no directory (sessions built before this
    /// field existed, or built by hand in tests); the fallback
    /// rebuilds the date-nested path from the session's creation
    /// date (UTC).
    pub(crate) fn spool_dir_for(&self, session: &UploadSession) -> PathBuf {
        if let Some(dir) = &session.spool_dir {
            if !dir.as_os_str().is_empty() {
                return dir.clone();
            }
        }
        self.session_spool_dir(&session.id, session.created_at)
            .unwrap_or_else(|_| self.effective_spool_root())
    }


    /// Apply config-file documents settings (`documents:` section
    /// of config.yaml) to the admin-managed settings. Called once
    /// at module startup; `None` values leave the defaults alone.
    /// The admin PATCH endpoint can override them at runtime; the
    /// config file wins again on the next restart.
    pub(crate) fn apply_config_settings(
        &self,
        config: &DocumentsConfig,
    ) -> UnifiedResult<()> {
        let mut settings = self.settings();
        if let Some(dir) = &config.spool_dir {
            let dir = dir.trim();
            if dir.is_empty() {
                return Err(UnifiedError::bad_request(
                    "documents.spool_dir must not be empty",
                ));
            }
            if dir.chars().any(|c| c == '\0' || c.is_control()) {
                return Err(UnifiedError::bad_request(
                    "documents.spool_dir contains invalid characters",
                ));
            }
            settings.spool_dir = Some(dir.to_string());
        }
        if let Some(value) = config.max_chunk_ram_bytes {
            if !(DocumentsConfig::MAX_CHUNK_RAM_MIN..=DocumentsConfig::MAX_CHUNK_RAM_MAX)
                .contains(&value)
            {
                return Err(UnifiedError::bad_request(format!(
                    "documents.max_chunk_ram_bytes must be between {} and {}",
                    DocumentsConfig::MAX_CHUNK_RAM_MIN,
                    DocumentsConfig::MAX_CHUNK_RAM_MAX
                )));
            }
            settings.max_chunk_ram_bytes = value;
        }
        if let Some(overrides) = &config.extensions {
            let registry = self.extensions().with_overrides(overrides);
            *self
                .extensions
                .write()
                .expect("extensions lock poisoned") = registry;
        }
        *self.settings.write().expect("settings lock poisoned") = settings;
        Ok(())
    }


    /// Write one validated part's bytes to its spool file
    /// (temporary file in the same directory, then rename, so a
    /// concurrent reader never sees a partial part) and return the
    /// metadata-only record kept in memory. Only one part (at most
    /// the 8 MiB chunk cap, and never more than
    /// `max_chunk_ram_bytes`) is ever in RAM at once. Part files
    /// are 6-digit zero-padded (`000001.part`, incremental).
    pub(crate) fn store_part_bytes(
        &self,
        session: &UploadSession,
        index: u32,
        bytes: &[u8],
        checksum: String,
    ) -> UnifiedResult<UploadPart> {
        let spool_dir = self.spool_dir_for(session);
        std::fs::create_dir_all(&spool_dir).map_err(|error| {
            UnifiedError::internal(format!(
                "Cannot create upload spool directory {}: {error}",
                spool_dir.display()
            ))
        })?;
        let path = spool_dir.join(format!("{index:06}.part"));
        let temp_name = format!(".{index:06}.part.tmp-{}", Uuid::new_v4());
        let temp_path = spool_dir.join(&temp_name);
        let write_result = (|| -> UnifiedResult<()> {
            let mut file = std::fs::File::create(&temp_path).map_err(|error| {
                UnifiedError::internal(format!("Cannot create spool file: {error}"))
            })?;
            file.write_all(bytes).map_err(|error| {
                UnifiedError::internal(format!("Cannot write spool file: {error}"))
            })?;
            file.sync_all().map_err(|error| {
                UnifiedError::internal(format!("Cannot flush spool file: {error}"))
            })?;
            drop(file);
            std::fs::rename(&temp_path, &path).map_err(|error| {
                UnifiedError::internal(format!(
                    "Cannot move spooled part into place at {}: {error}",
                    path.display()
                ))
            })?;
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
        Ok(UploadPart {
            index,
            size: bytes.len() as u64,
            sha256: checksum,
            path,
            received_at: now(),
        })
    }


    /// Stream-assemble a session's spooled parts, in index order,
    /// into a temporary file inside the session's spool directory,
    /// hashing incrementally: only one 64 KiB buffer is ever in
    /// RAM, however large the file. Returns the assembled path and
    /// its SHA-256 hex. The caller deletes the assembled file (or
    /// the whole spool directory) when done; on error the partial
    /// assembled file is removed here.
    pub(crate) fn assemble_session_file(
        &self,
        session: &UploadSession,
    ) -> UnifiedResult<(PathBuf, String)> {
        let (path, digest) = self.assemble_session_file_inner(session, true)?;
        Ok((path, digest.expect("hashing assemble always returns a digest")))
    }

    /// Stream-assemble a session's spooled parts WITHOUT hashing:
    /// the lazy-finalize path. The upload returns as soon as all
    /// parts are received; the whole-file hash is computed later
    /// by the finalize worker from the stored object, so hashing
    /// here would only duplicate that work on the request path.
    /// Returns the assembled path only. Same streaming guarantees
    /// as `assemble_session_file` (one 64 KiB buffer in RAM).
    pub(crate) fn assemble_session_file_unverified(
        &self,
        session: &UploadSession,
    ) -> UnifiedResult<PathBuf> {
        let (path, _) = self.assemble_session_file_inner(session, false)?;
        Ok(path)
    }

    /// Shared assembly loop: concatenate the spooled parts in
    /// index order into a temporary file, optionally folding the
    /// bytes into SHA-256 as they pass through.
    fn assemble_session_file_inner(
        &self,
        session: &UploadSession,
        hash: bool,
    ) -> UnifiedResult<(PathBuf, Option<String>)> {
        let spool_dir = self.spool_dir_for(session);
        let parts = self
            .parts
            .get(&session.id)
            .ok_or_else(|| UnifiedError::internal("Upload session parts are missing"))?;
        let assembled_name = format!(".assembled.tmp-{}", Uuid::new_v4());
        let assembled_path = spool_dir.join(&assembled_name);
        let result: UnifiedResult<(PathBuf, Option<String>)> = (|| {
            let mut output = std::fs::File::create(&assembled_path).map_err(|error| {
                UnifiedError::internal(format!("Cannot create assembled file: {error}"))
            })?;
            let mut hasher = Sha256::new();
            let mut total: u64 = 0;
            let mut buffer = [0u8; 65536];
            for index in 0..session.part_count {
                let part = parts
                    .get(&index)
                    .ok_or_else(|| UnifiedError::internal(format!("Part {index} is missing")))?;
                let mut input = std::fs::File::open(&part.path).map_err(|error| {
                    UnifiedError::internal(format!("Cannot open spooled part {index}: {error}"))
                })?;
                let mut copied: u64 = 0;
                loop {
                    let read = match input.read(&mut buffer) {
                        Ok(read) => read,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                            continue;
                        }
                        Err(error) => {
                            return Err(UnifiedError::internal(format!(
                                "Cannot read spooled part {index}: {error}"
                            )));
                        }
                    };
                    if read == 0 {
                        break;
                    }
                    if hash {
                        hasher.update(&buffer[..read]);
                    }
                    output.write_all(&buffer[..read]).map_err(|error| {
                        UnifiedError::internal(format!("Cannot write assembled file: {error}"))
                    })?;
                    copied += read as u64;
                }
                if copied != part.size {
                    return Err(UnifiedError::internal(format!(
                        "Spooled part {index} is {copied} bytes, expected {}",
                        part.size
                    )));
                }
                total += copied;
            }
            output.sync_all().map_err(|error| {
                UnifiedError::internal(format!("Cannot flush assembled file: {error}"))
            })?;
            drop(output);
            if total != session.total_size {
                return Err(UnifiedError::internal(
                    "Assembled size does not match the session total size",
                ));
            }
            let digest = if hash {
                let tag = hasher.finalize();
                Some(format!("{:x}", tag))
            } else {
                None
            };
            Ok((assembled_path.clone(), digest))
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&assembled_path);
        }
        result
    }


    /// Delete a session's spool directory and everything in it.
    /// Best-effort: a missing or undeletable directory is not an
    /// error (the orphan sweep removes leftovers later).
    pub(crate) fn remove_spool_dir(&self, session: &UploadSession) {
        let _ = std::fs::remove_dir_all(self.spool_dir_for(session));
    }


    /// Remove spool directories with no live session row (orphans
    /// left by crashed uploads or failed cleanups). The effective
    /// spool root and the default root are swept, so an override
    /// change never strands a directory. Session directories live
    /// at `<root>/YYYY/MM/DD/<session-id>`; empty date directories
    /// left behind are removed best-effort. Returns how many
    /// session directories were removed.
    pub(crate) fn sweep_orphan_spool_dirs(&self) -> usize {
        let mut roots = vec![
            self.effective_spool_root(),
            self.storage_dir.join("temps").join("uploads"),
        ];
        roots.dedup();
        let mut removed = 0;
        for root in roots {
            removed += self.sweep_orphan_spool_dirs_under(&root);
        }
        removed
    }


    /// Subdirectories of `dir`, or empty when it cannot be read.
    fn subdirs(dir: &Path) -> Vec<PathBuf> {
        match std::fs::read_dir(dir) {
            Ok(entries) => entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect(),
            Err(_) => Vec::new(),
        }
    }


    /// Sweep one spool root for orphaned session directories at
    /// `<root>/YYYY/MM/DD/<session-id>`. Empty date directories
    /// left behind are removed best-effort. Returns how many
    /// session directories were removed.
    fn sweep_orphan_spool_dirs_under(&self, root: &Path) -> usize {
        // Collect candidates first so the walk never mutates a
        // directory it is still reading.
        let mut candidates: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
        for year in Self::subdirs(root) {
            for month in Self::subdirs(&year) {
                for day in Self::subdirs(&month) {
                    for session in Self::subdirs(&day) {
                        candidates.push((
                            session,
                            vec![day.clone(), month.clone(), year.clone()],
                        ));
                    }
                }
            }
        }
        let mut removed = 0;
        for (session_dir, date_chain) in candidates {
            let name = session_dir
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if self.sessions.contains_key(&name) {
                continue;
            }
            if std::fs::remove_dir_all(&session_dir).is_ok() {
                removed += 1;
                // Best-effort: drop date directories left empty.
                for dir in date_chain {
                    let _ = std::fs::remove_dir(&dir);
                }
            }
        }
        removed
    }


    /// Atomically move a session from `active` to `completing`.
    /// The check-and-set runs under the session entry's lock, so
    /// two concurrent `complete` calls cannot both pass the
    /// `active` check: the second gets a 409 conflict. Callers
    /// must settle the session with [`finish_completion`] afterwards
    /// (completed on success, back to `active` on failure so the
    /// client can retry).
    pub(crate) fn try_begin_completion(&self, session_id: &str) -> UnifiedResult<()> {
        let mut entry = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| UnifiedError::not_found("Upload session not found"))?;
        if entry.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Upload session is {} and cannot be completed",
                entry.status
            )));
        }
        entry.status = "completing".to_string();
        entry.updated_at = now();
        Ok(())
    }


    /// Settle a session that [`try_begin_completion`] moved to
    /// `completing`: `completed` on success, back to `active` on
    /// failure (a failed finalize must not wedge the session).
    pub(crate) fn finish_completion(&self, session_id: &str, completed: bool) {
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.status = if completed {
                "completed".to_string()
            } else {
                "active".to_string()
            };
            entry.updated_at = now();
        }
    }


    /// Remove a session and its parts, unless the session already
    /// finished. Aborting a `completed` session (or one mid-
    /// completion, or one `received` - its document is already
    /// created and a finalize job is pending) is a conflict: the
    /// document is already finalized from it, so there is nothing
    /// left to abort.
    pub(crate) fn remove_session_if_abortable(
        &self,
        session_id: &str,
    ) -> UnifiedResult<UploadSession> {
        let session = self
            .sessions
            .get(session_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Upload session not found"))?;
        if session.status == "completed"
            || session.status == "completing"
            || session.status == "received"
        {
            return Err(UnifiedError::conflict(format!(
                "Upload session is {} and cannot be aborted",
                session.status
            )));
        }
        self.sessions.remove(session_id);
        self.parts.remove(session_id);
        self.remove_spool_dir(&session);
        Ok(session)
    }


    /// Expired sessions (and their parts) removed by the purge.
    /// Returns the number of sessions removed.
    pub(crate) fn purge_expired_sessions(&self) -> usize {
        let expired: Vec<UploadSession> = self
            .sessions
            .iter()
            .filter(|entry| entry.value().expires_at <= Utc::now())
            .map(|entry| entry.value().clone())
            .collect();
        let count = expired.len();
        for session in &expired {
            self.sessions.remove(&session.id);
            self.parts.remove(&session.id);
            self.remove_spool_dir(session);
        }
        // Orphaned spool directories (crashed uploads, failed
        // cleanups) are swept on every purge.
        self.sweep_orphan_spool_dirs();
        count
    }
}
