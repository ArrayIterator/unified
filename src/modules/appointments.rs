// Appointments module: bookable services, staff calendars, slot
// generation, bookings with double-booking prevention, reschedule
// and cancel with a cancellation policy, reminders, a public
// booking flow, and an iCalendar export.
//
// Split into one file per concern. The full route list lives in
// `types.rs`; every public item is re-exported below so
// `crate::modules::appointments::<Item>` keeps working unchanged.

pub mod ai_capabilities;
pub mod types;
pub mod store;
mod common;
mod scheduling;
mod reminders;
mod ics;
mod validation;
mod handlers_services;
mod handlers_staff;
mod handlers_appointments;
mod handlers_reminders;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::AppointmentStatus;
pub use self::types::AppointmentService;
pub use self::types::StaffAvailability;
pub use self::types::StaffService;
pub use self::types::Appointment;
pub use self::types::AppointmentReminder;
pub use self::types::ReminderRule;
pub use self::types::AppointmentSettings;
pub use self::types::BookingSlot;
pub use self::types::CreateServiceRequest;
pub use self::types::UpdateServiceRequest;
pub use self::types::CreateAvailabilityRequest;
pub use self::types::UpdateAvailabilityRequest;
pub use self::types::AssignStaffServiceRequest;
pub use self::types::CreateAppointmentRequest;
pub use self::types::GuestBookingRequest;
pub use self::types::UpdateAppointmentRequest;
pub use self::types::RescheduleRequest;
pub use self::types::CancelRequest;
pub use self::types::CreateReminderRequest;
pub use self::types::CreateReminderRuleRequest;
pub use self::types::UpdateSettingsRequest;
pub use self::types::SlotsQuery;
pub use self::types::CalendarQuery;
pub use self::store::AppointmentsState;
pub use self::module::AppointmentsModule;
pub use self::module::register;
