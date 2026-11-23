// Backup Manager module.
//
// Backup jobs that capture the database, files, configuration, or all of
// them, store the resulting archive in a configured storage target, and
// support verification, retention pruning, and restore.
//
// Routes under `/api/v1/backup`:
//
//   GET    /                        module summary (counts, targets, limits)
//   GET    /targets                 storage target names from the registry
//   GET    /jobs                    list jobs (pagination, kind/status/target)
//   POST   /jobs                    create a job
//   GET    /jobs/{id}               get one job
//   PUT    /jobs/{id}               update a job
//   DELETE /jobs/{id}               delete a job and its stored archives
//   POST   /jobs/{id}/run           run a job now (202, runs in background)
//   POST   /jobs/{id}/verify        verify the latest completed run
//   POST   /jobs/{id}/restore       restore from the latest completed run
//   GET    /jobs/{id}/runs          run history for one job
//   GET    /runs                    list all runs (pagination, filters)
//   GET    /runs/{run_id}           get one run
//   POST   /runs/{run_id}/verify    recompute and compare the checksum
//   POST   /runs/{run_id}/restore   restore from one run
//   POST   /runs/{run_id}/drill     restore drill: verify the archive and
//                                   extract it to a throwaway sandbox
//                                   directory, then delete the sandbox.
//                                   Live data is never touched.
//   POST   /jobs/{id}/drill         drill the latest completed run
//   DELETE /runs/{run_id}           delete one run and its stored archive
//   POST   /maintenance/verify      verify every completed run whose last
//                                   verification is older than
//                                   backup.verify_interval_hours
//   POST   /maintenance/prune       enforce retention for every job
//
// Capability checks: every route requires `X-User-Id` and `X-Role`
// headers and a role holding `backup:view` (reads) or `backup:manage`
// (writes), resolved through the `role.check` hook, failing closed
// exactly like the ai module. Hook-driven runs (scheduler dispatch)
// are system actors and carry no HTTP caller.
//
// Per-tenant scoping: jobs and runs carry the owning `company_id`
// resolved server-side from the verified caller and the tenant
// context at creation (the `X-Company-Id` header is never
// trusted). A caller with a company only sees that company's jobs
// and runs; a caller with no company is a platform admin and sees
// everything. Cross-company access to a single job or run answers
// 404 so existence is never leaked.
//
// Integrity: verification recomputes the archive checksum AND
// verifies the embedded manifest (job id, kind, and the SHA-256 of
// every listed file). Restores verify checksum + manifest first and
// fail closed on any mismatch. Verification outcomes are recorded
// on the run (`verified`, `last_verified_at`, `manifest_verified`,
// `verification_error`) so the scheduled sweep can skip fresh runs.
//
// Encryption at rest: NOT supported. Archives are plain tar.gz
// files, and there is no `backup.encryption` setting any more:
// the old setting only ever refused to run for any value other
// than `none` (fail closed), because no AEAD crate is in the
// dependency tree and the storage provider upload API carries no
// server-side-encryption flag, so any claim of encryption would
// have been false. Do not re-add an encryption mode without an
// AEAD implementation behind it; until then, protect archives
// with storage-level encryption (encrypted volume / bucket SSE).
// The manifest and run records still carry `encryption: "none"`
// so a future encrypted format can be told apart from today's
// plain archives.
//
// Configuration: the `backup` section is read from the serialized
// application configuration (`base_dir`, `max_total_bytes`,
// `max_file_bytes`, `default_retention_keep_last`,
// `default_retention_keep_days`, `pg_dump_bin`, `psql_bin`,
// `drill_dir`, `verify_interval_hours`). NOTE: the
// section only takes effect once `AppConfig` gains a `backup`
// field; today every value falls back to the defaults below (see
// the integrator notes in the module report for the exact snippet).
// A stale `backup.encryption` key in a config file is ignored:
// at-rest encryption is not supported (see above).
//
// Archives are gzip-compressed tar files containing `manifest.json` plus
// the payload (`dump.sql` for database backups, the captured files for
// file backups, `config.json` for configuration backups). The archive
// key is `backups/<job-id>/<timestamp>-<run>-<kind>.tar.gz`, so every
// upload lands under a unique name.
//
// Storage comes from the shared `StorageRegistry` on the application
// context (`ctx.storage_registry`). A job whose target is not registered
// is rejected at creation time and fails honestly at run time.
//
// Scheduling: creating or updating a job with a cron schedule emits
// `scheduler.enqueue` with kind `backup.run`. The scheduler needs a
// `backup.run` dispatch arm (see the integrator notes in the module
// report); until then the enqueue is refused and the fact is recorded
// on the job, while `run-now` keeps working. The module also handles
// `backup.run_requested` (emitted by the scheduler once it dispatches
// `backup.run`) and re-verifies the latest run of a target when
// `storage.sync.completed` reports that target.
//
// Hooks emitted: `backup.started`, `backup.completed`, `backup.failed`,
// `backup.restored`, `backup.verified`, plus `backup.created` and
// `backup.deleted` for the audit log. Verification and drill
// failures additionally emit `backup.verification_failed` and
// `backup.drill_failed`, and a `notification.requested` alert to
// the `system` user (high priority) so failures are surfaced even
// when the notifications module has no listener for the new events.
// The module also handles `backup.verify_requested` and
// `backup.prune_requested` (emitted by the scheduler once it
// dispatches `backup.verify` / `backup.prune`) for the scheduled
// integrity sweep and retention enforcement.

pub mod ai_capabilities;
pub mod constants;
pub mod types;
pub mod state;
mod validation;
mod config;
mod archive;
mod execution;
mod restore;
mod handlers;
pub mod routes;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::constants::BACKUP_STARTED;
pub use self::constants::BACKUP_COMPLETED;
pub use self::constants::BACKUP_FAILED;
pub use self::constants::BACKUP_RESTORED;
pub use self::constants::BACKUP_VERIFIED;
pub use self::constants::BACKUP_CREATED;
pub use self::constants::BACKUP_DELETED;
pub use self::constants::BACKUP_RUN_REQUESTED;
pub use self::constants::BACKUP_VERIFY_REQUESTED;
pub use self::constants::BACKUP_PRUNE_REQUESTED;
pub use self::constants::BACKUP_VERIFICATION_FAILED;
pub use self::constants::BACKUP_DRILL_COMPLETED;
pub use self::constants::BACKUP_DRILL_FAILED;
pub use self::constants::NOTIFICATION_REQUESTED;
pub use self::constants::STORAGE_SYNC_COMPLETED;
pub use self::constants::SCHEDULER_ENQUEUE;
pub use self::constants::BACKUP_RUN_KIND;
pub use self::constants::PERM_BACKUP_VIEW;
pub use self::constants::PERM_BACKUP_MANAGE;
pub use self::types::BackupKind;
pub use self::types::BackupStatus;
pub use self::types::SchedulingOutcome;
pub use self::types::BackupJob;
pub use self::types::BackupRun;
pub use self::state::BackupState;
pub use self::routes::BackupModule;
pub use self::routes::register;
