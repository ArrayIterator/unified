//! email templates - types.

use unified_core::hooks::HookBus;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use super::render::*;

/// Capability strings for the template routes: reads need
/// `email_templates:read`, every mutation and test-send needs
/// `email_templates:manage`. Checked through the `role.check` hook
/// (see handlers), failing closed when no role handler answers.
pub const PERM_EMAIL_TEMPLATES_READ: &str = "email_templates:read";
pub const PERM_EMAIL_TEMPLATES_MANAGE: &str = "email_templates:manage";

/// One stored version of a template.
#[derive(Debug, Clone, Serialize)]
pub struct TemplateVersion {
    pub version: u32,
    pub subject: String,
    pub html: String,
    pub json: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmailTemplate {
    pub id: String,
    pub name: String,
    pub subject: String,
    pub category: String,
    pub html: String,
    /// GrapeJS design JSON (blocks, styles, component tree).
    pub json: Value,
    pub version: u32,
    pub versions: Vec<TemplateVersion>,
    pub created_at: String,
    pub updated_at: String,
}

impl EmailTemplate {
    pub(crate) fn snapshot(&self) -> TemplateVersion {
        TemplateVersion {
            version: self.version,
            subject: self.subject.clone(),
            html: self.html.clone(),
            json: self.json.clone(),
            created_at: self.updated_at.clone(),
        }
    }
}

pub struct EmailTemplateState {
    pub(crate) templates: RwLock<HashMap<String, EmailTemplate>>,
    pub(crate) hooks: RwLock<Option<Arc<HookBus>>>,
    pub(crate) debug: AtomicBool,
}

impl EmailTemplateState {
    pub(crate) fn new() -> Self {
        Self {
            templates: RwLock::new(HashMap::new()),
            hooks: RwLock::new(None),
            debug: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_hooks(&self, hooks: Arc<HookBus>) {
        *self.hooks.write().expect("template hooks lock poisoned") = Some(hooks);
    }

    pub(crate) fn hooks(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("template hooks lock poisoned").clone()
    }

    pub(crate) fn is_debug(&self) -> bool {
        self.debug.load(Ordering::SeqCst)
    }

    pub(crate) fn get(&self, id: &str) -> Option<EmailTemplate> {
        self.templates
            .read()
            .expect("template lock poisoned")
            .get(id)
            .cloned()
    }

    /// Render a template with variables. Returns rendered HTML, rendered
    /// subject, and the placeholders that had no value.
    pub(crate) fn render(
        template: &EmailTemplate,
        variables: &HashMap<String, Value>,
    ) -> (String, String, Vec<String>) {
        let mut missing = Vec::new();
        let html = substitute(&template.html, variables, &mut missing);
        let subject = substitute(&template.subject, variables, &mut missing);
        (html, subject, missing)
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(hooks) = self.hooks() {
            hooks.emit(event, data);
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct TemplateInput {
    pub(crate) name: String,
    pub(crate) subject: String,
    pub(crate) category: Option<String>,
    pub(crate) html: String,
    pub(crate) json: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TemplateUpdateInput {
    pub(crate) name: Option<String>,
    pub(crate) subject: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) html: Option<String>,
    pub(crate) json: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PreviewInput {
    pub(crate) variables: Option<HashMap<String, Value>>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TestSendInput {
    pub(crate) to: String,
    pub(crate) variables: Option<HashMap<String, Value>>,
}
