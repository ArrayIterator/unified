//! Mail - in-memory mail state.
//!
//! Part of the `mail` module split; see `src/modules/mail.rs`.

use unified_core::hooks::HookBus;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use super::types::MailLogEntry;
use super::types::QueuedMail;
use super::types::SmtpSettings;

/// Maximum number of entries kept in the send log. Older entries are
/// dropped on append so the log cannot grow without bound (same cap as
/// the portals module uses for per-portal views).
pub(crate) const MAX_LOG_ENTRIES: usize = 10_000;

pub struct MailState {
    pub(crate) settings: RwLock<Option<SmtpSettings>>,
    pub(crate) log: RwLock<Vec<MailLogEntry>>,
    pub(crate) queue: RwLock<VecDeque<QueuedMail>>,
    pub(crate) hooks: RwLock<Option<Arc<HookBus>>>,
    pub(crate) debug: AtomicBool,
}

impl MailState {
    pub(crate) fn new() -> Self {
        Self {
            settings: RwLock::new(None),
            log: RwLock::new(Vec::new()),
            queue: RwLock::new(VecDeque::new()),
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("mail hooks lock poisoned") = Some(hooks);
    }

    pub(crate) fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("mail hooks lock poisoned").clone()
    }

    pub(crate) fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    pub(crate) fn settings_snapshot(&self) -> Option<SmtpSettings> {
        self.settings
            .read()
            .expect("mail settings lock poisoned")
            .clone()
    }

    pub(crate) fn append_log(&self, entry: MailLogEntry) {
        let mut log = self.log.write().expect("mail log lock poisoned");
        log.push(entry);
        if log.len() > MAX_LOG_ENTRIES {
            let excess = log.len() - MAX_LOG_ENTRIES;
            log.drain(..excess);
        }
    }

    pub(crate) fn update_log_status(&self, id: &str, status: &str, error: Option<String>) {
        let mut log = self.log.write().expect("mail log lock poisoned");
        if let Some(entry) = log.iter_mut().find(|entry| entry.id == id) {
            entry.status = status.to_string();
            entry.error = error;
        }
    }

    pub(crate) fn enqueue(&self, job: QueuedMail) {
        self.queue
            .write()
            .expect("mail queue lock poisoned")
            .push_back(job);
    }

    pub(crate) fn take_queued(&self, job_id: &str) -> Option<QueuedMail> {
        let mut queue = self.queue.write().expect("mail queue lock poisoned");
        let position = queue.iter().position(|job| job.job_id == job_id)?;
        queue.remove(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_round_trip() {
        let state = MailState::new();
        assert!(state.take_queued("missing").is_none());
        let job = QueuedMail {
            job_id: "job-1".to_string(),
            to: "user@example.com".to_string(),
            subject: "Hi".to_string(),
            body_text: None,
            body_html: None,
            template_id: None,
            variables: Default::default(),
        };
        state.enqueue(job);
        let taken = state
            .take_queued("job-1")
            .expect("queued job is returned once");
        assert_eq!(taken.job_id, "job-1");
        assert!(state.take_queued("job-1").is_none());
    }

    #[test]
    fn log_is_capped_at_max_entries() {
        let state = MailState::new();
        for i in 0..(MAX_LOG_ENTRIES + 2) {
            state.append_log(MailLogEntry {
                id: format!("log-{i}"),
                to: "user@example.com".to_string(),
                subject: "Hi".to_string(),
                status: "queued".to_string(),
                connector: "log".to_string(),
                template_id: None,
                error: None,
                created_at: "2026-11-22T00:00:00Z".to_string(),
            });
        }
        let log = state.log.read().expect("log lock");
        assert_eq!(log.len(), MAX_LOG_ENTRIES);
        // Only the newest entries survive; the two oldest were evicted.
        assert_eq!(log[0].id, "log-2");
        assert_eq!(log[log.len() - 1].id, format!("log-{}", MAX_LOG_ENTRIES + 1));
    }

    #[test]
    fn log_append_and_update() {
        let state = MailState::new();
        let entry = MailLogEntry {
            id: "log-1".to_string(),
            to: "user@example.com".to_string(),
            subject: "Hi".to_string(),
            status: "queued".to_string(),
            connector: "log".to_string(),
            template_id: None,
            error: None,
            created_at: "2026-11-22T00:00:00Z".to_string(),
        };
        state.append_log(entry);
        assert_eq!(state.log.read().expect("log lock").len(), 1);
        state.update_log_status("log-1", "sent", None);
        assert_eq!(state.log.read().expect("log lock")[0].status, "sent");
        state.update_log_status("missing", "failed", Some("boom".to_string()));
        assert_eq!(state.log.read().expect("log lock").len(), 1);
    }
}
