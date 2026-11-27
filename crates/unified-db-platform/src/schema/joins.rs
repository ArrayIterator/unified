// Joinable tables and same-query groups for the unified-db-platform bounded context.
//
// Split out of `src/schema.rs` (see `src/lib.rs`); every `joinable!` and
// `allow_tables_to_appear_in_same_query!` declaration from the original
// file lives here, whole, so no join group spans files.

use super::*;
// Joinable tables in this bounded context.

diesel::joinable!(analytics_report_runs -> analytics_reports (definition_id));
diesel::joinable!(analytics_scheduled_reports -> analytics_reports (definition_id));
diesel::joinable!(email_send_log -> email_connections (connection_id));
diesel::joinable!(email_template_versions -> email_templates (template_id));
diesel::joinable!(newsletter_lists -> email_connections (default_connection_id));
diesel::joinable!(newsletter_subscribers -> newsletter_lists (list_id));
diesel::joinable!(newsletter_campaigns -> newsletter_lists (list_id));
diesel::joinable!(newsletter_campaigns -> email_templates (template_id));
diesel::joinable!(newsletter_campaigns -> email_connections (connection_id));
diesel::joinable!(newsletter_tracking_events -> newsletter_campaigns (campaign_id));
diesel::joinable!(newsletter_tracking_events -> newsletter_subscribers (subscriber_id));
diesel::joinable!(announcement_read_receipts -> announcements (announcement_id));
diesel::joinable!(notification_templates -> email_connections (email_connection_id));
diesel::joinable!(meeting_participants -> meeting_rooms (room_id));
diesel::joinable!(meeting_messages -> meeting_rooms (room_id));
diesel::joinable!(meeting_recordings -> meeting_rooms (room_id));

diesel::joinable!(event_occurrences -> events (event_id));
diesel::joinable!(event_ticket_types -> events (event_id));
diesel::joinable!(event_registrations -> events (event_id));
diesel::joinable!(event_registrations -> event_ticket_types (ticket_type_id));
diesel::joinable!(event_attendees -> event_registrations (registration_id));
diesel::joinable!(event_waitlist -> events (event_id));
diesel::joinable!(event_reminders -> events (event_id));

diesel::joinable!(bi_report_schedules -> bi_reports (report_id));

diesel::joinable!(portal_menus -> portals (portal_id));
diesel::joinable!(portal_views -> portals (portal_id));

diesel::joinable!(form_fields -> forms (form_id));
diesel::joinable!(form_submissions -> forms (form_id));
diesel::joinable!(form_submission_values -> form_submissions (submission_id));
diesel::joinable!(form_submission_values -> form_fields (field_id));

diesel::joinable!(contracts -> contract_templates (template_id));
diesel::joinable!(contract_signers -> contracts (contract_id));
diesel::joinable!(contract_audit_trail -> contracts (contract_id));
diesel::joinable!(contract_audit_trail -> contract_signers (signer_id));

diesel::joinable!(inbox_assignments -> inbox_threads (thread_id));
diesel::joinable!(inbox_thread_reads -> inbox_threads (thread_id));

diesel::joinable!(survey_questions -> surveys (survey_id));
diesel::joinable!(survey_distributions -> surveys (survey_id));
diesel::joinable!(survey_responses -> surveys (survey_id));
diesel::joinable!(survey_responses -> survey_distributions (distribution_id));
diesel::joinable!(survey_answers -> survey_responses (response_id));
diesel::joinable!(survey_answers -> survey_questions (question_id));

diesel::joinable!(community_topics -> community_categories (category_id));
diesel::joinable!(community_posts -> community_topics (topic_id));
diesel::joinable!(community_post_edits -> community_posts (post_id));
diesel::joinable!(community_votes -> community_posts (post_id));
diesel::joinable!(community_flags -> community_posts (post_id));

diesel::joinable!(workflow_runs -> workflows (workflow_id));
diesel::joinable!(workflow_run_steps -> workflow_runs (run_id));
diesel::joinable!(workflow_run_steps -> workflows (workflow_id));

diesel::joinable!(document_versions -> documents (document_id));
diesel::joinable!(document_upload_sessions -> documents (document_id));
diesel::joinable!(document_upload_parts -> document_upload_sessions (session_id));
diesel::joinable!(document_downloads -> documents (document_id));

diesel::joinable!(webinar_registrations -> webinars (webinar_id));
diesel::joinable!(webinar_polls -> webinars (webinar_id));
diesel::joinable!(webinar_poll_votes -> webinar_polls (poll_id));
diesel::joinable!(webinar_poll_votes -> webinars (webinar_id));
diesel::joinable!(webinar_questions -> webinars (webinar_id));
diesel::joinable!(webinar_attendance -> webinars (webinar_id));
diesel::joinable!(webinar_recordings -> webinars (webinar_id));

diesel::joinable!(quality_criteria -> quality_points (point_id));
diesel::joinable!(quality_inspections -> quality_points (point_id));
diesel::joinable!(quality_alerts -> quality_inspections (inspection_id));
diesel::joinable!(quality_alerts -> quality_points (point_id));
diesel::joinable!(quality_capas -> quality_inspections (inspection_id));
diesel::joinable!(quality_capas -> quality_alerts (alert_id));

diesel::joinable!(approval_steps -> approval_definitions (definition_id));
diesel::joinable!(approval_requests -> approval_definitions (definition_id));
diesel::joinable!(approval_decisions -> approval_requests (request_id));
diesel::joinable!(approval_escalations -> approval_requests (request_id));

diesel::joinable!(studio_system_fields -> studio_record_types (record_type));
diesel::joinable!(studio_field_definitions -> studio_record_types (record_type));
diesel::joinable!(studio_field_values -> studio_field_definitions (definition_id));

// Tables allowed to appear in the same query, grouped by join graph.
//
// One `allow_tables_to_appear_in_same_query!` invocation generates an
// `AllowTablesToAppearInSameQuery` impl for EVERY ordered pair of its
// tables, so a single list of all tables costs ~275k impls and makes
// rustc run out of memory. The groups below are the connected
// components of the `joinable!` declarations: any two tables that can
// be joined share a group, and no pair outside a group is joinable,
// so splitting loses no query. Tables with no `joinable!` edge need
// no entry at all. If a future query needs two tables from different
// groups, merge the groups (the compiler reports the missing pair
// as E0277).

// Join group 11 (9 tables, e.g. email_connections (+8 more)).
diesel::allow_tables_to_appear_in_same_query!(
    email_connections,
    email_send_log,
    email_template_versions,
    email_templates,
    newsletter_campaigns,
    newsletter_lists,
    newsletter_subscribers,
    newsletter_tracking_events,
    notification_templates,
);

// Join group 17 (7 tables, e.g. event_attendees (+6 more)).
diesel::allow_tables_to_appear_in_same_query!(
    event_attendees,
    event_occurrences,
    event_registrations,
    event_reminders,
    event_ticket_types,
    event_waitlist,
    events,
);

// Join group 20 (7 tables, e.g. webinar_attendance (+6 more)).
diesel::allow_tables_to_appear_in_same_query!(
    webinar_attendance,
    webinar_poll_votes,
    webinar_polls,
    webinar_questions,
    webinar_recordings,
    webinar_registrations,
    webinars,
);

// Join group 23 (6 tables, e.g. community_categories (+5 more)).
diesel::allow_tables_to_appear_in_same_query!(
    community_categories,
    community_flags,
    community_post_edits,
    community_posts,
    community_topics,
    community_votes,
);

// Join group 28 (5 tables, e.g. approval_decisions (+4 more)).
diesel::allow_tables_to_appear_in_same_query!(
    approval_decisions,
    approval_definitions,
    approval_escalations,
    approval_requests,
    approval_steps,
);

// Join group 29 (5 tables, e.g. document_downloads (+4 more)).
diesel::allow_tables_to_appear_in_same_query!(
    document_downloads,
    document_upload_parts,
    document_upload_sessions,
    document_versions,
    documents,
);

// Join group 33 (5 tables, e.g. quality_alerts (+4 more)).
diesel::allow_tables_to_appear_in_same_query!(
    quality_alerts,
    quality_capas,
    quality_criteria,
    quality_inspections,
    quality_points,
);

// Join group 35 (5 tables, e.g. survey_answers (+4 more)).
diesel::allow_tables_to_appear_in_same_query!(
    survey_answers,
    survey_distributions,
    survey_questions,
    survey_responses,
    surveys,
);

// Join group 37 (4 tables, e.g. contract_audit_trail (+3 more)).
diesel::allow_tables_to_appear_in_same_query!(
    contract_audit_trail,
    contract_signers,
    contract_templates,
    contracts,
);

// Join group 38 (4 tables, e.g. form_fields (+3 more)).
diesel::allow_tables_to_appear_in_same_query!(
    form_fields,
    form_submission_values,
    form_submissions,
    forms,
);

// Join group 40 (4 tables, e.g. meeting_messages (+3 more)).
diesel::allow_tables_to_appear_in_same_query!(
    meeting_messages,
    meeting_participants,
    meeting_recordings,
    meeting_rooms,
);

// Join group 42 (4 tables, e.g. studio_field_definitions (+3 more)).
diesel::allow_tables_to_appear_in_same_query!(
    studio_field_definitions,
    studio_field_values,
    studio_record_types,
    studio_system_fields,
);

// Join group 44 (3 tables, e.g. analytics_report_runs).
diesel::allow_tables_to_appear_in_same_query!(
    analytics_report_runs,
    analytics_reports,
    analytics_scheduled_reports,
);

// Join group 45 (3 tables, e.g. inbox_assignments).
diesel::allow_tables_to_appear_in_same_query!(inbox_assignments, inbox_thread_reads, inbox_threads,);

// Join group 47 (3 tables, e.g. portal_menus).
diesel::allow_tables_to_appear_in_same_query!(portal_menus, portal_views, portals,);

// Join group 49 (3 tables, e.g. workflow_run_steps).
diesel::allow_tables_to_appear_in_same_query!(workflow_run_steps, workflow_runs, workflows,);

// Join group 50 (2 tables, e.g. announcement_read_receipts).
diesel::allow_tables_to_appear_in_same_query!(announcement_read_receipts, announcements,);

// Join group 52 (2 tables, e.g. bi_report_schedules).
diesel::allow_tables_to_appear_in_same_query!(bi_report_schedules, bi_reports,);
