// Recruitment / ATS module - job postings, applicants with a validated
// hiring pipeline, interviews, per-interviewer evaluations, offers, and
// the hire handoff to HR onboarding. The full route and behaviour
// documentation lives at the top of `recruitment/module.rs`.

pub mod ai_capabilities;
mod constants;
pub mod types;
pub mod state;
mod helpers;
pub mod module;
mod handlers_jobs;
mod handlers_applicants;
mod handlers_interviews;
mod handlers_evaluations;
mod handlers_offers;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{JobStatus, JobType, Job, ApplicantStage, Applicant, ApplicantStageHistory, InterviewType, Interview, EvaluationScore, Evaluation, OfferStatus, Offer, CreateJobRequest, UpdateJobRequest, CreateApplicantRequest, UpdateApplicantRequest, MoveApplicantRequest, CreateInterviewRequest, UpdateInterviewRequest, InterviewOutcomeRequest, CreateEvaluationRequest, CreateOfferRequest, UpdateOfferRequest};
pub use self::state::{RecruitmentState};
pub use self::module::{RecruitmentModule, register};
