//! In-memory forums store.

use super::types::{
    Forum, ForumCategory, ForumModerationAction, ForumPoll, ForumPollOption, ForumPollVote,
    ForumPost, ForumPostEdit, ForumReport, ForumReputation, ForumSettings, ForumSubscription,
    ForumThread, ForumVote,
};
use dashmap::DashMap;
use diesel::prelude::*;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use unified_core::db::{
    DbPool,
    models::{
        Forum as DbForum, ForumCategory as DbForumCategory,
        ForumModerationAction as DbForumModerationAction, ForumPoll as DbForumPoll,
        ForumPollOption as DbForumPollOption, ForumPollVote as DbForumPollVote,
        ForumPost as DbForumPost, ForumPostEdit as DbForumPostEdit, ForumReport as DbForumReport,
        ForumReputation as DbForumReputation, ForumSettings as DbForumSettings,
        ForumSubscription as DbForumSubscription, ForumThread as DbForumThread,
        ForumVote as DbForumVote,
    },
};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the forums. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000110_forums`; until that migration is executed,
/// forums data does not survive a restart.
#[derive(Clone)]
pub struct ForumState {
    pub forums: Arc<DashMap<Uuid, Forum>>,
    pub categories: Arc<DashMap<Uuid, ForumCategory>>,
    pub threads: Arc<DashMap<Uuid, ForumThread>>,
    pub posts: Arc<DashMap<Uuid, ForumPost>>,
    pub post_edits: Arc<DashMap<Uuid, ForumPostEdit>>,
    pub votes: Arc<DashMap<Uuid, ForumVote>>,
    pub reports: Arc<DashMap<Uuid, ForumReport>>,
    pub moderation_actions: Arc<DashMap<Uuid, ForumModerationAction>>,
    pub subscriptions: Arc<DashMap<Uuid, ForumSubscription>>,
    pub polls: Arc<DashMap<Uuid, ForumPoll>>,
    pub poll_options: Arc<DashMap<Uuid, ForumPollOption>>,
    pub poll_votes: Arc<DashMap<Uuid, ForumPollVote>>,
    pub reputations: Arc<DashMap<Uuid, ForumReputation>>,
    pub settings: Arc<RwLock<ForumSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ForumState {
    fn default() -> Self {
        Self {
            forums: Arc::new(DashMap::new()),
            categories: Arc::new(DashMap::new()),
            threads: Arc::new(DashMap::new()),
            posts: Arc::new(DashMap::new()),
            post_edits: Arc::new(DashMap::new()),
            votes: Arc::new(DashMap::new()),
            reports: Arc::new(DashMap::new()),
            moderation_actions: Arc::new(DashMap::new()),
            subscriptions: Arc::new(DashMap::new()),
            polls: Arc::new(DashMap::new()),
            poll_options: Arc::new(DashMap::new()),
            poll_votes: Arc::new(DashMap::new()),
            reputations: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(ForumSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ForumState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn notify(
        &self,
        kind: &str,
        user_id: &str,
        title: &str,
        body: &str,
        link: Option<String>,
    ) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "forums",
                "category": "forums",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    pub(crate) fn current_settings(&self) -> ForumSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    /// Hydrate the in-memory forums maps from the database (C11,
    /// task 271): forums, categories, threads, posts, post edits,
    /// votes, reports, moderation actions, subscriptions, polls,
    /// poll options, poll votes, and reputations are loaded from
    /// the `forum*` tables and replace the map contents, so a
    /// restart restores the discussion state instead of losing
    /// it. Every row already carries its `tenant_id` (the tables
    /// stamp it `NOT NULL`): no re-derivation, the database row
    /// is the record. Rows whose stored enum does not parse (an
    /// unknown forum visibility, a vote value other than +1 /
    /// -1) are skipped, never fatal: hydration must not fail
    /// startup on one bad row. Settings are hydrated only when
    /// exactly one `forum_settings` row exists: the runtime
    /// settings are a single installation-wide copy, not
    /// per-tenant, so a multi-tenant settings table needs the
    /// per-tenant settings follow-up first. Call once from
    /// `ForumsModule::init` after the pool is attached (see
    /// `docs/integration/INTEGRATION_NOTES_C11.md` for the
    /// exact call). Returns the number of rows loaded.
    pub(crate) fn hydrate_from_db(&self, pool: &DbPool) -> UnifiedResult<usize> {
        use super::types::ForumVisibility;
        use unified_core::db::schema::{
            forum_categories::dsl as cat_dsl, forum_moderation_actions::dsl as mod_dsl,
            forum_poll_options::dsl as opt_dsl, forum_poll_votes::dsl as pv_dsl,
            forum_polls::dsl as poll_dsl, forum_post_edits::dsl as edit_dsl,
            forum_posts::dsl as post_dsl, forum_reports::dsl as rep_dsl,
            forum_reputation::dsl as reput_dsl, forum_settings::dsl as set_dsl,
            forum_subscriptions::dsl as sub_dsl, forum_threads::dsl as thread_dsl,
            forum_votes::dsl as vote_dsl, forums::dsl as forum_dsl,
        };
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        let db_forums = forum_dsl::forums
            .select(DbForum::as_select())
            .load::<DbForum>(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot load forums: {error}")))?;
        let db_categories = cat_dsl::forum_categories
            .select(DbForumCategory::as_select())
            .load::<DbForumCategory>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load forum categories: {error}"))
            })?;
        let db_threads = thread_dsl::forum_threads
            .select(DbForumThread::as_select())
            .load::<DbForumThread>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load forum threads: {error}"))
            })?;
        let db_posts = post_dsl::forum_posts
            .select(DbForumPost::as_select())
            .load::<DbForumPost>(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot load forum posts: {error}")))?;
        let db_edits = edit_dsl::forum_post_edits
            .select(DbForumPostEdit::as_select())
            .load::<DbForumPostEdit>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load forum post edits: {error}"))
            })?;
        let db_votes = vote_dsl::forum_votes
            .select(DbForumVote::as_select())
            .load::<DbForumVote>(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot load forum votes: {error}")))?;
        let db_reports = rep_dsl::forum_reports
            .select(DbForumReport::as_select())
            .load::<DbForumReport>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load forum reports: {error}"))
            })?;
        let db_actions = mod_dsl::forum_moderation_actions
            .select(DbForumModerationAction::as_select())
            .load::<DbForumModerationAction>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load forum moderation actions: {error}"))
            })?;
        let db_subscriptions = sub_dsl::forum_subscriptions
            .select(DbForumSubscription::as_select())
            .load::<DbForumSubscription>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load forum subscriptions: {error}"))
            })?;
        let db_polls = poll_dsl::forum_polls
            .select(DbForumPoll::as_select())
            .load::<DbForumPoll>(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot load forum polls: {error}")))?;
        let db_options = opt_dsl::forum_poll_options
            .select(DbForumPollOption::as_select())
            .load::<DbForumPollOption>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load forum poll options: {error}"))
            })?;
        let db_poll_votes = pv_dsl::forum_poll_votes
            .select(DbForumPollVote::as_select())
            .load::<DbForumPollVote>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load forum poll votes: {error}"))
            })?;
        let db_reputations = reput_dsl::forum_reputation
            .select(DbForumReputation::as_select())
            .load::<DbForumReputation>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load forum reputations: {error}"))
            })?;
        let db_settings = set_dsl::forum_settings
            .select(DbForumSettings::as_select())
            .load::<DbForumSettings>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load forum settings: {error}"))
            })?;

        let mut loaded = 0;
        self.forums.clear();
        for row in &db_forums {
            let visibility = match row.visibility.as_str() {
                "public" => ForumVisibility::Public,
                "members_only" => ForumVisibility::MembersOnly,
                "staff_only" => ForumVisibility::StaffOnly,
                _ => continue,
            };
            self.forums.insert(
                row.id,
                Forum {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    name: row.name.clone(),
                    description: row.description.clone(),
                    slug: row.slug.clone(),
                    icon: row.icon.clone(),
                    visibility,
                    sort_order: row.sort_order,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.categories.clear();
        for row in &db_categories {
            self.categories.insert(
                row.id,
                ForumCategory {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    forum_id: row.forum_id,
                    name: row.name.clone(),
                    description: row.description.clone(),
                    slug: row.slug.clone(),
                    sort_order: row.sort_order,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.threads.clear();
        for row in &db_threads {
            self.threads.insert(
                row.id,
                ForumThread {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    forum_id: row.forum_id,
                    category_id: row.category_id,
                    title: row.title.clone(),
                    author_id: row.author_id.clone(),
                    tags: row.tags.clone(),
                    pinned: row.pinned,
                    locked: row.locked,
                    solved: row.solved,
                    best_answer_post_id: row.best_answer_post_id,
                    view_count: row.view_count,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.posts.clear();
        for row in &db_posts {
            self.posts.insert(
                row.id,
                ForumPost {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    thread_id: row.thread_id,
                    parent_post_id: row.parent_post_id,
                    depth: row.depth.clamp(0, 2) as i8,
                    author_id: row.author_id.clone(),
                    body: row.body.clone(),
                    hidden: row.hidden,
                    edited: row.edited,
                    edited_at: row.edited_at,
                    quoted_post_id: row.quoted_post_id,
                    attachment_ids: row.attachment_ids.clone(),
                    mentions: row.mentions.clone(),
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.post_edits.clear();
        for row in &db_edits {
            self.post_edits.insert(
                row.id,
                ForumPostEdit {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    post_id: row.post_id,
                    editor_id: row.editor_id.clone(),
                    previous_body: row.previous_body.clone(),
                    edited_at: row.edited_at,
                },
            );
            loaded += 1;
        }
        self.votes.clear();
        for row in &db_votes {
            if !matches!(row.value, 1 | -1) {
                continue;
            }
            self.votes.insert(
                row.id,
                ForumVote {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    post_id: row.post_id,
                    user_id: row.user_id.clone(),
                    value: row.value as i8,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.reports.clear();
        for row in &db_reports {
            self.reports.insert(
                row.id,
                ForumReport {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    target_type: row.target_type.clone(),
                    target_id: row.target_id,
                    reporter_id: row.reporter_id.clone(),
                    reason: row.reason.clone(),
                    status: row.status.clone(),
                    reviewed_by: row.reviewed_by.clone(),
                    reviewed_at: row.reviewed_at,
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.moderation_actions.clear();
        for row in &db_actions {
            self.moderation_actions.insert(
                row.id,
                ForumModerationAction {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    target_type: row.target_type.clone(),
                    target_id: row.target_id,
                    action: row.action.clone(),
                    moderator_id: row.moderator_id.clone(),
                    reason: row.reason.clone(),
                    created_at: row.created_at,
                },
            );
            loaded += 1;
        }
        self.subscriptions.clear();
        for row in &db_subscriptions {
            self.subscriptions.insert(
                row.id,
                ForumSubscription {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    user_id: row.user_id.clone(),
                    target_type: row.target_type.clone(),
                    target_id: row.target_id,
                    created_at: row.created_at,
                },
            );
            loaded += 1;
        }
        self.polls.clear();
        for row in &db_polls {
            self.polls.insert(
                row.id,
                ForumPoll {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    thread_id: row.thread_id,
                    question: row.question.clone(),
                    closed: row.closed,
                    closed_at: row.closed_at,
                    created_by: row.created_by.clone(),
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.poll_options.clear();
        for row in &db_options {
            self.poll_options.insert(
                row.id,
                ForumPollOption {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    poll_id: row.poll_id,
                    text: row.text.clone(),
                    sort_order: row.sort_order,
                    created_at: row.created_at,
                },
            );
            loaded += 1;
        }
        self.poll_votes.clear();
        for row in &db_poll_votes {
            self.poll_votes.insert(
                row.id,
                ForumPollVote {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    poll_id: row.poll_id,
                    option_id: row.option_id,
                    user_id: row.user_id.clone(),
                    created_at: row.created_at,
                },
            );
            loaded += 1;
        }
        self.reputations.clear();
        for row in &db_reputations {
            self.reputations.insert(
                row.id,
                ForumReputation {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    user_id: row.user_id.clone(),
                    score: row.score,
                    upvotes_received: row.upvotes_received,
                    downvotes_received: row.downvotes_received,
                    best_answers: row.best_answers,
                    badge_count: row.badge_count,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        if db_settings.len() == 1 {
            let row = &db_settings[0];
            *self.settings.write().expect("settings lock poisoned") = ForumSettings {
                max_threads_per_hour: row.max_threads_per_hour,
                max_posts_per_hour: row.max_posts_per_hour,
                spam_report_threshold: row.spam_report_threshold,
                new_user_link_limit: row.new_user_link_limit,
                new_user_age_hours: row.new_user_age_hours,
                max_attachments_per_post: row.max_attachments_per_post,
            };
            loaded += 1;
        }
        Ok(loaded)
    }
}
