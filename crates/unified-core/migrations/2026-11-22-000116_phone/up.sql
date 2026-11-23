-- Phone module (`src/modules/phone/`): WebRTC voice and
-- video calling over the web (internet-only) — 1:1 and group
-- calls with ringing / active / held / ended / missed states,
-- store-and-forward signalling (offer, answer, ICE
-- candidates), call queues and routing rules as data,
-- presence per user, IVR menus and auto-response scripts as
-- data, consent-gated recording references, AI call summaries
-- on the call log, and per-tenant settings.
-- PostgreSQL only. Run with diesel_migrations (embedded via
-- crates/unified-core/src/db/).
--
-- Notes on the mapping (same as migration 000115_kyc: the
-- in-memory DashMap store is still authoritative):
--   - These tables are persistence targets only. The module keeps
--     its live data in memory and these tables are not yet its read
--     path. The migration has never been executed against a live
--     PostgreSQL database.
--   - All ids are Uuid, all timestamps are Timestamptz. `caller_id`
--     / `user_id` / `sender_id` / `recipient_id` / `started_by` /
--     `consent_by` are plain TEXT with NO foreign key on purpose,
--     matching the rental convention: users live in the users module
--     and this module must not assume its table or migration order.
--     `call_id` / `queue_id` / `menu_id` / `recording_id`
--     reference phone rows by id with NO foreign key, so deleting
--     one phone record never orphans another module's rows.
--   - `tenant_id` is plain TEXT on every row: phone data is
--     tenant business data and every query filters by it. A row
--     from another tenant reads as 404, never 403.
--   - Media never passes through the server: `phone_signals`
--     holds only the signalling payload (SDP or ICE JSON), and
--     `phone_recordings` holds only metadata plus the storage
--     layer reference (`storage_target` / `storage_key`). The
--     server NEVER relays media in this version, and there is
--     no media server, SFU, or TURN relay built in.
--   - Recording is consent-gated: `phone_calls.recording_consent`
--     must be true before a recording may start (unless the
--     tenant settings explicitly relax the gate), and
--     `phone_recordings.consent_given` records whether consent
--     was given for that recording, so a recording started
--     while the gate was relaxed stays visible as such.
--   - The AI never books, charges, transfers, or changes
--     anything on its own: `phone_calls.summary` is the stored
--     output of the `summarize_call` capability, and the AI
--     answer / routing capabilities return text and suggestions
--     inline only.
--   - Nothing is hard-coded: queues, routing rules, IVR menus
--     and nodes, and auto-response scripts are all rows,
--     created and edited by an admin through the API. This
--     migration seeds no rows: a tenant with no queues has no
--     queue to join, a tenant with no routing rules falls
--     through to the default (direct call or default queue),
--     and a tenant with no IVR menus has no menu to resolve.
--     Only numeric / boolean knobs have defaults, in
--     `phone_settings` below.
--   - There is no floating point money in this module: phone
--     carries no money fields.
--   - `phone_calls.status` is `ringing`, `active`, `held`,
--     `ended`, or `missed`; `phone_calls.outcome` is
--     `completed`, `missed`, `declined`, `busy`, `no_answer`,
--     or `cancelled`. `phone_calls.duration_secs` is computed
--     from `started_at` / `ended_at` when the call ends.
--   - Unique indexes enforce the module invariants in the
--     database too: one queue code, one routing-rule code,
--     one IVR-menu code, and one auto-response-script code
--     per tenant; one member per user per queue; one node per
--     key per IVR menu; one waiting waiter per caller per
--     queue; one participant row per user per call. The
--     single-active-invariants that cannot be expressed as a
--     unique index (one live recording per call, one live
--     call state machine) are enforced by the module.
--   - Queue wait expiry is recorded (`joined_at` plus the
--     queue's `max_wait_secs` and overflow target) but no
--     background expiry pass runs in this version: an expired
--     waiter is visible on the dashboard until an admin or a
--     future scheduler pass clears it.

CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- ---------------------------------------------------------------------------
-- Calls: one voice/video call. `caller_id` is the user who
-- initiated it; participants live in `phone_call_participants`.
-- `queue_id` / `ivr_menu_id` record how the call arrived
-- (NULL for a direct call). `recording_consent` gates
-- recording; `recording_consent_by` records who gave it.
-- `summary` is the AI call summary, written after the call
-- ends (NULL until then). `duration_secs` is computed from
-- `started_at` / `ended_at` when the call ends.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_calls (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    caller_id TEXT NOT NULL CHECK (char_length(caller_id) BETWEEN 1 AND 200),
    kind TEXT NOT NULL DEFAULT 'voice' CHECK (kind IN ('voice', 'video')),
    status TEXT NOT NULL DEFAULT 'ringing' CHECK (status IN ('ringing', 'active', 'held', 'ended', 'missed')),
    outcome TEXT CHECK (outcome IS NULL OR outcome IN ('completed', 'missed', 'declined', 'busy', 'no_answer', 'cancelled')),
    queue_id UUID,
    ivr_menu_id UUID,
    subject TEXT CHECK (subject IS NULL OR char_length(subject) BETWEEN 1 AND 200),
    recording_consent BOOLEAN NOT NULL DEFAULT FALSE,
    recording_consent_by TEXT CHECK (recording_consent_by IS NULL OR char_length(recording_consent_by) BETWEEN 1 AND 200),
    started_at TIMESTAMPTZ,
    ended_at TIMESTAMPTZ,
    duration_secs BIGINT CHECK (duration_secs IS NULL OR duration_secs >= 0),
    summary TEXT CHECK (summary IS NULL OR char_length(summary) BETWEEN 1 AND 5000),
    summary_model TEXT CHECK (summary_model IS NULL OR char_length(summary_model) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_phone_calls_tenant_status ON phone_calls (tenant_id, status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_phone_calls_tenant_caller ON phone_calls (tenant_id, caller_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_phone_calls_tenant_queue ON phone_calls (tenant_id, queue_id, created_at DESC) WHERE queue_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Participants: one participant of one call. `direction` is
-- relative to the participant: `outbound` for the caller,
-- `inbound` for everyone invited. A participant who never
-- answered ends as `missed`; one who declined ends as
-- `declined`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_call_participants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    call_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    direction TEXT NOT NULL DEFAULT 'inbound' CHECK (direction IN ('inbound', 'outbound')),
    status TEXT NOT NULL DEFAULT 'invited' CHECK (status IN ('invited', 'ringing', 'joined', 'declined', 'left', 'missed')),
    joined_at TIMESTAMPTZ,
    left_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_phone_participants_call_user ON phone_call_participants (call_id, user_id);
CREATE INDEX IF NOT EXISTS idx_phone_participants_tenant_call ON phone_call_participants (tenant_id, call_id, created_at ASC);

-- ---------------------------------------------------------------------------
-- Signals: one stored signalling message (store-and-forward).
-- `payload` is the SDP or ICE JSON object, opaque to the
-- server. `recipient_id` NULL means broadcast to the whole
-- call. `delivered` is set when the recipient fetches the
-- message from the signals inbox, so a reconnecting client
-- does not receive the same offer twice.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_signals (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    call_id UUID NOT NULL,
    sender_id TEXT NOT NULL CHECK (char_length(sender_id) BETWEEN 1 AND 200),
    recipient_id TEXT CHECK (recipient_id IS NULL OR char_length(recipient_id) BETWEEN 1 AND 200),
    kind TEXT NOT NULL CHECK (kind IN ('offer', 'answer', 'ice_candidate')),
    payload JSONB NOT NULL,
    delivered BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_phone_signals_tenant_call ON phone_signals (tenant_id, call_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_phone_signals_tenant_recipient ON phone_signals (tenant_id, recipient_id, created_at ASC) WHERE recipient_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_phone_signals_tenant_undelivered ON phone_signals (tenant_id, call_id, created_at ASC) WHERE delivered = FALSE;

-- ---------------------------------------------------------------------------
-- Queues: one call queue, as data. `ring_strategy` is
-- `sequential`, `simultaneous`, or `round_robin`;
-- `ring_timeout_secs` bounds one ring attempt;
-- `max_wait_secs` bounds the wait before overflow.
-- `overflow_queue_id` names the queue a caller falls through
-- to when the wait expires (NULL means the caller stays
-- queued); `overflow_action` names what happens when there
-- is no overflow queue (`ivr`, `hangup`, or a script code —
-- values are data).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_queues (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 128),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    ring_strategy TEXT NOT NULL DEFAULT 'sequential' CHECK (ring_strategy IN ('sequential', 'simultaneous', 'round_robin')),
    ring_timeout_secs BIGINT NOT NULL DEFAULT 30 CHECK (ring_timeout_secs BETWEEN 1 AND 600),
    max_wait_secs BIGINT NOT NULL DEFAULT 300 CHECK (max_wait_secs BETWEEN 1 AND 3600),
    overflow_queue_id UUID,
    overflow_action TEXT CHECK (overflow_action IS NULL OR char_length(overflow_action) BETWEEN 1 AND 200),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_phone_queues_tenant_code ON phone_queues (tenant_id, code);
CREATE INDEX IF NOT EXISTS idx_phone_queues_tenant_active ON phone_queues (tenant_id, active, code ASC);

-- ---------------------------------------------------------------------------
-- Queue members: one agent on a queue. `position` is the ring
-- order for sequential / simultaneous ringing;
-- `last_answered_at` drives round-robin (least recently
-- answered rings first).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_queue_members (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    queue_id UUID NOT NULL,
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    position BIGINT NOT NULL DEFAULT 0 CHECK (position >= 0),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    last_answered_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_phone_queue_members_queue_user ON phone_queue_members (queue_id, user_id);
CREATE INDEX IF NOT EXISTS idx_phone_queue_members_tenant_queue ON phone_queue_members (tenant_id, queue_id, position ASC);

-- ---------------------------------------------------------------------------
-- Queue waiters: one caller waiting in a queue. `status` is
-- `waiting`, `answered`, `abandoned`, or `timed_out`;
-- `call_id` is the call created when an admin answers the
-- waiter.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_queue_waiters (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    queue_id UUID NOT NULL,
    caller_id TEXT NOT NULL CHECK (char_length(caller_id) BETWEEN 1 AND 200),
    status TEXT NOT NULL DEFAULT 'waiting' CHECK (status IN ('waiting', 'answered', 'abandoned', 'timed_out')),
    call_id UUID,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    answered_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS idx_phone_queue_waiters_tenant_queue ON phone_queue_waiters (tenant_id, queue_id, status, joined_at ASC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_phone_queue_waiters_queue_caller_waiting ON phone_queue_waiters (queue_id, caller_id) WHERE status = 'waiting';

-- ---------------------------------------------------------------------------
-- Routing rules: one routing rule, as data. Rules are
-- evaluated in `priority` order (lowest first); the first
-- matching rule wins. `match_caller_id` is a caller pattern
-- (`*`, a `prefix*`, or an exact id; NULL matches every
-- caller). `match_time_start` / `match_time_end` are `HH:MM`
-- bounds (NULL means that side is open; an end before the
-- start wraps past midnight). `target_type` is `queue`,
-- `user`, or `ivr_menu`; `target_id` names the target (a
-- queue code, a user id, or a menu code).
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_routing_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 128),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    description TEXT CHECK (description IS NULL OR char_length(description) BETWEEN 1 AND 5000),
    priority BIGINT NOT NULL DEFAULT 0,
    match_caller_id TEXT CHECK (match_caller_id IS NULL OR char_length(match_caller_id) BETWEEN 1 AND 200),
    match_time_start TEXT CHECK (match_time_start IS NULL OR match_time_start ~ '^[0-9]{2}:[0-9]{2}$'),
    match_time_end TEXT CHECK (match_time_end IS NULL OR match_time_end ~ '^[0-9]{2}:[0-9]{2}$'),
    target_type TEXT NOT NULL CHECK (target_type IN ('queue', 'user', 'ivr_menu')),
    target_id TEXT NOT NULL CHECK (char_length(target_id) BETWEEN 1 AND 200),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_phone_routing_rules_tenant_code ON phone_routing_rules (tenant_id, code);
CREATE INDEX IF NOT EXISTS idx_phone_routing_rules_tenant_priority ON phone_routing_rules (tenant_id, active, priority ASC);

-- ---------------------------------------------------------------------------
-- Presence: one user's presence. `status` is the declared
-- status; `updated_at` is the last heartbeat, and a stale
-- entry reads as `offline` (see `PRESENCE_STALE_SECS` in the
-- module). `note` is a free-text status message.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_presence (
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    user_id TEXT NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 200),
    status TEXT NOT NULL DEFAULT 'offline' CHECK (status IN ('available', 'busy', 'do_not_disturb', 'offline')),
    note TEXT CHECK (note IS NULL OR char_length(note) BETWEEN 1 AND 200),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_phone_presence_tenant_status ON phone_presence (tenant_id, status, updated_at DESC);

-- ---------------------------------------------------------------------------
-- IVR menus: one IVR menu, as data. `greeting` is the text
-- the caller hears (or the text the AI voice reads) when the
-- menu opens; `greeting_audio_ref` names a documents/storage
-- object by reference when a recorded greeting exists.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_ivr_menus (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 128),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    greeting TEXT CHECK (greeting IS NULL OR char_length(greeting) BETWEEN 1 AND 5000),
    greeting_audio_ref TEXT CHECK (greeting_audio_ref IS NULL OR char_length(greeting_audio_ref) BETWEEN 1 AND 200),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_phone_ivr_menus_tenant_code ON phone_ivr_menus (tenant_id, code);
CREATE INDEX IF NOT EXISTS idx_phone_ivr_menus_tenant_active ON phone_ivr_menus (tenant_id, active, code ASC);

-- ---------------------------------------------------------------------------
-- IVR nodes: one key of one IVR menu. `key` is a single
-- digit, letter, `*`, or `#`. `action` is `transfer_queue`,
-- `transfer_user`, `play_message`, `run_script`,
-- `go_to_menu`, `repeat_menu`, or `hangup`. `target_id`
-- names the target (a queue code, a user id, a script code,
-- or a menu code); `message` is the text played for
-- `play_message`.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_ivr_nodes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    menu_id UUID NOT NULL,
    key TEXT NOT NULL CHECK (char_length(key) = 1),
    action TEXT NOT NULL CHECK (action IN ('transfer_queue', 'transfer_user', 'play_message', 'run_script', 'go_to_menu', 'repeat_menu', 'hangup')),
    target_id TEXT CHECK (target_id IS NULL OR char_length(target_id) BETWEEN 1 AND 200),
    message TEXT CHECK (message IS NULL OR char_length(message) BETWEEN 1 AND 5000),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_phone_ivr_nodes_menu_key ON phone_ivr_nodes (menu_id, key);
CREATE INDEX IF NOT EXISTS idx_phone_ivr_nodes_tenant_menu ON phone_ivr_nodes (tenant_id, menu_id, created_at ASC);

-- ---------------------------------------------------------------------------
-- Auto-response scripts: one script, as data. `trigger` is a
-- keyword or phrase, matched case-insensitively as a
-- substring of the caller's question. `response_template` is
-- the canned answer; `ai_capability` names the phone AI
-- capability that answers when no template is set. A script
-- needs one of the two.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_auto_response_scripts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    code TEXT NOT NULL CHECK (char_length(code) BETWEEN 1 AND 128),
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    trigger TEXT NOT NULL CHECK (char_length(trigger) BETWEEN 1 AND 200),
    response_template TEXT CHECK (response_template IS NULL OR char_length(response_template) BETWEEN 1 AND 10000),
    ai_capability TEXT CHECK (ai_capability IS NULL OR char_length(ai_capability) BETWEEN 1 AND 128),
    priority BIGINT NOT NULL DEFAULT 0,
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_phone_scripts_tenant_code ON phone_auto_response_scripts (tenant_id, code);
CREATE INDEX IF NOT EXISTS idx_phone_scripts_tenant_priority ON phone_auto_response_scripts (tenant_id, active, priority ASC);

-- ---------------------------------------------------------------------------
-- Recordings: one call recording. Only metadata lives here:
-- the bytes live in the storage layer and are referenced by
-- `storage_target` / `storage_key` (plus an optional signed
-- `url`) once attached. `consent_given` records whether the
-- call carried recording consent when the recording started.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_recordings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id TEXT NOT NULL CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    call_id UUID NOT NULL,
    started_by TEXT NOT NULL CHECK (char_length(started_by) BETWEEN 1 AND 200),
    status TEXT NOT NULL DEFAULT 'recording' CHECK (status IN ('recording', 'stopped', 'stored')),
    consent_given BOOLEAN NOT NULL DEFAULT FALSE,
    consent_by TEXT CHECK (consent_by IS NULL OR char_length(consent_by) BETWEEN 1 AND 200),
    storage_target TEXT CHECK (storage_target IS NULL OR char_length(storage_target) BETWEEN 1 AND 200),
    storage_key TEXT CHECK (storage_key IS NULL OR char_length(storage_key) BETWEEN 1 AND 1000),
    url TEXT CHECK (url IS NULL OR char_length(url) BETWEEN 1 AND 2000),
    size_bytes BIGINT CHECK (size_bytes IS NULL OR size_bytes >= 0),
    duration_secs BIGINT CHECK (duration_secs IS NULL OR duration_secs >= 0),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ended_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_phone_recordings_tenant_call ON phone_recordings (tenant_id, call_id, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_phone_recordings_tenant_status ON phone_recordings (tenant_id, status, created_at DESC);

-- ---------------------------------------------------------------------------
-- Per-tenant phone settings. `recording_requires_consent`
-- defaults to true and is the only switch that can relax the
-- consent gate — it can only be turned off by a manager, and
-- recordings started while it is off still record
-- `consent_given: false` on the row so the gap stays visible.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS phone_settings (
    tenant_id TEXT PRIMARY KEY CHECK (char_length(tenant_id) BETWEEN 1 AND 200),
    default_ring_timeout_secs BIGINT NOT NULL DEFAULT 30 CHECK (default_ring_timeout_secs BETWEEN 1 AND 600),
    max_call_duration_secs BIGINT NOT NULL DEFAULT 0 CHECK (max_call_duration_secs BETWEEN 0 AND 86400),
    recording_requires_consent BOOLEAN NOT NULL DEFAULT TRUE,
    missed_call_timeout_secs BIGINT NOT NULL DEFAULT 30 CHECK (missed_call_timeout_secs BETWEEN 1 AND 600),
    allow_group_calls BOOLEAN NOT NULL DEFAULT TRUE,
    max_group_participants BIGINT NOT NULL DEFAULT 8 CHECK (max_group_participants BETWEEN 2 AND 50),
    default_queue_code TEXT CHECK (default_queue_code IS NULL OR char_length(default_queue_code) BETWEEN 1 AND 128),
    default_ivr_menu_code TEXT CHECK (default_ivr_menu_code IS NULL OR char_length(default_ivr_menu_code) BETWEEN 1 AND 128),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
