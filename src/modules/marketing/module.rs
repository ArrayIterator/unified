// Marketing Automation module.
//
// Campaigns, dynamic segments, drip sequences, lead scoring, and
// send analytics, in the style of a full marketing-automation suite.
// This module never sends mail itself and never stores its own copy
// of CRM contacts or newsletter subscribers: sending goes through
// the Mail module (`mail.send_requested`), tracking and unsubscribe
// signals are consumed from the Mail and Newsletter modules, and
// contacts live in CRM. What this module owns is the automation
// around those facts: who should receive what, when, in which
// variant, and whether they consented to it.
//
// Routes (all under `/api/v1/marketing`, every route requires the
// `X-User-Id` and `X-Role` headers and a capability check through
// the `role.check` hook, failing closed):
//   Campaigns:
//     GET    /campaigns                   - paginated campaigns
//     POST   /campaigns                   - create a campaign
//     GET    /campaigns/{id}              - one campaign
//     PUT    /campaigns/{id}              - update a draft campaign
//     DELETE /campaigns/{id}              - delete a draft campaign
//     POST   /campaigns/{id}/schedule     - schedule a campaign
//     POST   /campaigns/{id}/send-now     - send a campaign immediately
//     POST   /campaigns/{id}/pause        - pause a scheduled/sending campaign
//     POST   /campaigns/{id}/resume       - resume a paused campaign
//     GET    /campaigns/{id}/stats        - campaign stats with rates
//     GET    /campaigns/{id}/variants     - list A/B variants
//     POST   /campaigns/{id}/variants     - create an A/B variant
//     PUT    /campaigns/{id}/variants/{variant_id} - update a variant
//     DELETE /campaigns/{id}/variants/{variant_id} - delete a variant
//     POST   /campaigns/{id}/ab-decide    - pick the A/B winner from real data
//   Segments:
//     GET    /segments                    - paginated segments
//     POST   /segments                    - create a segment
//     GET    /segments/{id}               - one segment
//     PUT    /segments/{id}               - update a segment
//     DELETE /segments/{id}               - delete a segment
//     POST   /segments/{id}/evaluate      - evaluate membership, record size history
//     GET    /segments/{id}/members       - paginated current members
//     GET    /segments/{id}/history       - paginated size history
//   Sequences (drips):
//     GET    /sequences                   - paginated sequences
//     POST   /sequences                   - create a sequence
//     GET    /sequences/{id}              - one sequence
//     PUT    /sequences/{id}              - update a sequence
//     DELETE /sequences/{id}              - delete a sequence
//     GET    /sequences/{id}/steps        - list steps in order
//     POST   /sequences/{id}/steps        - add a step
//     PUT    /sequences/{id}/steps/{step_id} - update a step
//     DELETE /sequences/{id}/steps/{step_id} - delete a step
//     POST   /sequences/{id}/enroll       - enroll one contact
//     GET    /sequences/{id}/enrollments  - paginated enrollments
//     GET    /sequences/{id}/funnel       - per-step funnel
//     POST   /sequences/process-due       - run all due steps now
//     POST   /enrollments/{id}/advance    - run the next due step for one enrollment
//   Scoring:
//     GET    /scores                      - paginated contact scores
//     GET    /scores/{contact_id}         - one contact score (decay applied)
//     POST   /scores/{contact_id}/record  - record an action, apply matching rules
//     GET    /score-rules                 - paginated scoring rules
//     POST   /score-rules                 - create a scoring rule
//     PUT    /score-rules/{id}            - update a scoring rule
//     DELETE /score-rules/{id}            - delete a scoring rule
//     POST   /scores/apply-decay          - recompute all scores with decay
//   Audience, consent, suppression, audit, settings:
//     POST   /audience/sync               - upsert contact snapshots (from CRM / sync jobs)
//     GET    /audience                    - paginated known audience contacts
//     POST   /unsubscribe                 - record a global or per-campaign unsubscribe;
//                                          admin-only, unless the request carries a signed
//                                          recipient token (see `sign_unsubscribe_token`);
//                                          email templates must link with such a token,
//                                          never with a bare email address
//     GET    /suppressions                - paginated suppression list
//     DELETE /suppressions/{id}           - remove a suppression record
//     GET    /sends                       - paginated send audit trail
//     GET    /sends/{id}                  - one send record
//     GET    /settings                    - marketing settings (configurable defaults)
//     PUT    /settings                    - update marketing settings
//
// Hook contracts:
//   Emits `mail.send_requested` for every accepted recipient (the
//   Mail module owns delivery; the returned job id is stored on the
//   send record so `mail.sent` / `mail.failed` can update it).
//   Emits `scheduler.enqueue` for scheduled campaigns and due
//   sequence steps; the scheduler calls back through
//   `marketing.campaign.send` and `marketing.sequence.process_due`.
//   Emits `marketing.campaign.created`, `marketing.campaign.sent`,
//   `marketing.campaign.ab.winner`, `marketing.segment.evaluated`,
//   `marketing.sequence.enrolled`, `marketing.sequence.step.sent`,
//   `marketing.sequence.completed`, `marketing.score.updated`,
//   `marketing.lead.hot` (hot-lead handoff: a contact whose score
//   crossed the configured hot threshold; CRM consumes it),
//   `marketing.send.skipped`, `marketing.unsubscribed`.
//   Listens to `mail.sent` / `mail.failed` (delivery outcomes),
//   `newsletter.open` / `newsletter.click` (tracking events, only
//   when they reference a marketing campaign id),
//   `newsletter.unsubscribed` (suppression),
//   `compliance.consent.recorded` (the name the compliance
//   module actually emits, carrying the decision in `granted`),
//   `compliance.consent.granted` / `compliance.consent.withdrawn`
//   and `privacy.consent.changed` (consent cache),
//   `forms.submission.received` and `store.order.completed`
//   (sequence enrollment triggers and scoring actions; see the
//   note on `hook_sequence_trigger`: the forms event carries no
//   submitter identity, and the store order carries a user id but
//   no email, so both resolve the contact through the known
//   audience and otherwise do nothing),
//   `marketing.campaign.send`, `marketing.sequence.process_due`,
//   `marketing.sequence.enroll`, `marketing.contacts.sync`,
//   `marketing.unsubscribe`, `marketing.score.record`.
//
// Notes on honesty:
//   - Records are kept in memory (same pattern as the other modules
//     in this tree); the migration `2026-11-22-000076_marketing` is
//     the persistence target and the in-memory stores stay
//     authoritative until it is executed and verified.
//   - Open/click tracking is reported only from tracking events
//     actually received on the hook bus. Campaigns sent through the
//     Mail module carry no tracking pixel (the Mail module sends
//     the body as given), so their open/click counts stay zero
//     until a tracking source reports them; nothing is estimated
//     or fabricated. A/B winner selection refuses to decide
//     (`422`) until every variant reached the configured minimum
//     sample of sends and at least one tracking event exists for
//     the chosen metric.
//   - Consent is fail-closed. Before every send the module checks
//     (1) the local suppression list, (2) the consent cache built
//     from compliance/privacy events, and (3) the
//     `compliance.consent.check` event. A contact with no granted
//     consent on record, or for whom no handler answers the check,
//     is skipped and the skip is written to the send audit trail.
//     The compliance module does not answer
//     `compliance.consent.check` yet; until it does, consent must
//     arrive through the consent events or the audience sync
//     payload (`consented: true`), and unverified contacts are
//     skipped rather than mailed.
//   - Segments evaluate only against the audience snapshots this
//     module knows (synced through `POST /audience/sync` or the
//     `marketing.contacts.sync` event, plus contacts seen through
//     enrollments and form/purchase events). Every evaluation
//     reports how many contacts were actually evaluated, so a
//     segment size is never presented as full CRM coverage. The
//     CRM module currently answers no contact-list event; when it
//     gains one, evaluation should ask it first.
//   - All behaviour lists are data-driven: segment fields and
//     operators, scoring actions, split rules, thresholds, and
//     decay are admin-editable settings, not code constants. The
//     values shipped in `MarketingSettings::default` are
//     configurable defaults, clearly marked as such.


use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::service_sequences::*;
use super::handlers_campaigns::*;
use super::handlers_segments::*;
use super::handlers_sequences::*;
use super::handlers_scores::*;
use super::handlers_audience::*;
use super::hooks::*;
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module.
// ---------------------------------------------------------------------------

pub struct MarketingModule {
    state: Arc<MarketingState>,
}

impl MarketingModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(MarketingState::new()),
        }
    }
}

impl Default for MarketingModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(MarketingModule::new()));
}

impl Module for MarketingModule {
    fn id(&self) -> &'static str {
        "marketing"
    }

    fn name(&self) -> &'static str {
        "Marketing Automation"
    }

    fn description(&self) -> &'static str {
        "Marketing automation: campaigns with A/B testing, dynamic segments over CRM contacts, drip sequences with branching, lead scoring with decay and hot-lead handoff, send analytics, and fail-closed consent and unsubscribe handling."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["mail", "newsletter", "crm", "compliance"]
    }

    fn requires(&self) -> &[&'static str] {
        &["mail", "newsletter", "crm", "compliance"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["dashmap", "serde_json", "chrono", "uuid"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("marketing", AI_CAPABILITIES);
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_MAIL_SENT,
            "marketing",
            Arc::new(move |ctx| hook_mail_outcome(&state, ctx, true)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MAIL_FAILED,
            "marketing",
            Arc::new(move |ctx| hook_mail_outcome(&state, ctx, false)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_NEWSLETTER_OPEN,
            "marketing",
            Arc::new(move |ctx| hook_tracking(&state, ctx, "open")),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_NEWSLETTER_CLICK,
            "marketing",
            Arc::new(move |ctx| hook_tracking(&state, ctx, "click")),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_NEWSLETTER_UNSUBSCRIBED,
            "marketing",
            Arc::new(move |ctx| {
                let email = ctx
                    .get("email")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if !email.is_empty() {
                    let _ = record_unsubscribe(&state, &email, None, Some("newsletter unsubscribe"));
                }
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_COMPLIANCE_CONSENT_RECORDED,
            "marketing",
            Arc::new(move |ctx| {
                // The compliance module records consent with this
                // event name and carries the decision in `granted`.
                let granted = ctx
                    .get("granted")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true);
                hook_consent(&state, ctx, granted, EVENT_COMPLIANCE_CONSENT_RECORDED);
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_COMPLIANCE_CONSENT_GRANTED,
            "marketing",
            Arc::new(move |ctx| hook_consent(&state, ctx, true, EVENT_COMPLIANCE_CONSENT_GRANTED)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_COMPLIANCE_CONSENT_WITHDRAWN,
            "marketing",
            Arc::new(move |ctx| hook_consent(&state, ctx, false, EVENT_COMPLIANCE_CONSENT_WITHDRAWN)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_PRIVACY_CONSENT_CHANGED,
            "marketing",
            Arc::new(move |ctx| {
                let granted = ctx
                    .get("granted")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false);
                hook_consent(&state, ctx, granted, EVENT_PRIVACY_CONSENT_CHANGED);
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_FORMS_SUBMISSION_RECEIVED,
            "marketing",
            Arc::new(move |ctx| hook_sequence_trigger(&state, ctx, "form_submitted")),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_STORE_ORDER_COMPLETED,
            "marketing",
            Arc::new(move |ctx| hook_sequence_trigger(&state, ctx, "purchase")),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_CAMPAIGN_SEND,
            "marketing",
            Arc::new(move |ctx| {
                let campaign_id = ctx
                    .get("campaign_id")
                    .and_then(|value| value.as_str())
                    .or_else(|| {
                        ctx.get("payload")
                            .and_then(|payload| payload.get("campaign_id"))
                            .and_then(|value| value.as_str())
                    })
                    .unwrap_or("")
                    .to_string();
                if campaign_id.is_empty() {
                    ctx.add_result(json!({ "error": "marketing.campaign.send requires 'campaign_id'" }));
                    return;
                }
                match execute_campaign_send(&state, &campaign_id) {
                    Ok(result) => ctx.add_result(result),
                    Err(error) => ctx.add_result(json!({ "error": error.message })),
                }
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_SEQUENCE_PROCESS_DUE,
            "marketing",
            Arc::new(move |ctx| {
                let (advanced, waiting, errors) = process_due_enrollments(&state);
                ctx.add_result(json!({ "advanced": advanced, "waiting": waiting, "errors": errors }));
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_SEQUENCE_ENROLL,
            "marketing",
            Arc::new(move |ctx| {
                let sequence_id = ctx
                    .get("sequence_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let contact_id = ctx
                    .get("contact_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let email = ctx
                    .get("email")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if sequence_id.is_empty() || contact_id.is_empty() || email.is_empty() {
                    ctx.add_result(json!({
                        "error": "marketing.sequence.enroll requires 'sequence_id', 'contact_id' and 'email'"
                    }));
                    return;
                }
                match enroll_contact(&state, &sequence_id, &contact_id, &email) {
                    Ok(Some(enrollment)) => {
                        ctx.add_result(json!({ "enrolled": true, "enrollment_id": enrollment.id }));
                    }
                    Ok(None) => {
                        ctx.add_result(json!({ "enrolled": false, "already_enrolled": true }));
                    }
                    Err(error) => ctx.add_result(json!({ "error": error.message })),
                }
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_CONTACTS_SYNC,
            "marketing",
            Arc::new(move |ctx| {
                let contacts = ctx
                    .get("contacts")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mut upserted = 0u64;
                for raw in contacts {
                    if let Ok(input) = serde_json::from_value::<AudienceSyncInput>(raw) {
                        if upsert_audience_contact(&state, input).is_ok() {
                            upserted += 1;
                        }
                    }
                }
                ctx.add_result(json!({ "upserted": upserted }));
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_UNSUBSCRIBE,
            "marketing",
            Arc::new(move |ctx| {
                let email = ctx
                    .get("email")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if email.is_empty() {
                    ctx.add_result(json!({ "error": "marketing.unsubscribe requires 'email'" }));
                    return;
                }
                let campaign_id = ctx
                    .get("campaign_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                match record_unsubscribe(&state, &email, campaign_id.as_deref(), None) {
                    Ok(record) => ctx.add_result(json!({ "unsubscribed": true, "suppression_id": record.id })),
                    Err(error) => ctx.add_result(json!({ "error": error.message })),
                }
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_MARKETING_SCORE_RECORD,
            "marketing",
            Arc::new(move |ctx| {
                let contact_id = ctx
                    .get("contact_id")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let email = ctx
                    .get("email")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let action = ctx
                    .get("action")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                if contact_id.is_empty() || email.is_empty() || action.is_empty() {
                    ctx.add_result(json!({
                        "error": "marketing.score.record requires 'contact_id', 'email' and 'action'"
                    }));
                    return;
                }
                let applied = apply_score_action(&state, &contact_id, &email, &action);
                ctx.add_result(json!({ "rules_applied": applied }));
            }),
        );
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/marketing/campaigns", owner: "marketing", description: "List campaigns", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns", owner: "marketing", description: "Create campaign", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/campaigns/{id}", owner: "marketing", description: "Get campaign", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/marketing/campaigns/{id}", owner: "marketing", description: "Update draft campaign", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/campaigns/{id}", owner: "marketing", description: "Delete draft campaign", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/schedule", owner: "marketing", description: "Schedule campaign", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/send-now", owner: "marketing", description: "Send campaign now", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/pause", owner: "marketing", description: "Pause campaign", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/resume", owner: "marketing", description: "Resume campaign", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/campaigns/{id}/stats", owner: "marketing", description: "Campaign stats", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/campaigns/{id}/variants", owner: "marketing", description: "List A/B variants", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/variants", owner: "marketing", description: "Create A/B variant", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/marketing/campaigns/{id}/variants/{variant_id}", owner: "marketing", description: "Update A/B variant", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/campaigns/{id}/variants/{variant_id}", owner: "marketing", description: "Delete A/B variant", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/campaigns/{id}/ab-decide", owner: "marketing", description: "Pick A/B winner from real data", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/segments", owner: "marketing", description: "List segments", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/segments", owner: "marketing", description: "Create segment", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/segments/{id}", owner: "marketing", description: "Get segment", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/marketing/segments/{id}", owner: "marketing", description: "Update segment", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/segments/{id}", owner: "marketing", description: "Delete segment", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/segments/{id}/evaluate", owner: "marketing", description: "Evaluate segment membership", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/segments/{id}/members", owner: "marketing", description: "List segment members", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/segments/{id}/history", owner: "marketing", description: "Segment size history", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/sequences", owner: "marketing", description: "List sequences", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/sequences", owner: "marketing", description: "Create sequence", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/sequences/{id}", owner: "marketing", description: "Get sequence", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/marketing/sequences/{id}", owner: "marketing", description: "Update sequence", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/sequences/{id}", owner: "marketing", description: "Delete sequence", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/sequences/{id}/steps", owner: "marketing", description: "List sequence steps", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/sequences/{id}/steps", owner: "marketing", description: "Add sequence step", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/marketing/sequences/{id}/steps/{step_id}", owner: "marketing", description: "Update sequence step", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/sequences/{id}/steps/{step_id}", owner: "marketing", description: "Delete sequence step", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/sequences/{id}/enroll", owner: "marketing", description: "Enroll contact in sequence", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/sequences/{id}/enrollments", owner: "marketing", description: "List sequence enrollments", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/sequences/{id}/funnel", owner: "marketing", description: "Sequence funnel", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/sequences/process-due", owner: "marketing", description: "Run due sequence steps", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/enrollments/{id}/advance", owner: "marketing", description: "Advance one enrollment", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/scores", owner: "marketing", description: "List contact scores", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/scores/{contact_id}", owner: "marketing", description: "Get contact score", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/scores/{contact_id}/record", owner: "marketing", description: "Record scoring action", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/score-rules", owner: "marketing", description: "List scoring rules", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/score-rules", owner: "marketing", description: "Create scoring rule", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/marketing/score-rules/{id}", owner: "marketing", description: "Update scoring rule", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/score-rules/{id}", owner: "marketing", description: "Delete scoring rule", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/scores/apply-decay", owner: "marketing", description: "Recompute scores with decay", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/audience/sync", owner: "marketing", description: "Sync audience contacts", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/audience", owner: "marketing", description: "List audience contacts", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/marketing/unsubscribe", owner: "marketing", description: "Record unsubscribe (admin, or recipient with a signed token)", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/suppressions", owner: "marketing", description: "List suppressions", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/marketing/suppressions/{id}", owner: "marketing", description: "Delete suppression", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/sends", owner: "marketing", description: "List send audit trail", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/sends/{id}", owner: "marketing", description: "Get send record", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/marketing/settings", owner: "marketing", description: "Get marketing settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/marketing/settings", owner: "marketing", description: "Update marketing settings", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/marketing")
                .app_data(web::Data::new(state))
                .route("/campaigns", web::get().to(list_campaigns))
                .route("/campaigns", web::post().to(create_campaign))
                .route("/campaigns/{id}", web::get().to(get_campaign))
                .route("/campaigns/{id}", web::put().to(update_campaign))
                .route("/campaigns/{id}", web::delete().to(delete_campaign))
                .route("/campaigns/{id}/schedule", web::post().to(schedule_campaign))
                .route("/campaigns/{id}/send-now", web::post().to(send_campaign_now))
                .route("/campaigns/{id}/pause", web::post().to(pause_campaign))
                .route("/campaigns/{id}/resume", web::post().to(resume_campaign))
                .route("/campaigns/{id}/stats", web::get().to(get_campaign_stats))
                .route("/campaigns/{id}/variants", web::get().to(list_variants))
                .route("/campaigns/{id}/variants", web::post().to(create_variant))
                .route("/campaigns/{id}/variants/{variant_id}", web::put().to(update_variant))
                .route("/campaigns/{id}/variants/{variant_id}", web::delete().to(delete_variant))
                .route("/campaigns/{id}/ab-decide", web::post().to(ab_decide))
                .route("/segments", web::get().to(list_segments))
                .route("/segments", web::post().to(create_segment))
                .route("/segments/{id}", web::get().to(get_segment))
                .route("/segments/{id}", web::put().to(update_segment))
                .route("/segments/{id}", web::delete().to(delete_segment))
                .route("/segments/{id}/evaluate", web::post().to(evaluate_segment_endpoint))
                .route("/segments/{id}/members", web::get().to(list_segment_members))
                .route("/segments/{id}/history", web::get().to(list_segment_history))
                .route("/sequences", web::get().to(list_sequences))
                .route("/sequences", web::post().to(create_sequence))
                .route("/sequences/{id}", web::get().to(get_sequence))
                .route("/sequences/{id}", web::put().to(update_sequence))
                .route("/sequences/{id}", web::delete().to(delete_sequence))
                .route("/sequences/{id}/steps", web::get().to(list_steps))
                .route("/sequences/{id}/steps", web::post().to(create_step))
                .route("/sequences/{id}/steps/{step_id}", web::put().to(update_step))
                .route("/sequences/{id}/steps/{step_id}", web::delete().to(delete_step))
                .route("/sequences/{id}/enroll", web::post().to(enroll_in_sequence))
                .route("/sequences/{id}/enrollments", web::get().to(list_enrollments))
                .route("/sequences/{id}/funnel", web::get().to(sequence_funnel))
                .route("/sequences/process-due", web::post().to(process_due_sequences))
                .route("/enrollments/{id}/advance", web::post().to(advance_enrollment_endpoint))
                .route("/scores", web::get().to(list_scores))
                .route("/scores/{contact_id}", web::get().to(get_score))
                .route("/scores/{contact_id}/record", web::post().to(record_score))
                .route("/score-rules", web::get().to(list_score_rules))
                .route("/score-rules", web::post().to(create_score_rule))
                .route("/score-rules/{id}", web::put().to(update_score_rule))
                .route("/score-rules/{id}", web::delete().to(delete_score_rule))
                .route("/scores/apply-decay", web::post().to(apply_decay))
                .route("/audience/sync", web::post().to(sync_audience))
                .route("/audience", web::get().to(list_audience))
                .route("/unsubscribe", web::post().to(unsubscribe))
                .route("/suppressions", web::get().to(list_suppressions))
                .route("/suppressions/{id}", web::delete().to(delete_suppression))
                .route("/sends", web::get().to(list_sends))
                .route("/sends/{id}", web::get().to(get_send))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }
}
