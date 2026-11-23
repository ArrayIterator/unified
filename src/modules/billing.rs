// Billing / Invoicing module.
//
// Invoices built from orders (directly or automatically from the
// `order.created` hook), payment and voiding, structured rendering, and
// overdue detection. Interconnection happens only through the hook bus.
//
// Invoice numbering uses a configurable prefix plus a zero-padded
// sequence that is never reused. Taxes are computed per line from a
// configurable default rate, with tax-inclusive or tax-exclusive
// handling. Due dates default to the issue date plus the configured
// payment terms; overdue status is derived from the due date and the
// outstanding balance. Overdue reminders are emitted as the
// `billing.invoice.reminder` hook for the mail module to deliver -
// this module never sends mail itself. Credit notes reduce an
// invoice's balance, payments are recorded per invoice (partial
// payments included), and `/pdf-data` returns the full structured
// invoice payload shaped for a PDF renderer.
//
// Subscription lifecycle: the store owns the authoritative
// `Subscription` record and emits `subscription.created` when a
// subscription order is paid. Billing keeps a mirror
// (`SubscriptionLifecycle`) and runs the lifecycle engine over it:
// renewal invoices generated before the period ends, proration
// invoices from `subscription.upgraded` / `subscription.downgraded`,
// dunning on `payment.failed` (retries per the product's dunning
// settings, `notification.requested` on each failure and on final
// failure), renewal reminders before the period ends, trial-end
// transitions, and grace-period expiry. Billing never writes the
// store's records directly: every decision is applied through
// `subscription.lifecycle.apply`, which the store applies and
// answers with the public `subscription.*` event. Renewal charges
// are requested as `subscription.renewal_due` (the payments module
// owns the gateway charge; see the integration notes for the
// convergence point) and confirmed through `payment.captured`.
//
// Invoice templates and printing (batch 12): invoice templates
// are data, not hard-coded layouts (header/footer, logo storage
// reference, colours, columns, line-item layout, tax breakdown,
// payment instructions, footer notes, per-company templates with
// version history and restore, preview with sample data). Printing
// an invoice renders a print-ready HTML document with `@page`
// rules for the chosen paper size and queues the job through the
// printing module over the `printing.job.requested` event, then
// emits `billing.invoice.printed`; emailing an invoice sends the
// same document through `mail.send_requested` (the mail module has
// no attachment field, so the document travels as the HTML body).
// Binary PDF conversion is a deployment step, not a claim made
// here. Paper sizes are owned by the printing module; billing
// only names the size (or the explicit millimetre dimensions)
// the document was rendered for.


// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

pub mod ai_capabilities;
pub mod types;
pub mod state;
pub mod reference;
mod helpers;
mod handlers_invoices;
mod hooks_payments;
mod hooks_subscriptions;
pub mod module;
pub mod school_fees_types;
mod school_fees;
pub mod school_fees_handlers;
mod school_fees_charges;
pub mod templates;
mod templates_handlers;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::*;
pub use self::state::*;
pub use self::reference::*;
pub use self::helpers::*;
pub use self::handlers_invoices::*;
pub use self::hooks_payments::*;
pub use self::hooks_subscriptions::*;
pub use self::module::*;
pub use self::school_fees_types::*;
pub use self::school_fees::*;
pub use self::school_fees_handlers::*;
pub use self::school_fees_charges::*;
pub use self::templates::*;
pub use self::templates_handlers::*;
