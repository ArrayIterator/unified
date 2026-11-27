//! Module registration: routes, hooks and metadata.

use super::ai_capabilities::AI_CAPABILITIES;
use super::handlers_ai::ai_capability;
use super::handlers_audit::list_audit;
use super::handlers_gaming::{
    create_anti_gaming_rule, delete_anti_gaming_rule, get_anti_gaming_rule, list_anti_gaming_rules,
    update_anti_gaming_rule,
};
use super::handlers_jobs::{get_eligibility, list_eligibility, set_eligibility};
use super::handlers_links::{
    click_link, create_link, get_link, list_links, revoke_link, rotate_link,
};
use super::handlers_referrals::{
    create_referral, delete_referral, get_referral, list_referral_history, list_referrals,
    transition_referral, update_referral,
};
use super::handlers_rewards::{get_reward, list_rewards, mark_reward_paid, void_reward};
use super::handlers_rules::{
    create_reward_rule, delete_reward_rule, get_reward_rule, list_reward_rules, update_reward_rule,
};
use super::handlers_stages::{create_stage, delete_stage, get_stage, list_stages, update_stage};
use super::hooks::{
    on_hr_employee_created, on_recruitment_applicant_moved, on_recruitment_hired, on_user_deleted,
};
use super::state::ReferralState;
use super::types::{
    EVENT_APPLICANT_MOVED, EVENT_HR_EMPLOYEE_CREATED, EVENT_RECRUITMENT_HIRED, EVENT_USER_DELETED,
};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};

// Module

pub struct ReferralModule {
    pub(crate) state: ReferralState,
}

impl ReferralModule {
    pub fn new() -> Self {
        Self {
            state: ReferralState::default(),
        }
    }
}

impl Default for ReferralModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ReferralModule {
    fn id(&self) -> &'static str {
        "referral"
    }

    fn name(&self) -> &'static str {
        "Employee Referral"
    }

    fn description(&self) -> &'static str {
        "Employee referral: recruitment jobs marked referral-eligible with a per-job reward rule override or the tenant default rule, unique token-based shareable referral links per (employee, job) with click counts only, referrals with one entry per (candidate email, job), a fully data-driven pipeline (admin-defined stages with allowed transitions, audited moves), reward rules as data (amount in integer minor units, payout trigger stage, retention days, eligibility conditions) with rewards that become payable through events for billing / HR to pay out, anti-gaming rules as data (self-referral, same-candidate cooldown, per-period cap, active-employee check), and an append-only audit trail."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "recruitment", "hr"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "recruitment", "hr"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("referral", AI_CAPABILITIES);
        *self.state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVENT_USER_DELETED,
            "referral",
            Arc::new(move |ctx: &mut HookContext| on_user_deleted(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_HR_EMPLOYEE_CREATED,
            "referral",
            Arc::new(move |ctx: &mut HookContext| on_hr_employee_created(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_APPLICANT_MOVED,
            "referral",
            Arc::new(move |ctx: &mut HookContext| {
                on_recruitment_applicant_moved(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVENT_RECRUITMENT_HIRED,
            "referral",
            Arc::new(move |ctx: &mut HookContext| on_recruitment_hired(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/referral")
                .app_data(state)
                .route("/public/links/{token}", web::get().to(click_link))
                .route("/jobs/eligibility", web::get().to(list_eligibility))
                .route("/jobs/{job_id}/eligibility", web::get().to(get_eligibility))
                .route("/jobs/{job_id}/eligibility", web::put().to(set_eligibility))
                .route("/links", web::get().to(list_links))
                .route("/links", web::post().to(create_link))
                .route("/links/{link_id}", web::get().to(get_link))
                .route("/links/{link_id}", web::delete().to(revoke_link))
                .route("/links/{link_id}/rotate", web::post().to(rotate_link))
                .route("/referrals", web::get().to(list_referrals))
                .route("/referrals", web::post().to(create_referral))
                .route("/referrals/{referral_id}", web::get().to(get_referral))
                .route("/referrals/{referral_id}", web::patch().to(update_referral))
                .route(
                    "/referrals/{referral_id}",
                    web::delete().to(delete_referral),
                )
                .route(
                    "/referrals/{referral_id}/stage",
                    web::post().to(transition_referral),
                )
                .route(
                    "/referrals/{referral_id}/history",
                    web::get().to(list_referral_history),
                )
                .route("/rewards", web::get().to(list_rewards))
                .route("/rewards/{reward_id}", web::get().to(get_reward))
                .route(
                    "/rewards/{reward_id}/mark-paid",
                    web::post().to(mark_reward_paid),
                )
                .route("/rewards/{reward_id}/void", web::post().to(void_reward))
                .route("/rules", web::get().to(list_reward_rules))
                .route("/rules", web::post().to(create_reward_rule))
                .route("/rules/{rule_id}", web::get().to(get_reward_rule))
                .route("/rules/{rule_id}", web::patch().to(update_reward_rule))
                .route("/rules/{rule_id}", web::delete().to(delete_reward_rule))
                .route("/stages", web::get().to(list_stages))
                .route("/stages", web::post().to(create_stage))
                .route("/stages/{stage_id}", web::get().to(get_stage))
                .route("/stages/{stage_id}", web::patch().to(update_stage))
                .route("/stages/{stage_id}", web::delete().to(delete_stage))
                .route("/anti-gaming-rules", web::get().to(list_anti_gaming_rules))
                .route(
                    "/anti-gaming-rules",
                    web::post().to(create_anti_gaming_rule),
                )
                .route(
                    "/anti-gaming-rules/{rule_id}",
                    web::get().to(get_anti_gaming_rule),
                )
                .route(
                    "/anti-gaming-rules/{rule_id}",
                    web::patch().to(update_anti_gaming_rule),
                )
                .route(
                    "/anti-gaming-rules/{rule_id}",
                    web::delete().to(delete_anti_gaming_rule),
                )
                .route("/audit", web::get().to(list_audit))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let auth = unified_core::auth::RouteAuth::Authenticated;
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/public/links/{token}",
                owner: "referral",
                description: "PUBLIC (token-gated): follow a referral link - increments the click count only, no IP or fingerprint stored; returns the job id.",
                auth: unified_core::auth::RouteAuth::Public,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/jobs/eligibility",
                owner: "referral",
                description: "List job eligibility rows (paginated, filter by job, eligible). Jobs with no row are eligible by default.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/jobs/{job_id}/eligibility",
                owner: "referral",
                description: "Get the eligibility row of one job.",
                auth,
            },
            RouteInfo {
                method: "PUT",
                path: "/api/v1/referral/jobs/{job_id}/eligibility",
                owner: "referral",
                description: "Set a job referral-eligible or not, with an optional per-job reward rule override (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/links",
                owner: "referral",
                description: "List referral links (paginated, filter by job, referrer, active).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/referral/links",
                owner: "referral",
                description: "Create a referral link for the caller and one job; the raw token is returned once, only its hash is stored.",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/links/{link_id}",
                owner: "referral",
                description: "Get one referral link (token hash never exposed).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/referral/links/{link_id}",
                owner: "referral",
                description: "Revoke a referral link; the row and its click count are kept (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/referral/links/{link_id}/rotate",
                owner: "referral",
                description: "Rotate a referral link: the old token stops working, a fresh token is returned once (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/referrals",
                owner: "referral",
                description: "List referrals (paginated, filter by job, referrer, stage, source; search by candidate name/email).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/referral/referrals",
                owner: "referral",
                description: "Submit a referral for a job (one per candidate email per job; anti-gaming rules evaluated; duplicate or blocked submissions return 409).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/referrals/{referral_id}",
                owner: "referral",
                description: "Get one referral with its stage name.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/referral/referrals/{referral_id}",
                owner: "referral",
                description: "Update a referral's candidate name, phone, or notes (manager; email, job, referrer, and stage never change here).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/referral/referrals/{referral_id}",
                owner: "referral",
                description: "Delete a referral; refused when a reward was already paid (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/referral/referrals/{referral_id}/stage",
                owner: "referral",
                description: "Move a referral to another pipeline stage (validated against the stage data, audited; entering the hired stage creates the reward, entering the rejected stage voids open rewards) (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/referrals/{referral_id}/history",
                owner: "referral",
                description: "List the stage history of one referral, oldest first (paginated).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/rewards",
                owner: "referral",
                description: "List rewards (paginated, filter by referral, job, referrer, rule, status).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/rewards/{reward_id}",
                owner: "referral",
                description: "Get one reward.",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/referral/rewards/{reward_id}/mark-paid",
                owner: "referral",
                description: "Mark a reward paid: records that billing / HR paid it out elsewhere; moves no money (manager).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/referral/rewards/{reward_id}/void",
                owner: "referral",
                description: "Void a pending or eligible reward (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/rules",
                owner: "referral",
                description: "List reward rules (paginated, filter by active, default; search by name/description).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/referral/rules",
                owner: "referral",
                description: "Create a reward rule with amount in minor units, trigger stage, retention days, and conditions (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/rules/{rule_id}",
                owner: "referral",
                description: "Get one reward rule.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/referral/rules/{rule_id}",
                owner: "referral",
                description: "Update a reward rule; rewards already created from it keep their snapshot (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/referral/rules/{rule_id}",
                owner: "referral",
                description: "Delete a reward rule; its rewards are kept and jobs fall back to the default rule (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/stages",
                owner: "referral",
                description: "List pipeline stages in pipeline order (paginated, filter by active, terminal, hired, rejected; search by name/slug).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/referral/stages",
                owner: "referral",
                description: "Create a pipeline stage with slug, order, flags, and allowed next stages (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/stages/{stage_id}",
                owner: "referral",
                description: "Get one pipeline stage.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/referral/stages/{stage_id}",
                owner: "referral",
                description: "Update a pipeline stage (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/referral/stages/{stage_id}",
                owner: "referral",
                description: "Delete a pipeline stage; refused while it still holds referrals (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/anti-gaming-rules",
                owner: "referral",
                description: "List anti-gaming rules (paginated, filter by kind, enabled, enforcement).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/referral/anti-gaming-rules",
                owner: "referral",
                description: "Create an anti-gaming rule with its kind, thresholds, and block-or-flag enforcement (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/anti-gaming-rules/{rule_id}",
                owner: "referral",
                description: "Get one anti-gaming rule.",
                auth,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/referral/anti-gaming-rules/{rule_id}",
                owner: "referral",
                description: "Update an anti-gaming rule (manager).",
                auth,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/referral/anti-gaming-rules/{rule_id}",
                owner: "referral",
                description: "Delete an anti-gaming rule (manager).",
                auth,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/referral/audit",
                owner: "referral",
                description: "List the audit trail (paginated, filter by entity type, entity, action, actor; newest first).",
                auth,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/referral/ai/{capability}",
                owner: "referral",
                description: "Run one registered referral AI capability through the AI module's gated runner.",
                auth,
            },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ReferralModule::new()));
}
