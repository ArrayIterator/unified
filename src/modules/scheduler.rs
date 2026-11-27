//! Scheduler module - Scheduler / Queue module.
//!
//! Split into submodules by concern; the public API is
//! re-exported below. The full route map and module notes
//! live in `scheduler/module.rs`.

pub mod types;
pub mod state;
mod validation;
mod service;
mod events;
mod handlers;
pub mod module;

pub use self::types::{CreateJob, EnqueueRequest, Job, JobRun, QueueItem, UpdateJob};
pub(crate) use self::types::{KNOWN_KINDS};
pub use self::state::{SchedulerState};
pub(crate) use self::validation::{known_kinds_list, matches_search, new_id, now, paginate,
    respond_created, valid_kind, valid_schedule};
pub(crate) use self::service::{dispatch, process_queue_item, spawn_queue_worker};
pub(crate) use self::events::{on_scheduler_enqueue};
pub(crate) use self::handlers::{create_job, delete_job, enqueue, get_job, list_jobs, list_queue,
    list_runs, run_job, update_job};
pub use self::module::{SchedulerModule, register};
