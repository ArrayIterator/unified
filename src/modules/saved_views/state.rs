// Saved Views - In-memory saved-views state.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use chrono::Utc;
use dashmap::DashMap;

use super::{CreateSavedView, SavedView, UpdateSavedView, validate_filter, validate_name,
    validate_new, validate_order, validate_sort, validate_visibility};

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

pub struct SavedViewsState {
    pub(crate) views: DashMap<String, SavedView>,
}

impl SavedViewsState {
    pub(crate) fn new() -> Self {
        Self {
            views: DashMap::new(),
        }
    }

    /// Clear the default flag on the user's other views for a module.
    pub(crate) fn clear_other_defaults(&self, user_id: &str, module_id: &str, except_id: &str) {
        for mut entry in self.views.iter_mut() {
            let view = entry.value_mut();
            if view.user_id == user_id
                && view.module_id == module_id
                && view.id != except_id
                && view.is_default
            {
                view.is_default = false;
                view.updated_at = Utc::now();
            }
        }
    }

    pub(crate) fn create(&self, ctx: &AppContext, input: CreateSavedView) -> UnifiedResult<SavedView> {
        let view = validate_new(ctx, input)?;
        let is_default = view.is_default;
        self.views.insert(view.id.clone(), view.clone());
        if is_default {
            self.clear_other_defaults(&view.user_id, &view.module_id, &view.id);
        }
        Ok(view)
    }

    /// Update a view. Only the view's owner may update it: `actor`
    /// is the authenticated caller id, and a non-owner cannot change
    /// the view at all — not its name or filter, and not its
    /// visibility or default flag either.
    pub(crate) fn update(
        &self,
        ctx: &AppContext,
        id: &str,
        actor: &str,
        input: UpdateSavedView,
    ) -> UnifiedResult<SavedView> {
        let mut view = self
            .views
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Saved view not found"))?;
        if view.user_id != actor {
            return Err(UnifiedError::forbidden(
                "Saved view belongs to another user",
            ));
        }
        if let Some(name) = input.name {
            validate_name(&name)?;
            view.name = name.trim().to_string();
        }
        if let Some(filter) = input.filter {
            validate_filter(&filter)?;
            view.filter = filter;
        }
        if let Some(sort) = input.sort {
            view.sort = validate_sort(&sort)?;
        }
        if let Some(order) = input.order {
            view.order = Some(validate_order(&order)?);
        }
        if let Some(visibility) = input.visibility {
            view.visibility = validate_visibility(&visibility)?.to_string();
        }
        let make_default = input.is_default.unwrap_or(false);
        if let Some(is_default) = input.is_default {
            view.is_default = is_default;
        }
        view.updated_at = Utc::now();
        self.views.insert(id.to_string(), view.clone());
        if make_default {
            self.clear_other_defaults(&view.user_id, &view.module_id, &view.id);
        }
        Ok(view)
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::context::AppContext;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use crate::modules::saved_views::SavedViewsModule;
    use crate::modules::saved_views::get_view;
    use std::sync::Arc;

    fn registered_context() -> Arc<AppContext> {
        let ctx = Arc::new(AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        ));
        ctx.modules
            .register(Arc::new(SavedViewsModule::new()))
            .expect("register saved_views module");
        ctx
    }

    fn create_input(module_id: &str) -> CreateSavedView {
        CreateSavedView {
            user_id: "user-1".to_string(),
            module_id: module_id.to_string(),
            name: "Open tickets".to_string(),
            filter: serde_json::json!({"status": "open"}),
            sort: Some("created_at".to_string()),
            order: Some("asc".to_string()),
            visibility: Some("private".to_string()),
            is_default: Some(false),
        }
    }

    fn update_input() -> UpdateSavedView {
        UpdateSavedView {
            name: Some("Renamed".to_string()),
            filter: None,
            sort: None,
            order: None,
            visibility: Some("shared".to_string()),
            is_default: Some(true),
        }
    }

    #[test]
    fn create_then_get_view_round_trip() {
        let ctx = registered_context();
        let state = SavedViewsState::new();
        let created = state.create(&ctx, create_input("saved_views")).unwrap();
        let found = get_view(&state, &created.id).unwrap();
        assert_eq!(found.name, "Open tickets");
        assert_eq!(found.user_id, "user-1");
        assert_eq!(found.module_id, "saved_views");
        assert_eq!(found.sort.as_deref(), Some("created_at"));
        assert_eq!(found.order.as_deref(), Some("asc"));
    }

    #[test]
    fn create_rejects_unknown_module() {
        let ctx = registered_context();
        let state = SavedViewsState::new();
        assert!(state.create(&ctx, create_input("no-such-module")).is_err());
    }

    #[test]
    fn update_allows_the_owner() {
        let ctx = registered_context();
        let state = SavedViewsState::new();
        let created = state.create(&ctx, create_input("saved_views")).unwrap();
        let updated = state
            .update(&ctx, &created.id, "user-1", update_input())
            .unwrap();
        assert_eq!(updated.name, "Renamed");
        assert_eq!(updated.visibility, "shared");
        assert!(updated.is_default);
    }

    #[test]
    fn update_rejects_another_user() {
        let ctx = registered_context();
        let state = SavedViewsState::new();
        let created = state.create(&ctx, create_input("saved_views")).unwrap();
        let err = state
            .update(&ctx, &created.id, "user-2", update_input())
            .unwrap_err();
        assert_eq!(err.code, 403);
        // Nothing was changed: not the name, and not the visibility
        // or default flag either.
        let stored = get_view(&state, &created.id).unwrap();
        assert_eq!(stored.name, "Open tickets");
        assert_eq!(stored.visibility, "private");
        assert!(!stored.is_default);
    }

    #[test]
    fn update_rejects_unknown_view() {
        let ctx = registered_context();
        let state = SavedViewsState::new();
        let err = state
            .update(&ctx, "no-such-id", "user-1", update_input())
            .unwrap_err();
        assert_eq!(err.code, 404);
    }
}
