// Todos module: personal, per-user todos.
//
// Split into one file per concern. The boundary vs the tasks
// module and the ownership rules live in `types.rs`; every
// public item is re-exported below so
// `crate::modules::todos::<Item>` keeps working unchanged.

pub mod ai_capabilities;
pub mod types;
pub mod store;
mod common;
mod validation;
mod access;
mod service;
mod handlers_lists;
mod handlers_todos;
mod handlers_subtasks;
mod handlers_ai;
pub mod module;

pub use self::ai_capabilities::AI_CAPABILITIES;
pub use self::types::EVENT_LIST_CREATED;
pub use self::types::EVENT_LIST_UPDATED;
pub use self::types::EVENT_LIST_DELETED;
pub use self::types::EVENT_LIST_ARCHIVED;
pub use self::types::EVENT_LIST_UNARCHIVED;
pub use self::types::EVENT_LIST_SHARED;
pub use self::types::EVENT_LIST_SHARE_REVOKED;
pub use self::types::EVENT_TODO_CREATED;
pub use self::types::EVENT_TODO_UPDATED;
pub use self::types::EVENT_TODO_DELETED;
pub use self::types::EVENT_TODO_COMPLETED;
pub use self::types::EVENT_TODO_REOPENED;
pub use self::types::EVENT_TODO_OVERDUE;
pub use self::types::EVENT_SUBTASK_CREATED;
pub use self::types::EVENT_SUBTASK_UPDATED;
pub use self::types::EVENT_SUBTASK_TOGGLED;
pub use self::types::EVENT_SUBTASK_DELETED;
pub use self::types::EVENT_ATTACHMENT_ADDED;
pub use self::types::EVENT_ATTACHMENT_REMOVED;
pub use self::types::EVENT_REMINDER_CREATED;
pub use self::types::EVENT_REMINDER_CANCELLED;
pub use self::types::EVENT_REMINDER_SENT;
pub use self::types::EVENT_AI_DRAFT_REQUESTED;
pub use self::types::EVENT_TODOS_CONTEXT_REQUESTED;
pub use self::types::EVENT_REMINDERS_SEND_REQUESTED;
pub use self::types::SCHEDULER_JOB_KIND_REMINDERS;
pub use self::types::PERM_TODOS_READ;
pub use self::types::PERM_TODOS_WRITE;
pub use self::types::PERM_TODOS_SHARE;
pub use self::types::PERM_AI_USE;
pub use self::types::LIST_COLORS;
pub use self::types::LIST_ICONS;
pub use self::types::DEFAULT_LIST_NAME;
pub use self::types::TodoPriority;
pub use self::types::TodoStatus;
pub use self::types::RecurrenceFrequency;
pub use self::types::SharePermission;
pub use self::types::TodoList;
pub use self::types::TodoSubtask;
pub use self::types::TodoAttachment;
pub use self::types::Todo;
pub use self::types::TodoResponse;
pub use self::types::TodoShare;
pub use self::types::TodoCompletionLog;
pub use self::types::TodoReminder;
pub use self::types::StreaksView;
pub use self::types::AiDraftResponse;
pub use self::types::CreateListRequest;
pub use self::types::UpdateListRequest;
pub use self::types::ShareListRequest;
pub use self::types::CreateTodoRequest;
pub use self::types::UpdateTodoRequest;
pub use self::types::CreateSubtaskRequest;
pub use self::types::UpdateSubtaskRequest;
pub use self::types::AddAttachmentRequest;
pub use self::types::CreateReminderRequest;
pub use self::types::AiSummarizeRequest;
pub use self::types::AiPlanDayRequest;
pub use self::store::TodosState;
pub use self::module::TodosModule;
pub use self::module::register;
