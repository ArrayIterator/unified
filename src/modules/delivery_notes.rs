// Delivery notes module - delivery notes: DO documents
// generated from logistics shipments, with the
// shipment's items, sender, receiver, and addresses
// snapshotted at creation time, data-driven DO numbering,
// a pure status machine with proof-of-delivery gating on
// delivery, partial and full returns with rental-order
// linkage, and print-ready documents queued through the
// printing module.
//
// Routes live under `/api/v1/delivery-notes`:
//
//   Delivery notes:
//     GET/POST / - list DOs / create a draft DO.
//     GET  /by-shipment/{shipment_id} - DOs of one shipment.
//     POST /from-shipment/{shipment_id} - create a draft DO
//          from a logistics shipment id.
//     GET/PATCH/DELETE /{note_id} - one DO (update/delete
//          draft only).
//     POST /{note_id}/issue - draft -> issued.
//     POST /{note_id}/advance - next status, or a named target
//          when the machine allows it.
//     POST /{note_id}/deliver - link a proof of delivery and
//          record the recipient signature metadata
//          (in_transit -> delivered).
//     POST /{note_id}/return - partial or full return, with
//          per-item returned quantities and a reason.
//     POST /{note_id}/cancel - cancel (not delivered/returned).
//     POST /{note_id}/print - queue a print job through the
//          printing module, document returned inline.
//     GET  /{note_id}/print-preview - the print-ready document
//          without queueing a job.
//   Items:
//     GET/POST /{note_id}/items - list / add a line (draft only).
//     GET/PATCH/DELETE /{note_id}/items/{item_id} - one line
//          (draft only; returned moves only via return).
//   Settings and AI:
//     GET/PATCH /settings - per-tenant number format, print
//          defaults, and delivery gates.
//     POST /ai/{capability} - run a registered delivery notes
//          AI capability through the AI module's gated runner.
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory DO, item, and settings management; the DO status
// machine (draft -> issued -> in_transit -> delivered ->
// returned, plus cancelled, enforced by a single pure
// transition function); DO numbering from per-tenant settings
// (prefix, zero-pad width, yearly/monthly/never reset) with a
// per-tenant per-period sequence; snapshots frozen at creation
// so later shipment edits never rewrite a DO's history;
// proof-of-delivery linkage by id plus recipient signature
// metadata stored on the DO; partial and full returns with
// per-item returned quantities and a computed scope; print
// payloads queued through the printing module
// (`printing.job.requested`) with the print-ready HTML (or
// thermal lines) returned inline; and tenant scoping on every
// row with cross-tenant access reading as 404, never 403.
// What is NOT built here: durable storage until the migration
// runs (the DashMap stores are authoritative; migration
// `2026-11-22-000117_delivery_notes` is the persistence
// target); a live shipment lookup - the from-shipment create
// asks the logistics module through the
// `logistics.shipment.lookup` hook query, and no handler
// answers that query today, so the link is recorded as
// `unverified` (the same convention the logistics module
// itself uses for unverified store order links) and the
// response carries a warning - the snapshot in the request
// body is the document's source of truth, not the lookup,
// and the follow-up is for the logistics module to answer
// `logistics.shipment.lookup`; a company header lookup - the
// company module answers no header-lookup hook today (only
// `company.exists`, a boolean), so the printed header is the
// DO's snapshotted sender block plus the `company_id`
// reference in the payload; a direct printer driver or PDF
// generator (the printing module owns the queue, the ESC/POS
// stream, and dispatch to hardware, and converting the HTML
// to PDF bytes is a deployment step, as in printing's own
// honesty note); and actual email / push delivery (the
// notifications module owns delivery - this module only emits
// `notification.requested`).
//
// Shipment-link note: a DO may link a logistics shipment id,
// but this module never duplicates shipment rows and never
// reads the logistics tables directly. Linking asks the
// logistics module through the `logistics.shipment.lookup`
// hook query with `{"shipment_id": "..."}` and expects a
// result carrying `{"found": true}` (or `false`). When the
// logistics module denies it the link is recorded as
// `unverified` with a warning. When no handler answers at all
// - today the logistics module has no
// `logistics.shipment.lookup` answerer - the DO is still
// created but the link is recorded as `unverified` and the
// response carries a warning; the DO is NOT treated as linked
// to a real shipment until logistics confirms it. The
// follow-up is for the logistics module to answer
// `logistics.shipment.lookup`.
//
// Proof note: the logistics `ProofOfDelivery` carries a
// signature storage reference (`signature_storage_target` /
// `signature_storage_key`) but no signer name and no
// signed-at time, so the DO stores the recipient name, the
// signature reference, and the signed-at time itself
// (`recipient_name`, `recipient_signature_ref`,
// `recipient_signed_at`). The signature bytes live in the
// storage layer; only the reference is stored here.
//
// Events emitted on the hook bus:
//   - `delivery_notes.created`, `delivery_notes.updated`,
//     `delivery_notes.deleted`, `delivery_notes.issued`,
//     `delivery_notes.in_transit`, `delivery_notes.delivered`,
//     `delivery_notes.returned`, `delivery_notes.cancelled`,
//     `delivery_notes.printed`
//   - `notification.requested` (delivery completion, in the
//     notifications module's payload shape)
//   - `logistics.shipment.lookup` (query, not an
//     emit-and-forget event) with `{"shipment_id": "..."}`
//     when creating a DO from a shipment.
//   - `printing.job.requested` (query) with the print job
//     payload when printing a DO.
// Events this module listens to:
//   - `user.deleted` - anonymizes the user's DOs to
//     `deleted-user`; DOs are never deleted.
//   - `logistics.shipment.status_changed` - a shipment moving
//     out (picked up, in transit, out for delivery) moves an
//     issued DO to in transit (draft DOs are left alone).
//   - `logistics.shipment.delivered` - advances an in-transit
//     DO with a linked proof to delivered; a DO without a
//     proof stays in transit until the proof event arrives.
//   - `logistics.proof.created` - links the proof (id,
//     recipient name, signed-at) to the DOs of the shipment
//     and advances in-transit DOs to delivered.
//   - `logistics.shipment.returned` - returns the in-transit
//     and delivered DOs of the shipment (full scope).
//   - `logistics.shipment.cancelled` - cancels DOs that have
//     not been delivered or returned.
//   - `rental.order.returned` - returns the in-transit and
//     delivered DOs linked to that rental order (full scope).
//   - `delivery_notes.ai.summarize_delivery_note_requested`
//     - grounds the summary capability with the DO's stored
//     facts.
//   - `delivery_notes.ai.check_delivery_readiness_requested`
//     - grounds the readiness capability with the DO's
//     stored-facts checklist.
//
// Actor identity: the caller is the server-verified caller
// from `unified_core::caller::require_caller` (user id plus
// server-resolved role, both required for capability checks),
// exactly like the phone and sign modules. Capability checks
// use the `role.check` hook and fail closed (no answer means
// not allowed). All handlers take
// `web::Data<Arc<AppContext>>`, because the server registers
// `Arc<AppContext>` (see `src/http/server.rs`:
// `run_server(ctx: Arc<AppContext>)`);
// `web::Data<AppContext>` would fail to extract.

pub mod ai_capabilities;
mod handlers_ai;
mod handlers_items;
mod handlers_notes;
mod handlers_print;
mod handlers_settings;
mod helpers;
mod hooks;
mod module;
pub mod state;
pub mod types;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::register;
pub use self::module::DeliveryNotesModule;
pub use self::state::DeliveryNoteState;
pub use self::types::DeliveryNote;
pub use self::types::DeliveryNoteItem;
pub use self::types::DeliveryNoteSettings;
pub use self::types::DeliveryNoteStatus;
pub use self::types::DeliveryParty;
