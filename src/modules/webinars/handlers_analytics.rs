// webinars -- Analytics handler.

use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;

use super::helpers::*;
use super::state::*;
use super::types::*;

// Analytics handler

pub(crate) async fn get_analytics(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<AnalyticsQuery>,
) -> HttpResponse {
    let result: UnifiedResult<WebinarAnalytics> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let webinar = load_webinar(&state, &path.into_inner())?;
        if !can_manage(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, or an admin can read analytics",
            ));
        }
        let config = state.config();
        let (total, registered, waitlisted, pending, cancelled, denied) =
            registration_counts(&state, webinar.id);
        let sessions: Vec<WebinarAttendance> = state
            .attendance
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id)
            .collect();
        let mut attendee_ids: Vec<String> = Vec::new();
        for session in &sessions {
            if !attendee_ids.contains(&session.user_id) {
                attendee_ids.push(session.user_id.clone());
            }
        }
        let unique_attendees = attendee_ids.len() as u64;
        let attendance_rate = if registered > 0 {
            unique_attendees as f64 / registered as f64
        } else {
            0.0
        };
        let now = Utc::now();
        let total_watch: i64 = sessions
            .iter()
            .map(|row| {
                (row.left_at.unwrap_or(now) - row.joined_at)
                    .num_seconds()
                    .max(0)
            })
            .sum();
        let average_watch_secs = if unique_attendees > 0 {
            total_watch as f64 / unique_attendees as f64
        } else {
            0.0
        };
        // Drop-off buckets across the live window. The bucket width
        // comes from config (or the `bucket_mins` query override)
        // and widens automatically so a very long webinar never
        // produces more than 288 buckets.
        let start = webinar.started_at.unwrap_or(webinar.created_at);
        let end = webinar.ended_at.unwrap_or(now);
        let window_mins = ((end - start).num_seconds() / 60).max(1) as u64;
        let requested = query.bucket_mins.unwrap_or(config.drop_off_bucket_mins);
        let bucket_mins = requested.max(1).max(window_mins.div_ceil(288));
        let mut drop_off: Vec<DropOffBucket> = Vec::new();
        let mut cursor = start;
        while cursor < end {
            let bucket_end = (cursor + chrono::Duration::minutes(bucket_mins as i64)).min(end);
            let active = sessions
                .iter()
                .filter(|row| row.joined_at < bucket_end && row.left_at.unwrap_or(end) > cursor)
                .count() as u64;
            drop_off.push(DropOffBucket {
                bucket_start: cursor,
                bucket_end,
                active_count: active,
            });
            cursor = bucket_end;
        }
        let polls: Vec<WebinarPoll> = state
            .polls
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|poll| poll.webinar_id == webinar.id)
            .collect();
        let mut poll_voters: Vec<String> = Vec::new();
        let mut poll_votes_total = 0u64;
        for entry in state.votes.iter() {
            let vote = entry.value();
            if vote.webinar_id != webinar.id {
                continue;
            }
            poll_votes_total += vote.option_indexes.len() as u64;
            if !poll_voters.contains(&vote.user_id) {
                poll_voters.push(vote.user_id.clone());
            }
        }
        let poll_unique_voters = poll_voters.len() as u64;
        let poll_participation_rate = if unique_attendees > 0 {
            poll_unique_voters as f64 / unique_attendees as f64
        } else {
            0.0
        };
        let questions: Vec<WebinarQuestion> = state
            .questions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.webinar_id == webinar.id)
            .collect();
        let questions_answered = questions.iter().filter(|row| row.answered).count() as u64;
        let mut qa_people: Vec<String> = Vec::new();
        for row in &questions {
            if !qa_people.contains(&row.author_id) {
                qa_people.push(row.author_id.clone());
            }
            for voter in &row.upvoted_by {
                if !qa_people.contains(voter) {
                    qa_people.push(voter.clone());
                }
            }
        }
        let qa_unique_participants = qa_people.len() as u64;
        let qa_participation_rate = if unique_attendees > 0 {
            qa_unique_participants as f64 / unique_attendees as f64
        } else {
            0.0
        };
        let recordings_ready = state
            .recordings
            .iter()
            .filter(|entry| {
                entry.value().webinar_id == webinar.id && entry.value().status == "ready"
            })
            .count() as u64;
        Ok(WebinarAnalytics {
            webinar_id: webinar.id,
            status: status_name(webinar.status).to_string(),
            registrations_total: total,
            registered_count: registered,
            waitlisted_count: waitlisted,
            pending_approval_count: pending,
            cancelled_count: cancelled,
            denied_count: denied,
            unique_attendees,
            attendance_rate,
            average_watch_secs,
            drop_off,
            polls_total: polls.len() as u64,
            poll_votes_total,
            poll_unique_voters,
            poll_participation_rate,
            questions_total: questions.len() as u64,
            questions_answered,
            qa_unique_participants,
            qa_participation_rate,
            recordings_ready,
            follow_up_sent_at: webinar.follow_up_sent_at,
        })
    })();
    match result {
        Ok(analytics) => ok(analytics),
        Err(error) => err(error),
    }
}
