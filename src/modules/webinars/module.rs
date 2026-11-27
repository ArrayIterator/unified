// webinars -- Module struct, Module impl and registration.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::handlers_analytics::*;
use super::handlers_attendance::*;
use super::handlers_chat::*;
use super::handlers_config::*;
use super::handlers_followup::*;
use super::handlers_polls::*;
use super::handlers_questions::*;
use super::handlers_recordings::*;
use super::handlers_registrations::*;
use super::handlers_webinars::*;
use super::scheduler::*;
use super::state::*;
use super::ai_capabilities::AI_CAPABILITIES;

// Module

pub struct WebinarsModule {
    state: WebinarsState,
}

impl WebinarsModule {
    pub fn new() -> Self {
        Self {
            state: WebinarsState::default(),
        }
    }
}

impl Default for WebinarsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for WebinarsModule {
    fn id(&self) -> &'static str {
        "webinars"
    }

    fn name(&self) -> &'static str {
        "Webinars"
    }

    fn description(&self) -> &'static str {
        "Live webinars on top of the meetings video module: scheduled and recurring webinars, host / co-host / panelist roles, registration with approval and waitlist, live polls and Q&A, chat events, attendance tracking, recording references with replay data, post-webinar follow-up, and analytics. Video transport, payments, chat storage, survey delivery, and recording bytes stay with the meetings, billing, chat, surveys, and storage modules."
    }

    /// Webinars is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
    }

    fn requires(&self) -> &[&'static str] {
        &["meetings", "chat", "storage", "billing"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["meetings", "chat", "storage", "billing"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("webinars", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "webinar.start_requested",
            "webinars",
            Arc::new(move |ctx: &mut HookContext| on_webinar_start_requested(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "webinar.reminder_requested",
            "webinars",
            Arc::new(move |ctx: &mut HookContext| {
                on_webinar_reminder_requested(state.clone(), ctx)
            }),
        );
        for event in ["order.paid", "store.order.completed"] {
            let state = self.state.clone();
            bus.register(
                event,
                "webinars",
                Arc::new(move |ctx: &mut HookContext| on_order_paid(state.clone(), ctx)),
            );
        }
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/webinars")
                .app_data(state)
                // Static segments first: Actix matches routes in
                // registration order, so `/config` must precede
                // `/{id}` or a request for the config would be read
                // as a webinar id.
                .route("/config", web::get().to(get_config))
                .route("/config", web::patch().to(update_config))
                .route("", web::get().to(list_webinars))
                .route("", web::post().to(create_webinar))
                .route("/{id}", web::get().to(get_webinar))
                .route("/{id}", web::patch().to(update_webinar))
                .route("/{id}", web::delete().to(delete_webinar))
                .route("/{id}/publish", web::post().to(publish_webinar))
                .route("/{id}/cancel", web::post().to(cancel_webinar))
                .route("/{id}/start", web::post().to(start_webinar))
                .route("/{id}/end", web::post().to(end_webinar))
                .route("/{id}/link-meeting", web::post().to(link_meeting))
                .route("/{id}/mute", web::post().to(mute_webinar))
                .route("/{id}/live", web::get().to(get_live))
                .route("/{id}/register", web::post().to(register_for_webinar))
                .route("/{id}/registrations", web::get().to(list_registrations))
                .route("/{id}/registrations/mine", web::get().to(my_registration))
                .route(
                    "/{id}/registrations/{user_id}/approve",
                    web::post().to(approve_registration),
                )
                .route(
                    "/{id}/registrations/{user_id}/deny",
                    web::post().to(deny_registration),
                )
                .route(
                    "/{id}/registrations/{user_id}/cancel",
                    web::post().to(cancel_registration),
                )
                .route(
                    "/{id}/registrations/{user_id}/promote",
                    web::post().to(promote_registration),
                )
                .route("/{id}/join", web::post().to(join_webinar))
                .route("/{id}/leave", web::post().to(leave_webinar))
                .route("/{id}/attendance", web::get().to(list_attendance))
                .route("/{id}/polls", web::get().to(list_polls))
                .route("/{id}/polls", web::post().to(create_poll))
                .route("/{id}/polls/{poll_id}", web::get().to(get_poll))
                .route("/{id}/polls/{poll_id}/vote", web::post().to(vote_poll))
                .route("/{id}/polls/{poll_id}/close", web::post().to(close_poll))
                .route("/{id}/polls/{poll_id}", web::delete().to(delete_poll))
                .route("/{id}/questions", web::get().to(list_questions))
                .route("/{id}/questions", web::post().to(ask_question))
                .route(
                    "/{id}/questions/{question_id}/upvote",
                    web::post().to(upvote_question),
                )
                .route(
                    "/{id}/questions/{question_id}/answer",
                    web::post().to(answer_question),
                )
                .route(
                    "/{id}/questions/{question_id}",
                    web::delete().to(delete_question),
                )
                .route("/{id}/chat", web::get().to(get_chat_channel))
                .route("/{id}/chat", web::post().to(post_chat_message))
                .route("/{id}/recordings", web::get().to(list_recordings))
                .route("/{id}/recordings", web::post().to(attach_recording))
                .route(
                    "/{id}/recordings/{recording_id}/publish",
                    web::post().to(publish_recording),
                )
                .route(
                    "/{id}/recordings/{recording_id}",
                    web::delete().to(delete_recording),
                )
                .route("/{id}/replay", web::get().to(get_replay))
                .route("/{id}/follow-up", web::post().to(send_follow_up))
                .route("/{id}/analytics", web::get().to(get_analytics)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/config",
                owner: "webinars",
                description: "Read the configurable webinar defaults.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/webinars/config",
                owner: "webinars",
                description: "Update the configurable webinar defaults (admin only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars",
                owner: "webinars",
                description: "List webinars (paginated, filter by status, host_id, access_model, from, to; search title and description).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars",
                owner: "webinars",
                description: "Create a webinar (draft, or scheduled when starts_at is in the future).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}",
                owner: "webinars",
                description: "Get one webinar.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/webinars/{id}",
                owner: "webinars",
                description: "Update a webinar (host or co-host; ended or cancelled webinars cannot be updated).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/webinars/{id}",
                owner: "webinars",
                description: "Delete a webinar and its registrations, polls, questions, attendance, and recordings (host only, never while live).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/publish",
                owner: "webinars",
                description: "Publish a draft webinar: it becomes scheduled and start/reminder jobs are queued (host or co-host).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/cancel",
                owner: "webinars",
                description: "Cancel a draft or scheduled webinar and notify its registrants (host or co-host).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/start",
                owner: "webinars",
                description: "Start a scheduled webinar live; asks the linked meetings room to go live too (host or co-host).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/end",
                owner: "webinars",
                description: "End a live webinar and close open attendance sessions (host or co-host).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/link-meeting",
                owner: "webinars",
                description: "Link an existing meetings room as the webinar's live video session (host or co-host).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/mute",
                owner: "webinars",
                description: "Set audio/video mute state for one attendee or everyone (host or co-host; announced as webinar.mute_changed).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}/live",
                owner: "webinars",
                description: "Live session view: status, linked room, current attendees, mute states, open polls, unanswered questions.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/register",
                owner: "webinars",
                description: "Register for a webinar (free, paid via product entitlement, waitlist when full, pending approval when configured).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}/registrations",
                owner: "webinars",
                description: "List registrations (host or co-host; paginated, filter by status, user_id).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}/registrations/mine",
                owner: "webinars",
                description: "Get the caller's own registration for a webinar.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/registrations/{user_id}/approve",
                owner: "webinars",
                description: "Approve a pending registration (host or co-host; takes a seat or moves to the waitlist when full).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/registrations/{user_id}/deny",
                owner: "webinars",
                description: "Deny a pending registration (host or co-host).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/registrations/{user_id}/cancel",
                owner: "webinars",
                description: "Cancel a registration (the registrant or host/co-host; frees the seat and auto-promotes the waitlist).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/registrations/{user_id}/promote",
                owner: "webinars",
                description: "Promote a waitlisted registration into a free seat (host or co-host).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/join",
                owner: "webinars",
                description: "Join the live session (registered attendee or staff; paid entitlement checked, attendance recorded).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/leave",
                owner: "webinars",
                description: "Leave the live session (closes the attendance session).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}/attendance",
                owner: "webinars",
                description: "Attendance of a webinar: join/leave sessions with durations (host or co-host; paginated, filter by user_id).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}/polls",
                owner: "webinars",
                description: "List polls with live results (registered attendee or staff; paginated, filter by status).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/polls",
                owner: "webinars",
                description: "Create a live poll (host, co-host, or panelist).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}/polls/{poll_id}",
                owner: "webinars",
                description: "Get one poll with live results (registered attendee or staff).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/polls/{poll_id}/vote",
                owner: "webinars",
                description: "Vote in an open poll (registered attendee or staff; voting again replaces the previous vote).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/polls/{poll_id}/close",
                owner: "webinars",
                description: "Close a poll (host or co-host).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/webinars/{id}/polls/{poll_id}",
                owner: "webinars",
                description: "Delete a poll and its votes (host only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}/questions",
                owner: "webinars",
                description: "List Q&A questions, most upvoted first (registered attendee or staff; paginated, filter by answered).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/questions",
                owner: "webinars",
                description: "Ask a question during the live session (registered attendee or staff).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/questions/{question_id}/upvote",
                owner: "webinars",
                description: "Upvote a question (registered attendee or staff; one upvote per user, repeat upvotes are a no-op).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/questions/{question_id}/answer",
                owner: "webinars",
                description: "Mark a question answered (host, co-host, or panelist).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/webinars/{id}/questions/{question_id}",
                owner: "webinars",
                description: "Delete a question (host or its author).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}/chat",
                owner: "webinars",
                description: "Chat channel descriptor for a webinar (messages live in the chat module).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/chat",
                owner: "webinars",
                description: "Post a live chat message (registered attendee or staff; emitted as webinar.chat.message for the chat module).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}/recordings",
                owner: "webinars",
                description: "List recording references of a webinar (paginated, filter by status).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/recordings",
                owner: "webinars",
                description: "Attach a recording reference (stored object in the storage module) to a webinar (host or co-host).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/recordings/{recording_id}/publish",
                owner: "webinars",
                description: "Mark a recording ready for replay (host or co-host).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/webinars/{id}/recordings/{recording_id}",
                owner: "webinars",
                description: "Delete a recording reference (host only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}/replay",
                owner: "webinars",
                description: "Replay page data: webinar plus its ready recordings (ended webinars; host, staff, or registered attendees).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/webinars/{id}/follow-up",
                owner: "webinars",
                description: "Send post-webinar follow-up: recording link notifications plus a survey invitation (host or co-host; ended webinars only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/webinars/{id}/analytics",
                owner: "webinars",
                description: "Analytics for a webinar: registrations, attendance rate, drop-off buckets, poll and Q&A participation (host or co-host).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(WebinarsModule::new()));
}
