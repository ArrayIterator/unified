// In-memory session flash messages.
//
// Flash messages are short-lived notifications attached to a session id
// (for example "Profile saved"). They are consumed on read: `get` removes
// them, so a message is shown exactly once. Messages expire after a TTL
// of 5..=60 seconds (default 10 seconds) to prevent abuse, and each kind
// has a per-session threshold (default 100 items); when a kind exceeds
// its threshold the oldest message of that kind is evicted. Expired
// messages are pruned lazily on every access and by `cleanup`, which can
// also run on a background timer via `spawn_periodic_cleanup`.

use dashmap::DashMap;
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Minimum flash message TTL (5 seconds).
pub const MIN_TTL: Duration = Duration::from_secs(5);
/// Maximum flash message TTL (60 seconds).
pub const MAX_TTL: Duration = Duration::from_secs(60);
/// Default flash message TTL (10 seconds).
pub const DEFAULT_TTL: Duration = Duration::from_secs(10);
/// Minimum per-kind threshold (items per kind per session).
pub const MIN_THRESHOLD: u64 = 10;
/// Maximum per-kind threshold (items per kind per session).
pub const MAX_THRESHOLD: u64 = 1000;
/// Default per-kind threshold (items per kind per session).
pub const DEFAULT_THRESHOLD: u64 = 100;

/// The kind of a flash message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FlashKind {
    Success,
    Error,
    Warning,
    Info,
    Custom(String),
}

impl FlashKind {
    /// Stable map key for this kind, used for per-kind thresholds.
    pub fn key(&self) -> String {
        match self {
            FlashKind::Success => "success".to_string(),
            FlashKind::Error => "error".to_string(),
            FlashKind::Warning => "warning".to_string(),
            FlashKind::Info => "info".to_string(),
            FlashKind::Custom(name) => format!("custom:{name}"),
        }
    }
}

impl fmt::Display for FlashKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FlashKind::Success => write!(f, "success"),
            FlashKind::Error => write!(f, "error"),
            FlashKind::Warning => write!(f, "warning"),
            FlashKind::Info => write!(f, "info"),
            FlashKind::Custom(name) => write!(f, "{name}"),
        }
    }
}

impl From<&str> for FlashKind {
    fn from(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "success" => FlashKind::Success,
            "error" => FlashKind::Error,
            "warning" => FlashKind::Warning,
            "info" => FlashKind::Info,
            _ => FlashKind::Custom(value.to_string()),
        }
    }
}

impl From<String> for FlashKind {
    fn from(value: String) -> Self {
        FlashKind::from(value.as_str())
    }
}

/// One flash message.
#[derive(Debug, Clone)]
pub struct FlashItem {
    pub kind: FlashKind,
    pub message: String,
    pub created_at: Instant,
}

impl FlashItem {
    /// Create a message stamped with the current time.
    pub fn new(kind: FlashKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            created_at: Instant::now(),
        }
    }

    /// True when the message is older than `ttl`.
    pub fn is_expired(&self, ttl: Duration) -> bool {
        self.created_at.elapsed() >= ttl
    }
}

struct FlashManagerInner {
    /// Session id -> messages in insertion order.
    sessions: DashMap<String, VecDeque<FlashItem>>,
    /// Message TTL in nanoseconds.
    ttl_nanos: AtomicU64,
    /// Per-kind thresholds, keyed by `FlashKind::key`.
    thresholds: DashMap<String, u64>,
    /// Threshold used for kinds without an explicit override.
    default_threshold: AtomicU64,
}

/// In-memory flash message store, shared by cloning (all clones point at
/// the same underlying data).
#[derive(Debug, Clone)]
pub struct FlashManager {
    inner: Arc<FlashManagerInner>,
}

impl FlashManager {
    /// Create a manager with the default TTL (10 seconds) and default
    /// per-kind threshold (100 items).
    pub fn new() -> Self {
        Self {
            inner: Arc::new(FlashManagerInner {
                sessions: DashMap::new(),
                ttl_nanos: AtomicU64::new(DEFAULT_TTL.as_nanos() as u64),
                thresholds: DashMap::new(),
                default_threshold: AtomicU64::new(DEFAULT_THRESHOLD),
            }),
        }
    }

    /// The current message TTL.
    pub fn ttl(&self) -> Duration {
        Duration::from_nanos(self.inner.ttl_nanos.load(Ordering::SeqCst))
    }

    /// Set the message TTL, clamped to 5..=60 seconds.
    pub fn set_ttl(&self, ttl: Duration) -> &Self {
        let clamped = ttl.clamp(MIN_TTL, MAX_TTL);
        self.inner
            .ttl_nanos
            .store(clamped.as_nanos() as u64, Ordering::SeqCst);
        self
    }

    /// The per-kind threshold for `kind` (explicit override, or the
    /// default threshold when none was set).
    pub fn threshold(&self, kind: &FlashKind) -> u64 {
        self.inner
            .thresholds
            .get(&kind.key())
            .map(|entry| *entry)
            .unwrap_or_else(|| self.default_threshold())
    }

    /// The default per-kind threshold.
    pub fn default_threshold(&self) -> u64 {
        self.inner.default_threshold.load(Ordering::SeqCst)
    }

    /// Set the threshold for one kind, clamped to 10..=1000 items.
    pub fn set_threshold(&self, kind: &FlashKind, max: u64) -> &Self {
        let clamped = max.clamp(MIN_THRESHOLD, MAX_THRESHOLD);
        self.inner.thresholds.insert(kind.key(), clamped);
        self
    }

    /// Set the default threshold for kinds without an explicit override,
    /// clamped to 10..=1000 items.
    pub fn set_default_threshold(&self, max: u64) -> &Self {
        let clamped = max.clamp(MIN_THRESHOLD, MAX_THRESHOLD);
        self.inner
            .default_threshold
            .store(clamped, Ordering::SeqCst);
        self
    }

    /// Store a flash message for a session. Expired messages of that
    /// session are pruned first; when the kind already holds its
    /// threshold of messages, the oldest message of that kind is evicted.
    pub fn flash(&self, session_id: &str, kind: FlashKind, message: impl Into<String>) {
        let ttl = self.ttl();
        let item = FlashItem::new(kind.clone(), message);
        let mut entry = self
            .inner
            .sessions
            .entry(session_id.to_string())
            .or_default();
        entry.retain(|existing| !existing.is_expired(ttl));
        let threshold = self.threshold(&kind) as usize;
        while entry.iter().filter(|existing| existing.kind == kind).count() >= threshold {
            match entry.iter().position(|existing| existing.kind == kind) {
                Some(position) => {
                    entry.remove(position);
                }
                None => break,
            }
        }
        entry.push_back(item);
    }

    /// Convenience: store a success message.
    pub fn flash_success(&self, session_id: &str, message: impl Into<String>) {
        self.flash(session_id, FlashKind::Success, message);
    }

    /// Convenience: store an error message.
    pub fn flash_error(&self, session_id: &str, message: impl Into<String>) {
        self.flash(session_id, FlashKind::Error, message);
    }

    /// Convenience: store a warning message.
    pub fn flash_warning(&self, session_id: &str, message: impl Into<String>) {
        self.flash(session_id, FlashKind::Warning, message);
    }

    /// Convenience: store an info message.
    pub fn flash_info(&self, session_id: &str, message: impl Into<String>) {
        self.flash(session_id, FlashKind::Info, message);
    }

    /// Read and consume all unexpired messages for a session, in
    /// insertion order. The session entry is removed, so a second call
    /// returns an empty list.
    pub fn get(&self, session_id: &str) -> Vec<FlashItem> {
        let ttl = self.ttl();
        match self.inner.sessions.remove(session_id) {
            Some((_, items)) => items
                .into_iter()
                .filter(|item| !item.is_expired(ttl))
                .collect(),
            None => Vec::new(),
        }
    }

    /// Read all unexpired messages for a session without consuming them.
    pub fn peek(&self, session_id: &str) -> Vec<FlashItem> {
        let ttl = self.ttl();
        match self.inner.sessions.get(session_id) {
            Some(items) => items
                .iter()
                .filter(|item| !item.is_expired(ttl))
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    }

    /// Number of unexpired messages stored for a session.
    pub fn len(&self, session_id: &str) -> usize {
        self.peek(session_id).len()
    }

    /// True when a session has no unexpired messages.
    pub fn is_empty(&self, session_id: &str) -> bool {
        self.len(session_id) == 0
    }

    /// Remove all messages of a session. Returns how many were stored.
    pub fn clear(&self, session_id: &str) -> usize {
        match self.inner.sessions.remove(session_id) {
            Some((_, items)) => items.len(),
            None => 0,
        }
    }

    /// Number of sessions currently holding messages.
    pub fn session_count(&self) -> usize {
        self.inner.sessions.len()
    }

    /// Remove expired messages from every session and drop sessions left
    /// empty. Returns the number of expired messages removed.
    pub fn cleanup(&self) -> usize {
        let ttl = self.ttl();
        let mut removed = 0usize;
        self.inner.sessions.retain(|_, items| {
            let before = items.len();
            items.retain(|item| !item.is_expired(ttl));
            removed += before - items.len();
            !items.is_empty()
        });
        removed
    }

    /// Spawn a background task that runs `cleanup` every `interval`.
    /// Must be called from inside a Tokio runtime. The task runs until
    /// the process exits; the interval is clamped to at least one second.
    pub fn spawn_periodic_cleanup(self: Arc<Self>, interval: Duration) {
        let interval = interval.max(Duration::from_secs(1));
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                ticker.tick().await;
                self.cleanup();
            }
        });
    }
}

impl Default for FlashManager {
    fn default() -> Self {
        Self::new()
    }
}
