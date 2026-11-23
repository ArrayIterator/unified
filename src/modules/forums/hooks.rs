//! Hook handlers for user deletion, gamification badges, and
//! search reindex requests.

use super::helpers::{index_post, index_thread, recompute_reputation};
use super::state::ForumState;
use chrono::Utc;
use serde_json::json;
use unified_core::hooks::HookContext;
use uuid::Uuid;

/// Marker written into `author_id` / `editor_id` when the owning
/// user is deleted. Content is kept (threads and posts stay
/// readable); only the identity is anonymized.
pub(crate) const DELETED_USER_ID: &str = "deleted-user";

// ---------------------------------------------------------------------------
// User deletion (users module owns the event)
// ---------------------------------------------------------------------------

/// `user.deleted` carries `user_id` (falling back to `id`, the
/// same shape the library module reads). The user's threads,
/// posts, and post edits are anonymized to `deleted-user`; their
/// subscriptions, poll votes, and votes are deleted; their
/// reputation row is deleted. Reports they filed keep the
/// reporter id so moderation history stays auditable.
pub(crate) fn on_user_deleted(state: ForumState, ctx: &mut HookContext) {
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
        ctx.add_result(json!({ "forums": "skipped", "reason": "user.deleted carries no user_id" }));
        return;
    };
    let now = Utc::now();
    let mut anonymized_threads = 0i64;
    for mut entry in state.threads.iter_mut() {
        if entry.value().author_id == user_id {
            entry.value_mut().author_id = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_threads += 1;
        }
    }
    let mut anonymized_posts = 0i64;
    for mut entry in state.posts.iter_mut() {
        if entry.value().author_id == user_id {
            entry.value_mut().author_id = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_posts += 1;
        }
    }
    let mut anonymized_edits = 0i64;
    for mut entry in state.post_edits.iter_mut() {
        if entry.value().editor_id == user_id {
            entry.value_mut().editor_id = DELETED_USER_ID.to_string();
            anonymized_edits += 1;
        }
    }
    let removed_subscriptions = remove_where(&state.subscriptions, |row| row.user_id == user_id);
    let removed_poll_votes = remove_where(&state.poll_votes, |row| row.user_id == user_id);
    let removed_votes = remove_where(&state.votes, |row| row.user_id == user_id);
    let removed_reputations = remove_where(&state.reputations, |row| row.user_id == user_id);
    ctx.add_result(json!({
        "forums": "user_deleted",
        "user_id": user_id,
        "anonymized_threads": anonymized_threads,
        "anonymized_posts": anonymized_posts,
        "anonymized_edits": anonymized_edits,
        "removed_subscriptions": removed_subscriptions,
        "removed_poll_votes": removed_poll_votes,
        "removed_votes": removed_votes,
        "removed_reputations": removed_reputations,
    }));
}

/// Remove every row matching `matches` from a store and return how
/// many rows were removed.
fn remove_where<T, F>(store: &std::sync::Arc<dashmap::DashMap<Uuid, T>>, matches: F) -> i64
where
    F: Fn(&T) -> bool,
{
    let keys: Vec<Uuid> = store
        .iter()
        .filter(|entry| matches(entry.value()))
        .map(|entry| *entry.key())
        .collect();
    let count = keys.len() as i64;
    for key in keys {
        store.remove(&key);
    }
    count
}

// ---------------------------------------------------------------------------
// Badge listener (gamification owns badges)
// ---------------------------------------------------------------------------

/// `gamification.badge.awarded` carries `user_id` (and
/// `badge_id`). The user's reputation row gains one badge; the
/// row is created (score 0) when the user has no reputation row
/// yet, so a badge earned before any forum activity is not lost.
pub(crate) fn on_badge_awarded(state: ForumState, ctx: &mut HookContext) {
    let Some(user_id) = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({ "forums": "skipped", "reason": "gamification.badge.awarded carries no user_id" }));
        return;
    };
    let existing = state
        .reputations
        .iter()
        .find(|entry| entry.value().user_id == user_id)
        .map(|entry| *entry.key());
    match existing {
        Some(id) => {
            if let Some(mut row) = state.reputations.get_mut(&id) {
                row.badge_count += 1;
                row.updated_at = Utc::now();
                let count = row.badge_count;
                ctx.add_result(json!({ "forums": "badge_count_updated", "user_id": user_id, "badge_count": count }));
            }
        }
        None => {
            // No reputation row yet: create one with the badge
            // counted, then recount the rest from stored rows so
            // the score is consistent.
            let tenant_id = state
                .threads
                .iter()
                .find(|entry| entry.value().author_id == user_id)
                .map(|entry| entry.value().tenant_id.clone())
                .or_else(|| {
                    state
                        .posts
                        .iter()
                        .find(|entry| entry.value().author_id == user_id)
                        .map(|entry| entry.value().tenant_id.clone())
                })
                .unwrap_or_default();
            if tenant_id.is_empty() {
                ctx.add_result(json!({ "forums": "skipped", "user_id": user_id, "reason": "user has no forum activity to attach a reputation row to" }));
                return;
            }
            recompute_reputation(&state, &tenant_id, &user_id);
            let row_id = state
                .reputations
                .iter()
                .find(|entry| {
                    entry.value().tenant_id == tenant_id && entry.value().user_id == user_id
                })
                .map(|entry| *entry.key());
            if let Some(id) = row_id {
                if let Some(mut row) = state.reputations.get_mut(&id) {
                    row.badge_count += 1;
                    row.updated_at = Utc::now();
                    let count = row.badge_count;
                    ctx.add_result(json!({ "forums": "badge_count_updated", "user_id": user_id, "badge_count": count }));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Search reindex listener (search owns the index)
// ---------------------------------------------------------------------------

/// `search.reindex.requested` republishes every stored thread and
/// post through `search.index` so the search index is rebuilt
/// from the forums stores.
pub(crate) fn on_search_reindex_requested(state: ForumState, ctx: &mut HookContext) {
    let threads: Vec<_> = state
        .threads
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    let posts: Vec<_> = state
        .posts
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    for thread in &threads {
        index_thread(&state, thread);
    }
    for post in &posts {
        index_post(&state, post);
    }
    ctx.add_result(json!({
        "forums": "reindex_requested",
        "threads": threads.len(),
        "posts": posts.len(),
    }));
}
