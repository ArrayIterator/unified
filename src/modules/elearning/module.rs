// Elearning - Module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Module

pub struct ElearningModule {
    state: ElearningState,
}

impl ElearningModule {
    pub fn new() -> Self {
        Self {
            state: ElearningState::default(),
        }
    }
}

impl Default for ElearningModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ElearningModule {
    fn id(&self) -> &'static str {
        "elearning"
    }

    fn name(&self) -> &'static str {
        "E-Learning"
    }

    fn description(&self) -> &'static str {
        "E-learning: courses with modules and drip lessons (video, text, file, quiz), free and paid enrollment through store/billing events, multiple-choice quizzes with pass marks, attempt limits, auto-grading and review, per-lesson and per-course progress, verifiable certificates with tamper-evidence anchoring events, instructor ownership, course reviews, and ordered learning paths. All lists, levels, categories, languages, and defaults are admin-configurable settings, never hard-coded."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["store", "storage", "billing", "roles"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["store", "storage", "billing", "roles"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        ai::register_ai_capabilities("elearning", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "elearning.course.requested",
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_course_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            ORDER_PAID,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_paid(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_COMPLETED,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_paid(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_REFUNDED,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_refunded_or_cancelled(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_CANCELLED,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_refunded_or_cancelled(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            ANCHOR_CONFIRMED,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_anchor_outcome(state.clone(), ctx, true)
            }),
        );
        let state = self.state.clone();
        bus.register(
            ANCHOR_FAILED,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_anchor_outcome(state.clone(), ctx, false)
            }),
        );
        // Ground the quiz / lesson AI capabilities with the lesson
        // content (and, for quiz drafts, the existing questions).
        // No learner data is ever grounded: no enrollments,
        // attempts, or scores. Tenant scoping: this store is
        // in-memory with no tenant partition yet; when batch 17
        // persistence lands, these handlers must only ground
        // lessons of the calling tenant, keyed by the caller
        // company id in the payload.
        let state = self.state.clone();
        bus.register(
            "elearning.ai.draft_quiz_requested",
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                let lesson_id = ctx
                    .get("input")
                    .and_then(|input| input.get("lesson_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.add_result(quiz_grounding(&state, lesson_id));
            }),
        );
        let state = self.state.clone();
        bus.register(
            "elearning.ai.summarize_lesson_requested",
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                let lesson_id = ctx
                    .get("input")
                    .and_then(|input| input.get("lesson_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.add_result(lesson_grounding(&state, lesson_id));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/elearning")
                .app_data(state)
                .route("/courses", web::get().to(list_courses))
                .route("/courses", web::post().to(create_course))
                .route("/courses/{id}", web::get().to(get_course))
                .route("/courses/{id}", web::patch().to(update_course))
                .route("/courses/{id}", web::delete().to(delete_course))
                .route("/courses/{id}/publish", web::post().to(publish_course))
                .route("/courses/{id}/archive", web::post().to(archive_course))
                .route("/courses/{id}/modules", web::get().to(list_course_modules))
                .route("/courses/{id}/modules", web::post().to(create_course_module))
                .route("/modules/{id}", web::get().to(get_module))
                .route("/modules/{id}", web::patch().to(update_module))
                .route("/modules/{id}", web::delete().to(delete_module))
                .route("/modules/{id}/lessons", web::get().to(list_module_lessons))
                .route("/modules/{id}/lessons", web::post().to(create_lesson))
                .route("/lessons/{id}", web::get().to(get_lesson))
                .route("/lessons/{id}", web::patch().to(update_lesson))
                .route("/lessons/{id}", web::delete().to(delete_lesson))
                .route("/courses/{id}/enroll", web::post().to(enroll_course))
                .route("/enrollments", web::get().to(list_enrollments))
                .route("/enrollments/{id}", web::get().to(get_enrollment))
                .route("/enrollments/{id}/cancel", web::post().to(cancel_enrollment))
                .route("/my/enrollments", web::get().to(list_my_enrollments))
                .route("/lessons/{id}/complete", web::post().to(complete_lesson))
                .route("/courses/{id}/progress", web::get().to(get_course_progress))
                .route(
                    "/courses/{id}/progress/{user_id}",
                    web::get().to(get_user_course_progress),
                )
                .route("/my/progress", web::get().to(list_my_progress))
                .route("/lessons/{id}/quiz", web::get().to(get_lesson_quiz))
                .route("/lessons/{id}/quiz", web::post().to(create_lesson_quiz))
                .route("/lessons/{id}/quiz", web::patch().to(update_lesson_quiz))
                .route(
                    "/lessons/{id}/quiz/questions",
                    web::get().to(list_quiz_questions),
                )
                .route(
                    "/lessons/{id}/quiz/questions",
                    web::post().to(create_quiz_question),
                )
                .route("/questions/{id}", web::patch().to(update_question))
                .route("/questions/{id}", web::delete().to(delete_question))
                .route(
                    "/lessons/{id}/quiz/attempts",
                    web::post().to(submit_quiz_attempt),
                )
                .route("/quiz-attempts", web::get().to(list_quiz_attempts))
                .route("/quiz-attempts/{id}", web::get().to(get_quiz_attempt))
                .route("/certificates", web::get().to(list_certificates))
                .route("/certificates/verify/{code}", web::get().to(verify_certificate))
                .route("/certificates/{id}", web::get().to(get_certificate))
                .route(
                    "/certificates/{id}/revoke",
                    web::post().to(revoke_certificate),
                )
                .route(
                    "/certificates/{id}/anchor",
                    web::post().to(anchor_certificate),
                )
                .route("/courses/{id}/reviews", web::get().to(list_course_reviews))
                .route("/courses/{id}/reviews", web::post().to(create_course_review))
                .route("/reviews/{id}", web::get().to(get_review))
                .route("/reviews/{id}", web::patch().to(update_review))
                .route("/reviews/{id}", web::delete().to(delete_review))
                .route("/learning-paths", web::get().to(list_paths))
                .route("/learning-paths", web::post().to(create_path))
                .route("/learning-paths/{id}", web::get().to(get_path))
                .route("/learning-paths/{id}", web::patch().to(update_path))
                .route("/learning-paths/{id}", web::delete().to(delete_path))
                .route(
                    "/learning-paths/{id}/courses",
                    web::post().to(add_path_course),
                )
                .route(
                    "/learning-paths/{id}/courses/{course_id}",
                    web::delete().to(remove_path_course),
                )
                .route(
                    "/learning-paths/{id}/progress",
                    web::get().to(get_path_progress),
                )
                .route("/my/learning-paths", web::get().to(list_my_paths))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses", owner: "elearning", description: "List courses (paginated, filter by category, level, language, status, instructor_id; learners see published only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses", owner: "elearning", description: "Create a course in draft (manager or instructor; category, level, language validated against settings).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses/{id}", owner: "elearning", description: "Get one course (drafts only for managers and the instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/courses/{id}", owner: "elearning", description: "Update a course (manager or its instructor; price locked while a paid enrollment is pending).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/courses/{id}", owner: "elearning", description: "Delete a course with no enrollments, with its modules, lessons, quizzes, and reviews (manager or its instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses/{id}/publish", owner: "elearning", description: "Publish a draft course (manager or its instructor; requires at least one lesson).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses/{id}/archive", owner: "elearning", description: "Archive a published or draft course (manager or its instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses/{id}/modules", owner: "elearning", description: "List the modules of a course (paginated, ordered by position).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses/{id}/modules", owner: "elearning", description: "Add a module to a course (manager or its instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/modules/{id}", owner: "elearning", description: "Get one course module.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/modules/{id}", owner: "elearning", description: "Update a course module (manager or its instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/modules/{id}", owner: "elearning", description: "Delete a module with its lessons, quizzes, attempts, and progress (manager or its instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/modules/{id}/lessons", owner: "elearning", description: "List the lessons of a module (paginated, filter by lesson_type, ordered by position).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/modules/{id}/lessons", owner: "elearning", description: "Add a lesson (video/file need a storage reference, text needs a body; drip by date or previous lesson).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/lessons/{id}", owner: "elearning", description: "Get one lesson.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/lessons/{id}", owner: "elearning", description: "Update a lesson (manager or its instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/lessons/{id}", owner: "elearning", description: "Delete a lesson with its quiz, questions, attempts, and progress (manager or its instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses/{id}/enroll", owner: "elearning", description: "Enroll in a published course (free: active at once; paid: pending_payment plus a purchase event for store/billing).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/enrollments", owner: "elearning", description: "List enrollments (own only unless manager; filter by course_id, user_id, status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/enrollments/{id}", owner: "elearning", description: "Get one enrollment (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/enrollments/{id}/cancel", owner: "elearning", description: "Cancel an enrollment (own or manager; completed enrollments cannot be cancelled).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/my/enrollments", owner: "elearning", description: "The caller's enrollments (paginated, filter by status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/lessons/{id}/complete", owner: "elearning", description: "Mark a lesson complete (enrolled and active only; drip rules apply; quiz lessons need a passed attempt).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses/{id}/progress", owner: "elearning", description: "The caller's progress in a course (required lessons, percent, completion).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses/{id}/progress/{user_id}", owner: "elearning", description: "One learner's progress in a course (own, instructor, or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/my/progress", owner: "elearning", description: "The caller's progress across enrolled courses (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/lessons/{id}/quiz", owner: "elearning", description: "Get the quiz of a lesson.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/lessons/{id}/quiz", owner: "elearning", description: "Define the quiz of a lesson (manager or its instructor; pass mark and attempt limit default from settings).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/lessons/{id}/quiz", owner: "elearning", description: "Update a lesson quiz (manager or its instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/lessons/{id}/quiz/questions", owner: "elearning", description: "List quiz questions (paginated; correct answers hidden from learners).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/lessons/{id}/quiz/questions", owner: "elearning", description: "Add a multiple-choice question (manager or its instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/questions/{id}", owner: "elearning", description: "Update a quiz question (manager or its instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/questions/{id}", owner: "elearning", description: "Delete a quiz question (manager or its instructor; blocked once attempts exist).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/lessons/{id}/quiz/attempts", owner: "elearning", description: "Submit a quiz attempt (auto-graded; attempt limit enforced; review only when the quiz allows it).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/quiz-attempts", owner: "elearning", description: "List quiz attempts (own only unless manager; filter by quiz_id, user_id, passed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/quiz-attempts/{id}", owner: "elearning", description: "Get one quiz attempt with review when allowed (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/certificates", owner: "elearning", description: "List certificates (own only unless manager; filter by course_id, user_id, status, anchor_status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/certificates/verify/{code}", owner: "elearning", description: "PUBLIC certificate verification by code (no authentication; revoked or unknown codes answer 404).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/certificates/{id}", owner: "elearning", description: "Get one certificate (own only unless manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/certificates/{id}/revoke", owner: "elearning", description: "Revoke a certificate (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/certificates/{id}/anchor", owner: "elearning", description: "Request tamper-evidence anchoring for a certificate through the Web3 flow (manager; events only).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses/{id}/reviews", owner: "elearning", description: "List course reviews with a rating summary (paginated, filter by rating).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses/{id}/reviews", owner: "elearning", description: "Leave a course review (enrolled learners only, one per user, rating 1-5).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/reviews/{id}", owner: "elearning", description: "Get one review.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/reviews/{id}", owner: "elearning", description: "Edit a review (author or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/reviews/{id}", owner: "elearning", description: "Delete a review (author or manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/learning-paths", owner: "elearning", description: "List learning paths (paginated, searchable).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/learning-paths", owner: "elearning", description: "Create a learning path (manager or instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/learning-paths/{id}", owner: "elearning", description: "Get one learning path with its ordered courses.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/learning-paths/{id}", owner: "elearning", description: "Update a learning path (manager or instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/learning-paths/{id}", owner: "elearning", description: "Delete a learning path with its course links (manager or instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/learning-paths/{id}/courses", owner: "elearning", description: "Append a course to a learning path (manager or instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/learning-paths/{id}/courses/{course_id}", owner: "elearning", description: "Remove a course from a learning path (manager or instructor).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/learning-paths/{id}/progress", owner: "elearning", description: "The caller's progress in a learning path (per-course progress, percent, completion).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/my/learning-paths", owner: "elearning", description: "Learning paths the caller is progressing in (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/elearning/settings", owner: "elearning", description: "Get the configurable e-learning settings (levels, categories, languages, lesson types, defaults).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/elearning/settings", owner: "elearning", description: "Update the configurable e-learning settings (manager).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/elearning/ai/{capability}", owner: "elearning", description: "Run one registered e-learning AI capability (draft_quiz, summarize_lesson; requires ai:use plus elearning.view).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ElearningModule::new()));
}
