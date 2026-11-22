// Hook and event bus.
//
// This is the heart of the "fully hookable" requirement: every core behaviour
// can be observed and changed by code. Modules register handlers for named
// events; core code emits events at the important points (startup, reload,
// login, module enable/disable, requests, SSL renewal, geo lookup, ...).
// A handler may read and modify the event payload, add results, or stop
// further handlers from running.
//
// Event payloads are JSON values so that hooks stay decoupled from the
// concrete types of the core.
//
// Ordering: handlers run by priority, highest first; handlers with the
// same priority run in registration order. The default priority is 0,
// so plain `register` calls behave exactly as before. One-shot handlers
// (`register_once`) run at most once and are then removed. A handler
// that emits the same event it handles (directly or indirectly) is
// skipped while it is already running on the same thread, which stops
// accidental infinite recursion instead of overflowing the stack.

use serde_json::Value;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

/// Priority of a handler registered with plain `register`.
/// Higher priorities run first.
pub const DEFAULT_PRIORITY: isize = 0;

/// Well-known core events. Modules may also define their own event names.
pub mod events {
    pub const APP_START: &str = "app.start";
    pub const APP_STOP: &str = "app.stop";
    pub const APP_RELOAD: &str = "app.reload";
    pub const APP_PAUSE: &str = "app.pause";
    pub const APP_RESUME: &str = "app.resume";
    pub const APP_MAINTENANCE: &str = "app.maintenance";
    pub const CONFIG_VALIDATE: &str = "config.validate";
    pub const CONFIG_RELOAD: &str = "config.reload";
    pub const REQUEST_BEFORE: &str = "request.before";
    pub const REQUEST_AFTER: &str = "request.after";
    pub const AUTH_LOGIN: &str = "auth.login";
    pub const AUTH_LOGOUT: &str = "auth.logout";
    pub const AUTH_FAILED: &str = "auth.failed";
    pub const USER_CREATED: &str = "user.created";
    pub const USER_UPDATED: &str = "user.updated";
    pub const ROLE_CREATED: &str = "role.created";
    pub const MODULE_ENABLED: &str = "module.enabled";
    pub const MODULE_DISABLED: &str = "module.disabled";
    pub const SSL_RENEW: &str = "ssl.renew";
    pub const SSL_SWAP: &str = "ssl.swap";
    pub const GEO_LOOKUP: &str = "geo.lookup";
    pub const LICENSE_ISSUED: &str = "license.issued";
    pub const LICENSE_REVOKED: &str = "license.revoked";
    pub const DOWNLOAD_STARTED: &str = "download.started";
    pub const DOWNLOAD_FINISHED: &str = "download.finished";
    pub const SYNC_CONNECTED: &str = "sync.connected";
}

/// The mutable context passed to every hook handler for one event.
pub struct HookContext {
    /// The event name that was emitted.
    pub event: String,
    /// The event payload. Handlers may read and modify it.
    pub data: Value,
    /// Results contributed by handlers, in execution order.
    pub results: Vec<Value>,
    /// When true, no further handlers run for this event.
    stopped: bool,
}

impl HookContext {
    pub fn new(event: &str, data: Value) -> Self {
        Self {
            event: event.to_string(),
            data,
            results: Vec::new(),
            stopped: false,
        }
    }

    /// Stop further handlers from running for this event.
    pub fn stop(&mut self) {
        self.stopped = true;
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Set a top-level key on the event payload.
    pub fn set(&mut self, key: &str, value: Value) {
        if let Value::Object(map) = &mut self.data {
            map.insert(key.to_string(), value);
        }
    }

    /// Read a top-level key from the event payload.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    /// Contribute a result value from this handler.
    pub fn add_result(&mut self, value: Value) {
        self.results.push(value);
    }
}

/// A hook handler. It receives the event context and may modify it.
pub type HookHandler = Arc<dyn Fn(&mut HookContext) + Send + Sync>;

/// One registered handler, as reported by `registrations_detailed`.
#[derive(Debug, Clone)]
pub struct HookRegistration {
    /// Unique id of this handler registration.
    pub id: u64,
    /// The event the handler listens to.
    pub event: String,
    /// The registering module (or "core").
    pub owner: String,
    /// Execution priority; higher runs first.
    pub priority: isize,
    /// True when the handler runs at most once.
    pub once: bool,
}

/// One handler entry inside the bus.
struct HandlerEntry {
    id: u64,
    owner: String,
    priority: isize,
    once: bool,
    handler: HookHandler,
}

impl Clone for HandlerEntry {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            owner: self.owner.clone(),
            priority: self.priority,
            once: self.once,
            handler: Arc::clone(&self.handler),
        }
    }
}

// Ids of handlers currently running on this thread, for the
// re-entrancy guard in `emit`. Thread-local on purpose: a handler may
// legitimately run concurrently on another thread for another emit,
// but re-entering the same handler on the same stack is a recursion
// bug, so that case is skipped.
thread_local! {
    static RUNNING_HANDLERS: RefCell<HashSet<u64>> = const { RefCell::new(HashSet::new()) };
}

/// Marks one handler as running on this thread; removes the mark on
/// drop, even when the handler panics.
struct RunningGuard {
    id: u64,
}

impl RunningGuard {
    /// Returns `None` when the handler is already running on this
    /// thread (re-entrant call), so the caller can skip it.
    fn enter(id: u64) -> Option<Self> {
        let already_running = RUNNING_HANDLERS.with(|running| {
            let mut running = running.borrow_mut();
            if running.contains(&id) {
                true
            } else {
                running.insert(id);
                false
            }
        });
        if already_running {
            None
        } else {
            Some(Self { id })
        }
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        RUNNING_HANDLERS.with(|running| {
            running.borrow_mut().remove(&self.id);
        });
    }
}

/// The hook/event bus. Thread-safe and shared across the whole application.
pub struct HookBus {
    handlers: RwLock<HashMap<String, Vec<HandlerEntry>>>,
    next_id: AtomicU64,
}

impl HookBus {
    pub fn new() -> Self {
        Self {
            handlers: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }

    /// Register a handler for an event at the default priority.
    /// `owner` identifies the registering module (or "core") and is
    /// used for listing and debugging. Returns the handler id, which
    /// can be passed to `unregister_by_id`.
    pub fn register(&self, event: &str, owner: &str, handler: HookHandler) -> u64 {
        self.register_with_priority(event, owner, handler, DEFAULT_PRIORITY)
    }

    /// Register a handler for an event with an explicit priority.
    /// Higher priorities run first; handlers with the same priority
    /// run in registration order. Returns the handler id.
    pub fn register_with_priority(
        &self,
        event: &str,
        owner: &str,
        handler: HookHandler,
        priority: isize,
    ) -> u64 {
        self.register_inner(event, owner, handler, priority, false)
    }

    /// Register a handler that runs at most once and is then removed
    /// automatically. Returns the handler id.
    pub fn register_once(&self, event: &str, owner: &str, handler: HookHandler) -> u64 {
        self.register_inner(event, owner, handler, DEFAULT_PRIORITY, true)
    }

    /// Register a one-shot handler with an explicit priority.
    pub fn register_once_with_priority(
        &self,
        event: &str,
        owner: &str,
        handler: HookHandler,
        priority: isize,
    ) -> u64 {
        self.register_inner(event, owner, handler, priority, true)
    }

    fn register_inner(
        &self,
        event: &str,
        owner: &str,
        handler: HookHandler,
        priority: isize,
        once: bool,
    ) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut handlers = self.handlers.write().expect("hook bus lock poisoned");
        handlers.entry(event.to_string()).or_default().push(HandlerEntry {
            id,
            owner: owner.to_string(),
            priority,
            once,
            handler,
        });
        id
    }

    /// Remove every handler registered by `owner` for `event`.
    /// Returns how many handlers were removed.
    pub fn unregister(&self, event: &str, owner: &str) -> usize {
        let mut handlers = self.handlers.write().expect("hook bus lock poisoned");
        let Some(list) = handlers.get_mut(event) else {
            return 0;
        };
        let before = list.len();
        list.retain(|entry| entry.owner != owner);
        let removed = before - list.len();
        if list.is_empty() {
            handlers.remove(event);
        }
        removed
    }

    /// Remove one handler by the id returned from `register`.
    /// Returns true when a handler was removed.
    pub fn unregister_by_id(&self, event: &str, id: u64) -> bool {
        let mut handlers = self.handlers.write().expect("hook bus lock poisoned");
        let Some(list) = handlers.get_mut(event) else {
            return false;
        };
        let before = list.len();
        list.retain(|entry| entry.id != id);
        let removed = before != list.len();
        if list.is_empty() {
            handlers.remove(event);
        }
        removed
    }

    /// Emit an event and run its handlers by priority (highest first,
    /// registration order within a priority). Returns the final
    /// context, including any payload modifications and collected
    /// results. A handler re-entered on the same thread while it is
    /// already running is skipped. One-shot handlers that ran are
    /// removed afterwards.
    pub fn emit(&self, event: &str, data: Value) -> HookContext {
        let mut context = HookContext::new(event, data);
        let mut handlers = {
            let guard = self.handlers.read().expect("hook bus lock poisoned");
            guard.get(event).cloned().unwrap_or_default()
        };
        // Stable sort: higher priority first, registration order kept
        // within the same priority.
        handlers.sort_by(|a, b| b.priority.cmp(&a.priority));
        let mut ran_once: Vec<u64> = Vec::new();
        for entry in handlers {
            let Some(_guard) = RunningGuard::enter(entry.id) else {
                continue;
            };
            (entry.handler)(&mut context);
            if entry.once {
                ran_once.push(entry.id);
            }
            if context.is_stopped() {
                break;
            }
        }
        if !ran_once.is_empty() {
            let mut handlers = self.handlers.write().expect("hook bus lock poisoned");
            if let Some(list) = handlers.get_mut(event) {
                list.retain(|entry| !ran_once.contains(&entry.id));
                if list.is_empty() {
                    handlers.remove(event);
                }
            }
        }
        context
    }

    /// Emit an event only if at least one handler is registered.
    pub fn emit_if_registered(&self, event: &str, data: Value) -> Option<HookContext> {
        if self.has(event) {
            Some(self.emit(event, data))
        } else {
            None
        }
    }

    /// True when at least one handler is registered for the event.
    pub fn has(&self, event: &str) -> bool {
        let handlers = self.handlers.read().expect("hook bus lock poisoned");
        handlers.get(event).is_some_and(|list| !list.is_empty())
    }

    /// List every registered event name.
    pub fn events(&self) -> Vec<String> {
        let handlers = self.handlers.read().expect("hook bus lock poisoned");
        let mut events: Vec<String> = handlers.keys().cloned().collect();
        events.sort();
        events
    }

    /// List every registered handler as (event, owner) pairs.
    pub fn registrations(&self) -> Vec<(String, String)> {
        let handlers = self.handlers.read().expect("hook bus lock poisoned");
        let mut out = Vec::new();
        for (event, list) in handlers.iter() {
            for entry in list {
                out.push((event.clone(), entry.owner.clone()));
            }
        }
        out.sort();
        out
    }

    /// List every registered handler with its id, priority, and
    /// one-shot flag, ordered by event, then priority (highest
    /// first), then registration order.
    pub fn registrations_detailed(&self) -> Vec<HookRegistration> {
        let handlers = self.handlers.read().expect("hook bus lock poisoned");
        let mut out: Vec<HookRegistration> = Vec::new();
        let mut events: Vec<&String> = handlers.keys().collect();
        events.sort();
        for event in events {
            let mut list = handlers[event].clone();
            list.sort_by(|a, b| b.priority.cmp(&a.priority));
            for entry in list {
                out.push(HookRegistration {
                    id: entry.id,
                    event: event.clone(),
                    owner: entry.owner,
                    priority: entry.priority,
                    once: entry.once,
                });
            }
        }
        out
    }
}

impl Default for HookBus {
    fn default() -> Self {
        Self::new()
    }
}
