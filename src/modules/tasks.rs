// Tasks module.
//
// Task management under `/api/v1/tasks`: tasks with title, description,
// assignee (a user id), reporter, priority, a validated status workflow,
// due date (overdue is computed), optional company linkage and project
// name, tags, comments, checklist items, and time estimate / time spent.
//
// Status workflow:
//
//   todo -> in_progress -> review -> done
//   todo / in_progress / review -> blocked -> todo | in_progress
//   any non-terminal status -> cancelled
//
// `done` and `cancelled` are terminal: no transition out of them is
// accepted. An illegal transition is rejected with HTTP 422 and the
// allowed target statuses in the error metadata.
//
// Persistence: tasks live in an in-memory store (the same approach the
// audit module uses). The Diesel tables for durable persistence do not
// exist in crates/unified-core/src/db/schema.rs` yet; the migration SQL and the schema /
// model entries are supplied to the integrator separately and must be
// added before a database-backed implementation can be wired in.
// Nothing in this file references a table that does not exist yet, so
// it compiles against the current schema.
//
// Interconnection happens only through the hook bus. This module emits:
// `task.created`, `task.assigned`, `task.unassigned`, `task.status.changed`,
// `task.completed`, `task.comment.added`, and `task.deleted`. The records module
// subscribes to these events. The assignee is referenced by user id
// only; this module never calls the users module directly.

pub mod ai_capabilities;
mod handlers;
pub mod module;
pub mod requests;
pub mod types;
mod validation;

pub(crate) use self::handlers::add_checklist_item;
pub(crate) use self::handlers::add_comment;
pub(crate) use self::handlers::assign_task;
pub(crate) use self::handlers::change_task_status;
pub(crate) use self::handlers::create_task;
pub(crate) use self::handlers::delete_checklist_item;
pub(crate) use self::handlers::delete_task;
pub(crate) use self::handlers::get_task;
pub(crate) use self::handlers::list_comments;
pub(crate) use self::handlers::list_tasks;
pub(crate) use self::handlers::log_time;
pub(crate) use self::handlers::toggle_checklist_item;
pub(crate) use self::handlers::update_task;
pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::module::TasksModule;
pub use self::module::register;
pub use self::requests::AddChecklistInput;
pub use self::requests::AddCommentInput;
pub use self::requests::AssignInput;
pub use self::requests::ChangeStatusInput;
pub use self::requests::CreateTaskInput;
pub use self::requests::LogTimeInput;
pub use self::requests::UpdateTaskInput;
pub use self::types::ChecklistItem;
pub use self::types::TASK_ASSIGNED;
pub use self::types::TASK_COMMENT_ADDED;
pub use self::types::TASK_COMPLETED;
pub use self::types::TASK_CREATED;
pub use self::types::TASK_STATUS_CHANGED;
pub use self::types::Task;
pub use self::types::TaskComment;
pub use self::types::TaskPriority;
pub use self::types::TaskResponse;
pub use self::types::TaskStatus;
pub use self::types::TasksStore;
pub(crate) use self::validation::client_ip;
pub(crate) use self::validation::parse_date_filter;
pub(crate) use self::validation::parse_uuid_filter;
pub(crate) use self::validation::task_event_payload;
pub(crate) use self::validation::validate_description;
pub(crate) use self::validation::validate_minutes;
pub(crate) use self::validation::validate_tags;
pub(crate) use self::validation::validate_title;
