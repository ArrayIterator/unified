// Marketing Automation module.

// Split into per-concern files under `src/modules/marketing/`; the full
// module documentation is preserved at the top of `module.rs`.

pub mod ai_capabilities;
pub mod constants;
pub mod types;
pub mod store;
mod helpers;
mod validation;
mod service;
mod service_sequences;
mod handlers_campaigns;
mod handlers_segments;
mod handlers_sequences;
mod handlers_scores;
mod handlers_audience;
mod hooks;
pub mod module;

// Re-exported from `constants`.
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::EVENT_MAIL_SEND_REQUESTED;
pub use self::constants::EVENT_MAIL_SENT;
pub use self::constants::EVENT_MAIL_FAILED;
pub use self::constants::EVENT_SCHEDULER_ENQUEUE;
pub use self::constants::EVENT_NEWSLETTER_OPEN;
pub use self::constants::EVENT_NEWSLETTER_CLICK;
pub use self::constants::EVENT_NEWSLETTER_UNSUBSCRIBED;
pub use self::constants::EVENT_COMPLIANCE_CONSENT_GRANTED;
pub use self::constants::EVENT_COMPLIANCE_CONSENT_WITHDRAWN;
pub use self::constants::EVENT_COMPLIANCE_CONSENT_RECORDED;
pub use self::constants::EVENT_COMPLIANCE_CONSENT_CHECK;
pub use self::constants::EVENT_PRIVACY_CONSENT_CHANGED;
pub use self::constants::EVENT_FORMS_SUBMISSION_RECEIVED;
pub use self::constants::EVENT_STORE_ORDER_COMPLETED;
pub use self::constants::EVENT_ROLE_CHECK;
pub use self::constants::EVENT_MARKETING_CAMPAIGN_CREATED;
pub use self::constants::EVENT_MARKETING_CAMPAIGN_SENT;
pub use self::constants::EVENT_MARKETING_CAMPAIGN_AB_WINNER;
pub use self::constants::EVENT_MARKETING_CAMPAIGN_SEND;
pub use self::constants::EVENT_MARKETING_SEGMENT_EVALUATED;
pub use self::constants::EVENT_MARKETING_SEQUENCE_ENROLLED;
pub use self::constants::EVENT_MARKETING_SEQUENCE_STEP_SENT;
pub use self::constants::EVENT_MARKETING_SEQUENCE_COMPLETED;
pub use self::constants::EVENT_MARKETING_SEQUENCE_PROCESS_DUE;
pub use self::constants::EVENT_MARKETING_SEQUENCE_ENROLL;
pub use self::constants::EVENT_MARKETING_SCORE_UPDATED;
pub use self::constants::EVENT_MARKETING_LEAD_HOT;
pub use self::constants::EVENT_MARKETING_SCORE_RECORD;
pub use self::constants::EVENT_MARKETING_SEND_SKIPPED;
pub use self::constants::EVENT_MARKETING_UNSUBSCRIBED;
pub use self::constants::EVENT_MARKETING_UNSUBSCRIBE;
pub use self::constants::EVENT_MARKETING_CONTACTS_SYNC;

// Re-exported from `types`.
pub use self::types::CampaignStats;
pub use self::types::CampaignStatsView;
pub use self::types::MarketingCampaign;
pub use self::types::CampaignVariant;
pub use self::types::Segment;
pub use self::types::SegmentSizeSnapshot;
pub use self::types::Sequence;
pub use self::types::SequenceStep;
pub use self::types::EnrollmentStepEvent;
pub use self::types::Enrollment;
pub use self::types::ScoreEvent;
pub use self::types::ContactScore;
pub use self::types::ScoreRule;
pub use self::types::MarketingSend;
pub use self::types::AudienceContact;
pub use self::types::Suppression;
pub use self::types::ConsentCacheEntry;
pub use self::types::TrackingEvent;
pub use self::types::MarketingSettings;

// Re-exported from `store`.
pub use self::store::MarketingState;

// Re-exported from `module`.
pub use self::module::MarketingModule;
pub use self::module::register;
