//! eSignature external module.

use super::*;


// In-memory state and workflow operations

/// In-memory store for the module. The `DashMap` store is
/// authoritative for the running process; a database migration is
/// the persistence target for requests, audit events, and
/// certificates.
pub struct EsignatureState {
    pub requests: DashMap<String, SignatureRequest>,
    pub audit: DashMap<String, Vec<AuditEvent>>,
    pub certificates: DashMap<String, Certificate>,
    pub certificates_by_request: DashMap<String, String>,
    pub links: DashMap<String, SigningLink>,
    pub reminders: DashMap<String, DateTime<Utc>>,
}

impl EsignatureState {
    pub fn new() -> Self {
        Self {
            requests: DashMap::new(),
            audit: DashMap::new(),
            certificates: DashMap::new(),
            certificates_by_request: DashMap::new(),
            links: DashMap::new(),
            reminders: DashMap::new(),
        }
    }

    fn append_audit(
        &self,
        request_id: &str,
        kind: &str,
        actor: &str,
        details: &str,
    ) -> AuditEvent {
        let mut chain = self.audit.entry(request_id.to_string()).or_default();
        let prev_hash = chain
            .last()
            .map(|event| event.hash.clone())
            .unwrap_or_else(|| GENESIS_HASH.to_string());
        let mut event = AuditEvent {
            id: Uuid::now_v7().to_string(),
            request_id: request_id.to_string(),
            kind: kind.to_string(),
            actor: actor.to_string(),
            details: details.to_string(),
            prev_hash: prev_hash.clone(),
            hash: String::new(),
            created_at: Utc::now(),
        };
        event.hash = sha256_hex(&audit_hash_input(
            &prev_hash,
            &event.id,
            &event.request_id,
            &event.kind,
            &event.actor,
            event.created_at,
            &event.details,
        ));
        chain.push(event.clone());
        event
    }

    pub(crate) fn audit_events(&self, request_id: &str) -> Vec<AuditEvent> {
        self.audit
            .get(request_id)
            .map(|events| events.clone())
            .unwrap_or_default()
    }

    /// Flip a request to `expired` when its expiry has passed. This
    /// is the only place expiry takes effect: there is no background
    /// job, and nothing auto-completes. The audit event and hook are
    /// emitted exactly once, by whichever read flips the status.
    fn refresh_expiry(&self, request_id: &str, hooks: Option<&HookBus>) {
        let mut flipped = false;
        if let Some(mut req) = self.requests.get_mut(request_id) {
            if req.status != RequestStatus::Expired
                && effective_status(&req, Utc::now()) == RequestStatus::Expired
            {
                req.status = RequestStatus::Expired;
                req.updated_at = Utc::now();
                flipped = true;
            }
        }
        if flipped {
            let tenant_id = self
                .requests
                .get(request_id)
                .map(|req| req.tenant_id.clone())
                .unwrap_or_default();
            self.append_audit(
                request_id,
                "expired",
                "system",
                "Signature request expired before all signers signed.",
            );
            if let Some(bus) = hooks {
                bus.emit(
                    events::REQUEST_EXPIRED,
                    json!({
                        "request_id": request_id,
                        "tenant_id": tenant_id,
                    }),
                );
            }
        }
    }

    /// Load a request, enforcing tenant isolation. A missing request
    /// and a request from another tenant are both 404, so callers
    /// cannot probe for other tenants' requests.
    pub(crate) fn load_for_tenant(
        &self,
        request_id: &str,
        tenant_id: &str,
        hooks: Option<&HookBus>,
    ) -> UnifiedResult<SignatureRequest> {
        let stored = self
            .requests
            .get(request_id)
            .map(|req| req.clone())
            .ok_or_else(|| UnifiedError::not_found("signature request not found"))?;
        if stored.tenant_id != tenant_id {
            return Err(UnifiedError::not_found("signature request not found"));
        }
        self.refresh_expiry(request_id, hooks);
        self.requests
            .get(request_id)
            .map(|req| req.clone())
            .ok_or_else(|| UnifiedError::not_found("signature request not found"))
    }

    /// Create a draft signature request.
    pub fn create_request(&self, input: CreateRequestInput) -> UnifiedResult<SignatureRequest> {
        if !valid_tenant_id(&input.tenant_id) {
            return Err(UnifiedError::bad_request(
                "tenant_id is required (letters, digits, '-' and '_' only, max 128 characters)",
            ));
        }
        if input.document_id.trim().is_empty() || input.document_id.len() > 512 {
            return Err(UnifiedError::bad_request(
                "document_id is required (max 512 characters)",
            ));
        }
        if !valid_sha256_hex(&input.document_sha256) {
            return Err(UnifiedError::bad_request(
                "document_sha256 must be 64 lowercase hex characters",
            ));
        }
        if input.title.trim().is_empty() || input.title.len() > 500 {
            return Err(UnifiedError::bad_request(
                "title is required (max 500 characters)",
            ));
        }
        if input.signers.is_empty() || input.signers.len() > MAX_SIGNERS {
            return Err(UnifiedError::bad_request(format!(
                "at least one signer is required (max {MAX_SIGNERS})"
            )));
        }
        if input.expires_at <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "expires_at must be in the future",
            ));
        }
        if let Some(callback_url) = &input.callback_url {
            validate_callback_url(callback_url)?;
        }
        let mut seen_orders = Vec::new();
        let mut seen_emails = Vec::new();
        let mut signers: Vec<Signer> = Vec::with_capacity(input.signers.len());
        for signer in &input.signers {
            if signer.name.trim().is_empty() || signer.name.len() > 200 {
                return Err(UnifiedError::bad_request(
                    "each signer needs a name (max 200 characters)",
                ));
            }
            if !valid_email(&signer.email) {
                return Err(UnifiedError::bad_request(format!(
                    "signer email '{}' is not a valid email address",
                    signer.email
                )));
            }
            if signer.role.trim().is_empty() || signer.role.len() > 100 {
                return Err(UnifiedError::bad_request(
                    "each signer needs a role (max 100 characters)",
                ));
            }
            if seen_orders.contains(&signer.order) {
                return Err(UnifiedError::bad_request(
                    "signer signing orders must be unique",
                ));
            }
            let email_lower = signer.email.to_lowercase();
            if seen_emails.contains(&email_lower) {
                return Err(UnifiedError::bad_request(
                    "each signer email may appear only once per request",
                ));
            }
            seen_orders.push(signer.order);
            seen_emails.push(email_lower);
            signers.push(Signer {
                id: Uuid::now_v7().to_string(),
                name: signer.name.trim().to_string(),
                email: signer.email.trim().to_string(),
                role: signer.role.trim().to_string(),
                order: signer.order,
                status: SignerStatus::Pending,
                signed_at: None,
                decline_reason: None,
                signature: None,
            });
        }
        // Signing order is the sort key, not the submission order.
        signers.sort_by_key(|signer| signer.order);
        let now = Utc::now();
        let request = SignatureRequest {
            id: Uuid::now_v7().to_string(),
            tenant_id: input.tenant_id,
            document_id: input.document_id,
            document_sha256: input.document_sha256.to_lowercase(),
            title: input.title.trim().to_string(),
            status: RequestStatus::Draft,
            contract_id: input.contract_id.filter(|id| !id.trim().is_empty()),
            signers,
            expires_at: input.expires_at,
            callback_url: input.callback_url,
            created_at: now,
            updated_at: now,
            completed_at: None,
            certificate_hash: None,
        };
        self.requests.insert(request.id.clone(), request.clone());
        let signer_count = request.signers.len();
        self.append_audit(
            &request.id,
            "created",
            "system",
            &format!(
                "Signature request created with {signer_count} signer(s) for document {}.",
                request.document_id
            ),
        );
        Ok(request)
    }

    /// List requests for one tenant, optionally filtered by status.
    /// Expiry is refreshed for each listed request, so the list
    /// never reports a stale `sent` status past expiry.
    pub fn list_requests(
        &self,
        tenant_id: &str,
        status: Option<RequestStatus>,
        hooks: Option<&HookBus>,
    ) -> Vec<SignatureRequest> {
        let mut out = Vec::new();
        for entry in self.requests.iter() {
            let req = entry.value();
            if req.tenant_id != tenant_id {
                continue;
            }
            if let Some(wanted) = status {
                if effective_status(req, Utc::now()) != wanted {
                    continue;
                }
            }
            out.push(req.id.clone());
        }
        out.sort();
        let mut requests = Vec::with_capacity(out.len());
        for id in out {
            self.refresh_expiry(&id, hooks);
            if let Some(req) = self.requests.get(&id) {
                requests.push(req.clone());
            }
        }
        requests
    }

    /// Send a draft request: flip it to `sent`, issue one signing
    /// link per signer, and emit `esignature.request.sent` for the
    /// mail/notifications modules to deliver.
    pub fn send_request(
        &self,
        request_id: &str,
        tenant_id: &str,
        secret: &[u8],
        hooks: &HookBus,
    ) -> UnifiedResult<(SignatureRequest, Vec<SigningLink>)> {
        let stored = self.load_for_tenant(request_id, tenant_id, Some(hooks))?;
        if stored.status != RequestStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "only a draft request can be sent (current status: {})",
                status_name(stored.status)
            )));
        }
        if Utc::now() > stored.expires_at {
            return Err(UnifiedError::unprocessable(
                "request expiry has passed; create a new request",
            ));
        }
        let now = Utc::now();
        let mut links = Vec::with_capacity(stored.signers.len());
        for signer in &stored.signers {
            let token = issue_token(secret, &stored.id, &signer.id, stored.expires_at);
            let link = SigningLink {
                request_id: stored.id.clone(),
                signer_id: signer.id.clone(),
                email: signer.email.clone(),
                signing_url: format!("/api/v1/esignature/sign/{token}"),
                expires_at: stored.expires_at,
                issued_at: now,
            };
            self.links.insert(
                format!("{}:{}", stored.id, signer.id),
                link.clone(),
            );
            links.push(link);
        }
        if let Some(mut req) = self.requests.get_mut(request_id) {
            req.status = RequestStatus::Sent;
            req.updated_at = now;
        }
        self.append_audit(
            request_id,
            "sent",
            "system",
            &format!(
                "Signature request sent to {} signer(s).",
                stored.signers.len()
            ),
        );
        hooks.emit(
            events::REQUEST_SENT,
            json!({
                "request_id": stored.id,
                "tenant_id": stored.tenant_id,
                "document_id": stored.document_id,
                "signers": stored.signers.iter().map(|s| json!({
                    "signer_id": s.id,
                    "email": s.email,
                    "signing_url": links.iter().find(|l| l.signer_id == s.id).map(|l| l.signing_url.clone()).unwrap_or_default(),
                })).collect::<Vec<_>>(),
                "expires_at": stored.expires_at.to_rfc3339(),
            }),
        );
        let request = self
            .requests
            .get(request_id)
            .map(|req| req.clone())
            .ok_or_else(|| UnifiedError::not_found("signature request not found"))?;
        Ok((request, links))
    }

    /// Resolve a signing token to its request and signer, checking
    /// the HMAC, the token expiry, and the request expiry.
    fn resolve_token(
        &self,
        token: &str,
        secret: &[u8],
        hooks: Option<&HookBus>,
    ) -> UnifiedResult<(SignatureRequest, Signer)> {
        let (request_id, signer_id, _) = verify_token(token, secret)?;
        self.refresh_expiry(&request_id, hooks);
        let request = self
            .requests
            .get(&request_id)
            .map(|req| req.clone())
            .ok_or_else(|| UnifiedError::not_found("signature request not found"))?;
        if request.status == RequestStatus::Expired {
            return Err(UnifiedError::new(
                "signature request has expired",
                410,
            ));
        }
        let signer = request
            .signers
            .iter()
            .find(|signer| signer.id == signer_id)
            .cloned()
            .ok_or_else(|| UnifiedError::not_found("signer not found on this request"))?;
        Ok((request, signer))
    }

    /// The signing ceremony payload for a token: the document hash,
    /// the signer, and the fields to sign.
    pub fn ceremony(
        &self,
        token: &str,
        secret: &[u8],
        hooks: Option<&HookBus>,
    ) -> UnifiedResult<Value> {
        let (request, signer) = self.resolve_token(token, secret, hooks)?;
        self.append_audit(
            &request.id,
            "viewed",
            &signer.email,
            &format!("Signer {} opened the signing ceremony.", signer.name),
        );
        Ok(json!({
            "request_id": request.id,
            "document_id": request.document_id,
            "document_sha256": request.document_sha256,
            "title": request.title,
            "status": request.status,
            "expires_at": request.expires_at.to_rfc3339(),
            "signer": {
                "id": signer.id,
                "name": signer.name,
                "email": signer.email,
                "role": signer.role,
                "order": signer.order,
                "status": signer.status,
                "already_signed": signer.status == SignerStatus::Signed,
            },
            "fields_to_sign": [
                { "name": "signature", "kind": "signature", "required": true },
                { "name": "printed_name", "kind": "text", "required": true },
                { "name": "date", "kind": "date", "required": true },
            ],
        }))
    }

    /// Record a signature from a signing token. Signers must sign
    /// in order: every lower-order signer must already have signed.
    /// When the last signer signs, the request completes and a
    /// completion certificate is issued.
    pub fn record_signature(
        &self,
        token: &str,
        secret: &[u8],
        input: SignInput,
        ip: &str,
        user_agent: &str,
        hooks: &HookBus,
    ) -> UnifiedResult<(SignatureRequest, Option<Certificate>)> {
        let (request, signer) = self.resolve_token(token, secret, Some(hooks))?;
        if request.status != RequestStatus::Sent
            && request.status != RequestStatus::PartiallySigned
        {
            return Err(UnifiedError::conflict(format!(
                "this request cannot be signed (current status: {})",
                status_name(request.status)
            )));
        }
        if signer.status == SignerStatus::Signed {
            return Err(UnifiedError::conflict(
                "this signer has already signed",
            ));
        }
        if signer.status == SignerStatus::Declined {
            return Err(UnifiedError::conflict("this signer declined the request"));
        }
        if input.typed_name.trim().is_empty() || input.typed_name.len() > 200 {
            return Err(UnifiedError::bad_request(
                "typed_name is required (max 200 characters)",
            ));
        }
        if !valid_sha256_hex(&input.signature_hash) {
            return Err(UnifiedError::bad_request(
                "signature_hash must be 64 lowercase hex characters (SHA-256 of the drawn signature)",
            ));
        }
        let blocked_by = request
            .signers
            .iter()
            .find(|other| other.order < signer.order && other.status != SignerStatus::Signed);
        if let Some(other) = blocked_by {
            return Err(UnifiedError::conflict(format!(
                "signer {} must sign before {} (signing order)",
                other.name, signer.name
            )));
        }
        let now = Utc::now();
        let record = SignatureRecord {
            typed_name: input.typed_name.trim().to_string(),
            signature_hash: input.signature_hash.to_lowercase(),
            ip: ip.to_string(),
            user_agent: user_agent.to_string(),
            signed_at: now,
        };
        let mut completed: Option<Certificate> = None;
        if let Some(mut req) = self.requests.get_mut(&request.id) {
            if let Some(target) = req.signers.iter_mut().find(|s| s.id == signer.id) {
                target.status = SignerStatus::Signed;
                target.signed_at = Some(now);
                target.signature = Some(record);
            }
            let all_signed = req
                .signers
                .iter()
                .all(|s| s.status == SignerStatus::Signed);
            if all_signed {
                req.status = RequestStatus::Completed;
                req.completed_at = Some(now);
            } else {
                req.status = RequestStatus::PartiallySigned;
            }
            req.updated_at = now;
        }
        self.append_audit(
            &request.id,
            "signed",
            &signer.email,
            &format!("Signer {} signed (typed name: {}).", signer.name, input.typed_name.trim()),
        );
        let updated = self
            .requests
            .get(&request.id)
            .map(|req| req.clone())
            .ok_or_else(|| UnifiedError::not_found("signature request not found"))?;
        if updated.status == RequestStatus::Completed {
            let certificate = self.issue_certificate(&updated);
            if let Some(mut req) = self.requests.get_mut(&request.id) {
                req.certificate_hash = Some(certificate.certificate_hash.clone());
            }
            self.append_audit(
                &request.id,
                "completed",
                "system",
                &format!(
                    "All signers signed; completion certificate {} issued.",
                    certificate.certificate_hash
                ),
            );
            hooks.emit(
                events::REQUEST_COMPLETED,
                json!({
                    "request_id": updated.id,
                    "tenant_id": updated.tenant_id,
                    "document_id": updated.document_id,
                    "certificate_hash": certificate.certificate_hash,
                    "completed_at": updated.completed_at.map(|t| t.to_rfc3339()),
                }),
            );
            completed = Some(certificate);
        }
        let final_request = self
            .requests
            .get(&request.id)
            .map(|req| req.clone())
            .ok_or_else(|| UnifiedError::not_found("signature request not found"))?;
        Ok((final_request, completed))
    }

    /// Decline a request from a signing token. Any pending signer
    /// may decline; a decline ends the whole request.
    pub fn decline_via_token(
        &self,
        token: &str,
        secret: &[u8],
        input: DeclineInput,
        hooks: &HookBus,
    ) -> UnifiedResult<SignatureRequest> {
        let (request, signer) = self.resolve_token(token, secret, Some(hooks))?;
        if request.status != RequestStatus::Sent
            && request.status != RequestStatus::PartiallySigned
        {
            return Err(UnifiedError::conflict(format!(
                "this request cannot be declined (current status: {})",
                status_name(request.status)
            )));
        }
        if signer.status != SignerStatus::Pending {
            return Err(UnifiedError::conflict(
                "this signer has already signed or declined",
            ));
        }
        if input.reason.trim().is_empty() || input.reason.len() > 1000 {
            return Err(UnifiedError::bad_request(
                "reason is required (max 1000 characters)",
            ));
        }
        let now = Utc::now();
        if let Some(mut req) = self.requests.get_mut(&request.id) {
            if let Some(target) = req.signers.iter_mut().find(|s| s.id == signer.id) {
                target.status = SignerStatus::Declined;
                target.decline_reason = Some(input.reason.trim().to_string());
            }
            req.status = RequestStatus::Declined;
            req.updated_at = now;
        }
        self.append_audit(
            &request.id,
            "declined",
            &signer.email,
            &format!("Signer {} declined: {}", signer.name, input.reason.trim()),
        );
        hooks.emit(
            events::REQUEST_DECLINED,
            json!({
                "request_id": request.id,
                "tenant_id": request.tenant_id,
                "signer_id": signer.id,
                "email": signer.email,
                "reason": input.reason.trim(),
            }),
        );
        self.requests
            .get(&request.id)
            .map(|req| req.clone())
            .ok_or_else(|| UnifiedError::not_found("signature request not found"))
    }

    /// Record a reminder for a request. Rate-limited to one per
    /// hour per request (429 otherwise). Delivery is not done here:
    /// the `esignature.reminder.requested` hook carries the payload
    /// for the mail/notifications modules.
    pub fn remind(
        &self,
        request_id: &str,
        tenant_id: &str,
        hooks: &HookBus,
    ) -> UnifiedResult<Value> {
        let request = self.load_for_tenant(request_id, tenant_id, Some(hooks))?;
        if request.status != RequestStatus::Sent
            && request.status != RequestStatus::PartiallySigned
        {
            return Err(UnifiedError::conflict(format!(
                "only a sent request can be reminded (current status: {})",
                status_name(request.status)
            )));
        }
        let now = Utc::now();
        if let Some(last) = self.reminders.get(request_id) {
            let next_allowed = *last + chrono::Duration::seconds(REMINDER_MIN_INTERVAL_SECS);
            if now < next_allowed {
                return Err(UnifiedError::new(
                    format!(
                        "a reminder was already sent for this request; next reminder allowed at {}",
                        next_allowed.to_rfc3339()
                    ),
                    429,
                ));
            }
        }
        self.reminders.insert(request_id.to_string(), now);
        let pending: Vec<String> = request
            .signers
            .iter()
            .filter(|signer| signer.status == SignerStatus::Pending)
            .map(|signer| signer.email.clone())
            .collect();
        self.append_audit(
            request_id,
            "reminded",
            "system",
            &format!("Reminder recorded for {} pending signer(s).", pending.len()),
        );
        hooks.emit(
            events::REMINDER_REQUESTED,
            json!({
                "request_id": request.id,
                "tenant_id": request.tenant_id,
                "pending_signers": pending,
                "expires_at": request.expires_at.to_rfc3339(),
            }),
        );
        Ok(json!({
            "request_id": request.id,
            "reminded_at": now.to_rfc3339(),
            "next_allowed_at": (now + chrono::Duration::seconds(REMINDER_MIN_INTERVAL_SECS)).to_rfc3339(),
            "pending_signers": pending,
        }))
    }

    /// Issue the completion certificate for a completed request and
    /// store it under its hash and under the request id.
    fn issue_certificate(&self, request: &SignatureRequest) -> Certificate {
        let events = self.audit_events(&request.id);
        let audit_head_hash = events
            .last()
            .map(|event| event.hash.clone())
            .unwrap_or_else(|| GENESIS_HASH.to_string());
        let signers: Vec<CertificateSigner> = request
            .signers
            .iter()
            .map(|signer| {
                let record = signer.signature.clone();
                CertificateSigner {
                    signer_id: signer.id.clone(),
                    name: signer.name.clone(),
                    email: signer.email.clone(),
                    role: signer.role.clone(),
                    order: signer.order,
                    signed_at: signer.signed_at.unwrap_or(request.updated_at),
                    typed_name: record
                        .as_ref()
                        .map(|r| r.typed_name.clone())
                        .unwrap_or_default(),
                    signature_hash: record
                        .as_ref()
                        .map(|r| r.signature_hash.clone())
                        .unwrap_or_default(),
                }
            })
            .collect();
        let completed_at = request.completed_at.unwrap_or(request.updated_at);
        let signers_json = serde_json::to_string(&signers).unwrap_or_default();
        let certificate_hash = sha256_hex(&format!(
            "{}|{}|{}|{}|{}",
            request.id,
            request.document_sha256,
            signers_json,
            audit_head_hash,
            completed_at.to_rfc3339()
        ));
        let certificate = Certificate {
            certificate_hash: certificate_hash.clone(),
            request_id: request.id.clone(),
            tenant_id: request.tenant_id.clone(),
            document_id: request.document_id.clone(),
            document_sha256: request.document_sha256.clone(),
            title: request.title.clone(),
            signers,
            audit_event_ids: events.iter().map(|event| event.id.clone()).collect(),
            audit_head_hash,
            completed_at,
            issued_at: Utc::now(),
        };
        self.certificates
            .insert(certificate_hash.clone(), certificate.clone());
        self.certificates_by_request
            .insert(request.id.clone(), certificate_hash);
        certificate
    }

    /// The completion certificate for a request (completed only).
    pub fn certificate(
        &self,
        request_id: &str,
        tenant_id: &str,
        hooks: Option<&HookBus>,
    ) -> UnifiedResult<Certificate> {
        let request = self.load_for_tenant(request_id, tenant_id, hooks)?;
        if request.status != RequestStatus::Completed {
            return Err(UnifiedError::conflict(format!(
                "no certificate exists yet (current status: {})",
                status_name(request.status)
            )));
        }
        let hash = self
            .certificates_by_request
            .get(request_id)
            .map(|hash| hash.clone())
            .ok_or_else(|| {
                UnifiedError::internal("request is completed but no certificate was issued")
            })?;
        self.certificates
            .get(&hash)
            .map(|cert| cert.clone())
            .ok_or_else(|| {
                UnifiedError::internal("request is completed but no certificate was issued")
            })
    }

    /// Verify a certificate hash: recompute the certificate hash
    /// from the stored fields and compare it in constant time.
    pub fn verify_certificate(
        &self,
        certificate_hash: &str,
    ) -> UnifiedResult<CertificateVerification> {
        if !valid_sha256_hex(certificate_hash) {
            return Err(UnifiedError::bad_request(
                "certificate_hash must be 64 lowercase hex characters",
            ));
        }
        let certificate = self
            .certificates
            .get(certificate_hash)
            .map(|cert| cert.clone())
            .ok_or_else(|| UnifiedError::not_found("certificate not found"))?;
        let signers_json = serde_json::to_string(&certificate.signers).unwrap_or_default();
        let expected = sha256_hex(&format!(
            "{}|{}|{}|{}|{}",
            certificate.request_id,
            certificate.document_sha256,
            signers_json,
            certificate.audit_head_hash,
            certificate.completed_at.to_rfc3339()
        ));
        let valid = constant_time_eq(
            expected.as_bytes(),
            certificate.certificate_hash.as_bytes(),
        );
        Ok(CertificateVerification {
            valid,
            certificate: Some(certificate),
        })
    }

    /// The audit trail for a request, with its chain verification.
    pub fn audit_trail(
        &self,
        request_id: &str,
        tenant_id: &str,
        hooks: Option<&HookBus>,
    ) -> UnifiedResult<Value> {
        let request = self.load_for_tenant(request_id, tenant_id, hooks)?;
        let events = self.audit_events(request_id);
        let verification = verify_audit_events(&events);
        Ok(json!({
            "request_id": request.id,
            "events": events,
            "chain_valid": verification.valid,
            "head_hash": events.last().map(|e| e.hash.clone()).unwrap_or_else(|| GENESIS_HASH.to_string()),
        }))
    }

    /// Verify the audit chain for a request.
    pub fn verify_audit_chain(
        &self,
        request_id: &str,
        tenant_id: &str,
        hooks: Option<&HookBus>,
    ) -> UnifiedResult<ChainVerification> {
        self.load_for_tenant(request_id, tenant_id, hooks)?;
        let events = self.audit_events(request_id);
        Ok(verify_audit_events(&events))
    }

    /// Link a completed request back to a contract from the
    /// `contracts.signed` hook. Matches on the `contract_id` the
    /// request was created with, or on the sealed content hash
    /// matching the request's document hash. Returns the linked
    /// request ids.
    pub(crate) fn link_completed_requests(&self, contract_id: &str, content_hash: &str) -> Vec<String> {
        let mut linked = Vec::new();
        for entry in self.requests.iter() {
            let req = entry.value();
            if req.status != RequestStatus::Completed {
                continue;
            }
            let matches_contract = !contract_id.is_empty()
                && req.contract_id.as_deref() == Some(contract_id);
            let matches_hash =
                !content_hash.is_empty() && req.document_sha256 == content_hash;
            if matches_contract || matches_hash {
                linked.push(req.id.clone());
            }
        }
        for id in &linked {
            let mut changed = false;
            if let Some(mut req) = self.requests.get_mut(id) {
                if req.contract_id.is_none() && !contract_id.is_empty() {
                    req.contract_id = Some(contract_id.to_string());
                    req.updated_at = Utc::now();
                    changed = true;
                }
            }
            if changed {
                self.append_audit(
                    id,
                    "linked",
                    "system",
                    &format!("Linked to contracts module contract {contract_id}."),
                );
            }
        }
        linked
    }
}

impl Default for EsignatureState {
    fn default() -> Self {
        Self::new()
    }
}
