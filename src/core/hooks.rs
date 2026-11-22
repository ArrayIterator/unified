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

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

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
    /// Results contributed by handlers, in registration order.
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

/// The hook/event bus. Thread-safe and shared across the whole application.
pub struct HookBus {
    handlers: RwLock<HashMap<String, Vec<(String, HookHandler)>>>,
}

impl HookBus {
    pub fn new() -> Self {
        Self {
            handlers: RwLock::new(HashMap::new()),
        }
    }

    /// Register a handler for an event. `owner` identifies the registering
    /// module (or "core") and is used for listing and debugging.
    pub fn register(&self, event: &str, owner: &str, handler: HookHandler) {
        let mut handlers = self.handlers.write().expect("hook bus lock poisoned");
        handlers
            .entry(event.to_string())
            .or_default()
            .push((owner.to_string(), handler));
    }

    /// Emit an event and run its handlers in registration order.
    /// Returns the final context, including any payload modifications and
    /// collected results.
    pub fn emit(&self, event: &str, data: Value) -> HookContext {
        let mut context = HookContext::new(event, data);
        let handlers = {
            let guard = self.handlers.read().expect("hook bus lock poisoned");
            guard.get(event).cloned().unwrap_or_default()
        };
        for (_, handler) in handlers {
            handler(&mut context);
            if context.is_stopped() {
                break;
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
            for (owner, _) in list {
                out.push((event.clone(), owner.clone()));
            }
        }
        out.sort();
        out
    }
}

impl Default for HookBus {
    fn default() -> Self {
        Self::new()
    }
}
