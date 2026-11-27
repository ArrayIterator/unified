// Events Manager module - conferences, workshops, meetups, and
// other scheduled gatherings, with recurring events, ticket types,
// registrations, waitlists, attendee check-in, reminders, and
// calendar feeds.

// Split into per-concern files under `src/modules/events/`; the full
// module documentation is preserved at the top of `module.rs`.

pub mod ai_capabilities;
pub mod constants;
pub mod types;
pub mod store;
mod helpers;
mod validation;
mod service;
mod handlers_events;
mod handlers_tickets;
mod handlers_registrations;
mod handlers_reminders;
mod handlers_public;
mod hooks;
pub mod module;

// Re-exported from `constants`.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::EVENTS_REMINDERS_JOB_KIND;
pub use self::constants::EVENTS_REMINDERS_SEND_REQUESTED;

// Re-exported from `types`.
pub use self::types::EventStatus;
pub use self::types::EventVisibility;
pub use self::types::RecurrenceFrequency;
pub use self::types::Venue;
pub use self::types::RecurrenceRule;
pub use self::types::Event;
pub use self::types::EventOccurrence;
pub use self::types::TicketType;
pub use self::types::RegistrationStatus;
pub use self::types::Registration;
pub use self::types::Attendee;
pub use self::types::WaitlistEntry;
pub use self::types::EventReminder;
pub use self::types::CreateEventRequest;
pub use self::types::UpdateEventRequest;
pub use self::types::CreateTicketTypeRequest;
pub use self::types::UpdateTicketTypeRequest;
pub use self::types::RegisterRequest;
pub use self::types::AttendeeInput;
pub use self::types::CancelRegistrationRequest;
pub use self::types::RefundRequestInput;
pub use self::types::CheckInRequest;
pub use self::types::OverrideOccurrenceRequest;
pub use self::types::CreateReminderRequest;
pub use self::types::ExportQuery;

// Re-exported from `store`.
pub use self::store::EventsState;

// Re-exported from `module`.
pub use self::module::EventsModule;
pub use self::module::register;
