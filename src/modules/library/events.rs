// Library - Hook handlers for billing, users and AI grounding.

use unified_core::hooks::HookContext;
use unified_core::json;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

use super::{COPY_AVAILABLE, FINE_PAID, FINE_PENDING, ITEM_ACTIVE, LOAN_ACTIVE, LOAN_OVERDUE,
    LibraryItem, LibraryState, MAX_GROUNDING_CATEGORIES, MAX_GROUNDING_MATCHES, MEMBER_ACTIVE,
    MEMBER_SUSPENDED, item_matches_query, opt_string};

// Hook handlers

/// Grounding for `library.ai.search_assist_requested`: add catalog
/// matches for the query. Catalog data only - no member data.
pub(crate) fn on_search_assist_grounding(state: LibraryState, ctx: &mut HookContext) {
    let query = ctx
        .get("query")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("input")
                .and_then(|value| value.get("query"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    if query.trim().is_empty() {
        return;
    }
    let settings = state.settings();
    let limit = settings.max_grounding_matches.min(MAX_GROUNDING_MATCHES).max(1);
    let mut matches: Vec<LibraryItem> = state
        .items
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.status == ITEM_ACTIVE && item_matches_query(item, &query))
        .collect();
    matches.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
    matches.truncate(limit);
    let grounded: Vec<Value> = matches
        .iter()
        .map(|item| {
            let available = state
                .copies
                .iter()
                .filter(|entry| {
                    entry.value().item_id == item.id && entry.value().status == COPY_AVAILABLE
                })
                .count();
            json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "authors": item.authors,
                "isbn": item.isbn_normalized,
                "category_name": item.category_name,
                "item_type": item.item_type,
                "available_copies": available,
            })
        })
        .collect();
    ctx.add_result(json!({
        "source": "library.catalog",
        "query": query,
        "matches": grounded,
    }));
}

/// Grounding for `library.ai.recommendations_requested`: add the
/// member's anonymised loan-history category profile plus available
/// items in those categories. Category counts only - never raw
/// member personal data, never loan titles.
pub(crate) fn on_recommendations_grounding(state: LibraryState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("input")
                .and_then(|value| value.get("user_id"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    if user_id.is_empty() {
        return;
    }
    let member = state
        .members
        .iter()
        .map(|entry| entry.value().clone())
        .find(|member| member.user_id == user_id);
    let Some(member) = member else {
        return;
    };
    let mut category_counts: HashMap<String, i64> = HashMap::new();
    let mut on_loan_items: Vec<Uuid> = Vec::new();
    for loan in state.loans.iter() {
        let loan = loan.value();
        if loan.member_id != member.id {
            continue;
        }
        if loan.status == LOAN_ACTIVE || loan.status == LOAN_OVERDUE {
            on_loan_items.push(loan.item_id);
        }
        if let Some(item) = state.items.get(&loan.item_id) {
            if let Some(category) = &item.value().category_name {
                *category_counts.entry(category.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut ranked: Vec<(String, i64)> = category_counts.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.truncate(MAX_GROUNDING_CATEGORIES);
    let top_categories: Vec<String> = ranked.iter().map(|entry| entry.0.clone()).collect();
    let settings = state.settings();
    let limit = settings.max_grounding_matches.min(MAX_GROUNDING_MATCHES).max(1);
    let mut suggestions: Vec<Value> = Vec::new();
    for category in &top_categories {
        let mut items: Vec<LibraryItem> = state
            .items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                item.status == ITEM_ACTIVE
                    && item.category_name.as_deref() == Some(category.as_str())
                    && !on_loan_items.contains(&item.id)
            })
            .collect();
        items.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)));
        for item in items {
            if suggestions.len() >= limit {
                break;
            }
            suggestions.push(json!({
                "item_id": item.id.to_string(),
                "title": item.title,
                "authors": item.authors,
                "category_name": item.category_name,
                "item_type": item.item_type,
            }));
        }
        if suggestions.len() >= limit {
            break;
        }
    }
    ctx.add_result(json!({
        "source": "library.loan_history_profile",
        "category_counts": ranked.iter().map(|entry| json!({
            "category": entry.0,
            "loans": entry.1,
        })).collect::<Vec<Value>>(),
        "suggestions": suggestions,
    }));
}

/// A billing payment that names a library fine marks it paid.
/// Both real emitters nest the record: `invoice.paid` carries
/// `{"invoice": {...}, "method": ...}` and `payment.completed`
/// carries the order itself. The fine link is read from the nested
/// `invoice` (or `order`) record when one is present
/// (`fine_id`, or `reference_type` = `"library.fine"` with
/// `reference_id`); top-level fields are kept for a direct emit
/// that names the fine itself.
pub(crate) fn on_billing_paid(state: LibraryState, ctx: &mut HookContext) {
    let nested = ctx.get("invoice").or_else(|| ctx.get("order"));
    let fine_id = nested
        .and_then(|record| record.get("fine_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            let is_library_fine = nested
                .and_then(|record| record.get("reference_type"))
                .and_then(|value| value.as_str())
                == Some("library.fine");
            if is_library_fine {
                nested
                    .and_then(|record| record.get("reference_id"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            } else {
                None
            }
        })
        .or_else(|| {
            ctx.get("fine_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            let is_library_fine =
                ctx.get("reference_type").and_then(|value| value.as_str()) == Some("library.fine");
            if is_library_fine {
                ctx.get("reference_id")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            } else {
                None
            }
        });
    let Some(raw) = fine_id else {
        return;
    };
    let Ok(uuid) = Uuid::parse_str(&raw) else {
        return;
    };
    let Some(mut entry) = state.fines.get_mut(&uuid) else {
        return;
    };
    if entry.value().status != FINE_PENDING {
        return;
    }
    let reference = ctx
        .get("payment_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| opt_string(&ctx.data, "reference"))
        .or_else(|| {
            nested
                .and_then(|record| record.get("reference"))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    entry.value_mut().status = FINE_PAID.to_string();
    entry.value_mut().paid_reference = reference;
    entry.value_mut().updated_at = Utc::now();
    let fine = entry.value().clone();
    drop(entry);
    state.emit(
        "library.fine.paid",
        json!({
            "fine_id": fine.id.to_string(),
            "loan_id": fine.loan_id.to_string(),
            "member_id": fine.member_id.to_string(),
            "user_id": fine.user_id,
            "amount_minor": fine.amount_minor,
            "currency": fine.currency,
            "paid_reference": fine.paid_reference,
        }),
    );
}

/// A deleted user suspends every library membership tied to them.
pub(crate) fn on_user_deleted(state: LibraryState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(user_id) = user_id else {
        return;
    };
    let now = Utc::now();
    for mut entry in state.members.iter_mut() {
        if entry.value().user_id == user_id && entry.value().status == MEMBER_ACTIVE {
            entry.value_mut().status = MEMBER_SUSPENDED.to_string();
            entry.value_mut().updated_at = now;
            let member = entry.value().clone();
            state.emit(
                "library.member.suspended",
                json!({
                    "member_id": member.id.to_string(),
                    "user_id": member.user_id,
                    "reason": "user_deleted",
                }),
            );
        }
    }
}
