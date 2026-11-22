-- Appointments tables for the Unified Pentagonal Project
-- (`src/modules/appointments.rs`): bookable services, staff
-- availability (working hours, breaks, days off), staff-service
-- assignments, bookings, and reminder rows.
-- Matches src/db/schema.rs conventions. PostgreSQL only.
-- Run with diesel_migrations (embedded via src/db/mod.rs).
--
-- THIS MIGRATION HAS NEVER BEEN EXECUTED AGAINST A LIVE
-- POSTGRESQL DATABASE. It is a persistence target for the
-- in-memory DashMap stores in `src/modules/appointments.rs`, which stay
-- authoritative until the migration is executed and verified.
--
-- Notes on the mapping (the in-memory DashMap store is still
-- authoritative; see TODO.md):
--   - `appointment_services.price` is NUMERIC(19,4) (money), never
--     negative; `duration_minutes` is 5..1440; `capacity` is >= 1
--     (1 = one client at a time, higher for group services).
--     `buffer_before_minutes` / `buffer_after_minutes`,
--     `cancellation_notice_hours`, and
--     `cancellation_fee_percent` are per-service overrides of the
--     module settings; NULL means "use the module setting".
--     `location_type` is validated in the module against the
--     configured `allowed_location_types` (default: in_person,
--     online, phone).
--   - `appointment_staff_availability.kind` is `working_hours`
--     (weekly recurring window: `weekday` 0 = Monday .. 6 =
--     Sunday, optional break inside the window, optional
--     `valid_from` / `valid_until`) or `day_off` (one calendar
--     `date`). The CHECKs below mirror the module validation.
--     `staff_user_id` is a plain VARCHAR reference to the users
--     module: staff are existing users, never duplicated here, so
--     there is no foreign key to `users`.
--   - `appointment_staff_services` is the assignment of a staff
--     member to a service they may perform; the UNIQUE pair keeps
--     one row per (staff, service).
--   - `appointments.status` is `pending_confirmation`, `confirmed`,
--     `cancelled`, `completed`, or `no_show`
--     (`AppointmentStatus`). `ends_at` is always computed
--     server-side as `starts_at + service duration`; the CHECK
--     below only enforces `ends_at > starts_at`.
--     `confirmation_code` is the 8-character code a guest uses to
--     look a booking up publicly; it is unique. `customer_user_id`
--     is NULL for guest bookings. `price` / `currency` snapshot
--     the service price at booking time.
--     `cancellation_fee_charged` is the fee actually applied when
--     cancelling inside the notice window (0 when cancelled in
--     time); the fee rule itself lives in the service row /
--     module settings.
--   - `appointment_reminders.offset_minutes` is how long before
--     the appointment start the reminder fires.
--     `scheduler_status` is `not_scheduled`, `queued`, or
--     `declined`: the scheduler does not know the
--     `appointments.reminders.send` job kind yet, so reminders
--     record `declined` until that kind is added to `KNOWN_KINDS`
--     in `scheduler.rs`.
--   - Reminder *rules* (the admin-editable definitions that
--     materialise reminder rows) have NO table in this migration:
--     they live in the in-memory store seeded by
--     `AppointmentsState::new`. A follow-up migration can add an
--     `appointment_reminder_rules` table if rules must survive a
--     restart.
--   - ATOMICITY NOTE (double-booking): the module checks for
--     conflicts in memory (check-then-insert), which is not atomic
--     under concurrency. A true database guarantee needs an
--     exclusion constraint over the buffered booking windows, but
--     the buffer lengths are per-service values, so the constraint
--     cannot be expressed as a plain range exclusion on this table
--     alone. The integrator follow-up is either a serialisable
--     transaction around the check-and-insert, or a materialised
--     occupied-range table with an EXCLUDE USING gist constraint
--     (requires the btree_gist extension). Until then the
--     in-memory check is the only defence, and it is documented as
--     such in `src/modules/appointments.rs`.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- One bookable service.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS appointment_services (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(200) NOT NULL,
    description TEXT,
    duration_minutes INTEGER NOT NULL CHECK (duration_minutes BETWEEN 5 AND 1440),
    price NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (price >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    capacity INTEGER NOT NULL DEFAULT 1 CHECK (capacity >= 1),
    requires_staff BOOLEAN NOT NULL DEFAULT TRUE,
    location_type VARCHAR(50) NOT NULL DEFAULT 'in_person',
    location VARCHAR(200),
    buffer_before_minutes INTEGER CHECK (buffer_before_minutes BETWEEN 0 AND 480),
    buffer_after_minutes INTEGER CHECK (buffer_after_minutes BETWEEN 0 AND 480),
    cancellation_notice_hours INTEGER CHECK (cancellation_notice_hours BETWEEN 0 AND 720),
    cancellation_fee_percent NUMERIC(5, 2) CHECK (cancellation_fee_percent BETWEEN 0 AND 100),
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    company_id VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- One staff availability row: working hours (with optional break) or
-- one day off.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS appointment_staff_availability (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    staff_user_id VARCHAR(128) NOT NULL,
    kind VARCHAR(20) NOT NULL CHECK (kind IN ('working_hours', 'day_off')),
    weekday INTEGER CHECK (weekday BETWEEN 0 AND 6),
    date DATE,
    start_minutes INTEGER CHECK (start_minutes BETWEEN 0 AND 1440),
    end_minutes INTEGER CHECK (end_minutes BETWEEN 0 AND 1440),
    break_start_minutes INTEGER CHECK (break_start_minutes BETWEEN 0 AND 1440),
    break_end_minutes INTEGER CHECK (break_end_minutes BETWEEN 0 AND 1440),
    valid_from DATE,
    valid_until DATE,
    reason VARCHAR(1000),
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (kind <> 'working_hours' OR (weekday IS NOT NULL AND start_minutes IS NOT NULL AND end_minutes IS NOT NULL AND start_minutes < end_minutes AND date IS NULL)),
    CHECK (kind <> 'day_off' OR (date IS NOT NULL AND weekday IS NULL AND start_minutes IS NULL AND end_minutes IS NULL)),
    CHECK (valid_from IS NULL OR valid_until IS NULL OR valid_from <= valid_until),
    CHECK (break_start_minutes IS NULL AND break_end_minutes IS NULL OR (break_start_minutes IS NOT NULL AND break_end_minutes IS NOT NULL AND break_start_minutes < break_end_minutes))
);

CREATE INDEX IF NOT EXISTS idx_appointment_staff_availability_staff
    ON appointment_staff_availability (staff_user_id);
CREATE INDEX IF NOT EXISTS idx_appointment_staff_availability_date
    ON appointment_staff_availability (staff_user_id, date);

-- ---------------------------------------------------------------------------
-- One assignment of a staff member to a service they may perform.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS appointment_staff_services (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    staff_user_id VARCHAR(128) NOT NULL,
    service_id UUID NOT NULL REFERENCES appointment_services (id) ON DELETE CASCADE,
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (staff_user_id, service_id)
);

CREATE INDEX IF NOT EXISTS idx_appointment_staff_services_staff
    ON appointment_staff_services (staff_user_id);
CREATE INDEX IF NOT EXISTS idx_appointment_staff_services_service
    ON appointment_staff_services (service_id);

-- ---------------------------------------------------------------------------
-- One booking.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS appointments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    service_id UUID NOT NULL REFERENCES appointment_services (id),
    staff_user_id VARCHAR(128),
    customer_user_id VARCHAR(128),
    customer_name VARCHAR(200) NOT NULL,
    customer_email VARCHAR(254) NOT NULL,
    customer_phone VARCHAR(40),
    starts_at TIMESTAMPTZ NOT NULL,
    ends_at TIMESTAMPTZ NOT NULL,
    status VARCHAR(30) NOT NULL DEFAULT 'pending_confirmation'
        CHECK (status IN ('pending_confirmation', 'confirmed', 'cancelled', 'completed', 'no_show')),
    source VARCHAR(20) NOT NULL DEFAULT 'customer'
        CHECK (source IN ('staff', 'customer', 'guest')),
    price NUMERIC(19, 4) NOT NULL DEFAULT 0 CHECK (price >= 0),
    currency CHAR(3) NOT NULL DEFAULT 'USD',
    cancellation_fee_charged NUMERIC(19, 4) CHECK (cancellation_fee_charged IS NULL OR cancellation_fee_charged >= 0),
    cancelled_at TIMESTAMPTZ,
    cancel_reason VARCHAR(1000),
    notes TEXT,
    confirmation_code VARCHAR(16) NOT NULL UNIQUE,
    company_id VARCHAR(128),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);

CREATE INDEX IF NOT EXISTS idx_appointments_service
    ON appointments (service_id);
CREATE INDEX IF NOT EXISTS idx_appointments_staff
    ON appointments (staff_user_id, starts_at);
CREATE INDEX IF NOT EXISTS idx_appointments_customer
    ON appointments (customer_user_id, starts_at);
CREATE INDEX IF NOT EXISTS idx_appointments_status
    ON appointments (status, starts_at);

-- ---------------------------------------------------------------------------
-- One reminder for one appointment.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS appointment_reminders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    appointment_id UUID NOT NULL REFERENCES appointments (id) ON DELETE CASCADE,
    offset_minutes INTEGER NOT NULL CHECK (offset_minutes BETWEEN 0 AND 43200),
    channels TEXT[] NOT NULL DEFAULT '{}',
    status VARCHAR(20) NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'sent', 'failed', 'cancelled')),
    scheduler_job_id VARCHAR(128),
    scheduler_status VARCHAR(20) NOT NULL DEFAULT 'not_scheduled'
        CHECK (scheduler_status IN ('not_scheduled', 'queued', 'declined')),
    sent_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_appointment_reminders_appointment
    ON appointment_reminders (appointment_id);
