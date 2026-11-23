//! Hook handlers for gamification badge events.

use unified_core::hooks::{HookContext};
use unified_core::json;
use chrono::{Utc};
use serde_json::{json};
use super::state::CommunityState;

// ---------------------------------------------------------------------------
// Badge listener (gamification owns badges)
// ---------------------------------------------------------------------------

/// `gamification.badge.awarded` carries `user_id` (and `badge_id`).
/// The member's badge count is incremented when the member exists;
/// a non-member is ignored (joining the community later starts
/// from zero — badge history itself lives in gamification).
pub(crate) fn on_badge_awarded(state: CommunityState, ctx: &mut HookContext) {
    let Some(user_id) = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        ctx.add_result(json!({ "community": "skipped", "reason": "gamification.badge.awarded carries no user_id" }));
        return;
    };
    let member_id = state
        .members
        .iter()
        .find(|entry| entry.value().user_id == user_id)
        .map(|entry| *entry.key());
    let Some(member_id) = member_id else {
        ctx.add_result(json!({ "community": "skipped", "user_id": user_id, "reason": "user is not a community member" }));
        return;
    };
    if let Some(mut member) = state.members.get_mut(&member_id) {
        member.badge_count += 1;
        member.updated_at = Utc::now();
        let count = member.badge_count;
        ctx.add_result(json!({ "community": "badge_count_updated", "user_id": user_id, "badge_count": count }));
    }
}
