// Storage Manager module — the storage hub every other module uses.
//
// Split into submodules; the full module documentation lives in `module.rs`.

pub mod ai_capabilities;
mod constants;
pub mod events;
mod crypto;
pub mod types;
pub mod state;
mod helpers;
mod validation;
mod requests;
mod handlers_status;
mod handlers_buckets;
mod handlers_health;
mod handlers_objects;
mod handlers_signed_urls;
mod handlers_bindings;
mod handlers_quotas;
mod handlers_stats;
mod handlers_lifecycle;
mod hooks;
pub mod plans_constants;
mod plans_helpers;
mod plans_requests;
mod handlers_plans;
mod byo_helpers;
mod byo_requests;
mod handlers_byo_settings;
mod handlers_byo_terms;
mod handlers_byo_connections;
mod handlers_byo_health;
mod handlers_byo_objects;
pub mod module;

pub(crate) use self::constants::{MAX_UPLOAD_BYTES, DEFAULT_SIGNED_URL_SECS, MAX_SIGNED_URL_SECS,
    BYO_MAX_VALIDATION_AGE_SECS, DEV_ENCRYPTION_SECRET, PERM_READ, PERM_MANAGE_BUCKETS, PERM_MANAGE_BINDINGS, PERM_MANAGE_QUOTAS,
    PERM_BROWSE, PERM_UPLOAD, PERM_MANAGE_LIFECYCLE, PERM_MANAGE_SIGNED_URLS, PERM_HEALTH,
    PERM_MANAGE_PLANS, PERM_MANAGE_BYO, PERM_BYO_USE};
pub(crate) use self::crypto::{HmacSha256, hmac_sha256, sha256_hex_of,
    encryption_secret_from, encryption_secret_for_encrypt, keystream, constant_time_eq,
    encrypt_secret_value, decrypt_secret_value, secret_fingerprint, ciphertext_fingerprint};
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::{StorageBucket, BucketView, StorageBinding, StorageQuota, UsageDaily,
    LifecycleRule, SignedUrlRecord, SignedUrlView, HealthCheck, StoredObject, ObjectView};
pub(crate) use self::types::{encryption_note};
pub use self::state::{StorageManagerState};
pub(crate) use self::helpers::{
    Caller, authorize, bucket_company_visible, caller, caller_company, check_bucket_access,
    check_capability, check_object_access, created, err_response, in_company_scope,
    is_platform_admin, ok, paginate, quota_in_scope, require_company_match_for, require_permission,
    require_quota_in_scope, require_record_in_company_scope,
    require_record_writable_in_company_scope, require_tenant, resolve_upload_company,
};
pub(crate) use self::validation::{require_text, optional_text, parse_uuid, valid_scope_id,
    valid_module_id, valid_prefix, valid_quota_limit, object_key, quota_key, load_bucket,
    load_binding, load_quota, load_rule, target_config_for_bucket, target_config_for_bucket_with,
    provider_for_bucket, provider_for_bucket_with, operator_allowed_hosts,
    validate_requested_allowed_hosts, validate_endpoint_dns, resolve_binding, QuotaCheck,
    active_plan_for_buyer, check_plan_limits, check_quotas, apply_quota_delta, reserve_quota,
    release_quota, release_plan_reservation, record_stored, record_deleted, upload_delta};
pub(crate) use self::requests::{CreateBucketRequest, UpdateBucketRequest, CreateBindingRequest,
    UpdateBindingRequest, CreateQuotaRequest, UpdateQuotaRequest, CreateLifecycleRequest,
    UpdateLifecycleRequest, RunLifecycleRequest, IssueSignedUrlRequest, MoveObjectRequest,
    ObjectQuery, UploadQuery, SetDefaultRequest, RevokeSignedUrlRequest};
pub(crate) use self::handlers_status::{get_status, get_defaults, set_default};
pub(crate) use self::handlers_buckets::{encrypt_credentials, validate_bucket_fields, list_buckets,
    create_bucket, get_bucket, update_bucket, delete_bucket, test_bucket};
pub(crate) use self::handlers_health::{run_health_check, health_bucket, list_health,
    list_bucket_health};
pub(crate) use self::handlers_objects::{list_objects, upload_object, download_object, delete_object,
    verify_object, default_true, OrphanCleanupRequest, cleanup_orphans, move_object,
    move_destination_check, move_quota_delta};
pub(crate) use self::handlers_signed_urls::{issue_signed_url, list_signed_urls, revoke_signed_url,
    redeem_download, signed_url_token_hash, signed_url_token_matches, is_redeemable};
pub(crate) use self::handlers_bindings::{list_bindings, create_binding, get_binding, update_binding,
    delete_binding, resolve_binding_endpoint};
pub(crate) use self::handlers_quotas::{list_quotas, create_quota, get_quota, update_quota,
    delete_quota};
pub(crate) use self::handlers_stats::{get_stats, get_stats_daily, get_stats_top};
pub(crate) use self::handlers_lifecycle::{list_lifecycle, create_lifecycle, get_lifecycle,
    update_lifecycle, delete_lifecycle, run_one_rule, run_lifecycle, validate_retention_days};
pub(crate) use self::hooks::{on_upload_requested, on_lifecycle_run_requested};
pub(crate) use self::plans_constants::{BYO_KIND_NAMES, BILLING_CYCLES, STORAGE_MODULE_ID,
    canonical_provider_kind_name, storage_kind_for_name, validate_provider_kind_list,
    validate_billing_cycle};
pub use self::plans_constants::{StoragePlan, PlanView, PlanAssignment, MeteringReport, ByoSettings,
    ByoConnection, ByoConnectionView, ByoObject, ByoTerms, ByoTermsAcceptance, ByoHealthCheck};
pub(crate) use self::plans_helpers::{load_plan, assignment_key, load_assignment,
    active_assignment_for_buyer, resolve_my_plan_company, period_usage_from_daily, buyer_usage,
    emit_metering, require_storage_module_entitlement, check_plan_product_entitlement,
    check_plan_change_quota};
pub(crate) use self::plans_requests::{CreatePlanRequest, UpdatePlanRequest, AssignPlanRequest,
    CancelAssignmentRequest, ReportMeteringRequest};
pub(crate) use self::handlers_plans::{list_plans, list_public_plans, create_plan, get_plan,
    update_plan, delete_plan, assign_plan, list_plan_assignments, get_plan_assignment,
    cancel_plan_assignment, get_my_plan, report_metering, list_metering};
pub(crate) use self::byo_helpers::{load_byo_connection, load_byo_connection_for, load_byo_terms,
    current_byo_terms, has_accepted_terms, validate_extensions, validate_mime_types,
    target_config_for_byo, provider_for_byo, provider_for_byo_probe, run_byo_validation,
    run_byo_health_check, byo_full_key, check_byo_operation, byo_object_key,
    validation_is_fresh};
pub(crate) use self::byo_requests::{UpdateByoSettingsRequest, CreateByoTermsRequest,
    UpdateByoTermsRequest, CreateByoConnectionRequest, UpdateByoConnectionRequest, ByoStatusRequest,
    AcceptTermsRequest, RunByoHealthRequest, ByoUploadQuery};
pub(crate) use self::handlers_byo_settings::{get_byo_settings, update_byo_settings};
pub(crate) use self::handlers_byo_terms::{list_byo_terms, get_current_byo_terms, create_byo_terms,
    get_byo_terms, update_byo_terms, delete_byo_terms, accept_byo_terms, enforce_byo_reacceptance};
pub(crate) use self::handlers_byo_connections::{list_byo_connections, create_byo_connection,
    get_byo_connection, update_byo_connection, delete_byo_connection, validate_byo_connection,
    activate_byo_connection, set_byo_status, suspend_byo_connection, revoke_byo_connection};
pub(crate) use self::handlers_byo_health::{health_byo_connection, list_byo_health,
    list_byo_connection_health, run_byo_health, on_byo_health_check_requested};
pub(crate) use self::handlers_byo_objects::{emit_byo_metering, list_byo_objects, upload_byo_object,
    download_byo_object, delete_byo_object};
pub use self::module::{StorageManagerModule, register};
