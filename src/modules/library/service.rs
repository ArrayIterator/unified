// Library - Store lookups, hold queue and fine helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use chrono::{DateTime, Utc};
use serde_json::json;
use uuid::Uuid;

use super::{COPY_AVAILABLE, COPY_ON_LOAN, COPY_RESERVED, FINE_PENDING, HOLD_FULFILLED, HOLD_QUEUED,
    HOLD_READY, InventoryAudit, LOAN_ACTIVE, LOAN_OVERDUE, LibraryCategory, LibraryCopy, LibraryFine,
    LibraryHold, LibraryItem, LibraryLoan, LibraryMember, LibraryState, LoanPolicy, MEMBER_ACTIVE,
    MEMBER_EXPIRED, parse_uuid};

/// Resolve the active loan policy for a member type and item type:
/// an exact match first, then the member type with item type
/// `"all"`, then a default policy for member type `"default"`.
/// Fails with 409 when no policy matches — checkout must never
/// fall back to a baked-in number.
pub(crate) fn resolve_policy(
    state: &LibraryState,
    member_type: &str,
    item_type: &str,
) -> UnifiedResult<LoanPolicy> {
    let mut active: Vec<LoanPolicy> = state
        .policies
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|policy| policy.active)
        .collect();
    active.sort_by_key(|policy| policy.created_at);
    let exact = active
        .iter()
        .find(|policy| policy.member_type == member_type && policy.item_type == item_type);
    if let Some(policy) = exact {
        return Ok(policy.clone());
    }
    let member_wide = active
        .iter()
        .find(|policy| policy.member_type == member_type && policy.item_type == "all");
    if let Some(policy) = member_wide {
        return Ok(policy.clone());
    }
    let fallback = active
        .iter()
        .find(|policy| policy.member_type == "default" && policy.item_type == "all");
    if let Some(policy) = fallback {
        return Ok(policy.clone());
    }
    Err(UnifiedError::conflict(format!(
        "No active loan policy covers member type \"{member_type}\" and item type \"{item_type}\"; create a loan policy first"
    )))
}

/// Overdue fine for a loan under a policy: full days past the due
/// date minus the policy grace days, times the daily fine rate.
/// Zero when the loan is inside the grace period.
pub(crate) fn overdue_fine_minor(policy: &LoanPolicy, due_at: DateTime<Utc>, at: DateTime<Utc>) -> i64 {
    if at <= due_at {
        return 0;
    }
    let days = (at - due_at).num_days();
    let chargeable = days - policy.grace_days;
    if chargeable <= 0 {
        return 0;
    }
    chargeable * policy.fine_per_day_minor
}

pub(crate) fn load_item(state: &LibraryState, id: &str) -> UnifiedResult<LibraryItem> {
    let uuid = parse_uuid(id, "item")?;
    state
        .items
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Item \"{id}\" not found")))
}

pub(crate) fn load_copy(state: &LibraryState, id: &str) -> UnifiedResult<LibraryCopy> {
    let uuid = parse_uuid(id, "copy")?;
    state
        .copies
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Copy \"{id}\" not found")))
}

pub(crate) fn load_member(state: &LibraryState, id: &str) -> UnifiedResult<LibraryMember> {
    let uuid = parse_uuid(id, "member")?;
    state
        .members
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Member \"{id}\" not found")))
}

pub(crate) fn load_loan(state: &LibraryState, id: &str) -> UnifiedResult<LibraryLoan> {
    let uuid = parse_uuid(id, "loan")?;
    state
        .loans
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Loan \"{id}\" not found")))
}

pub(crate) fn load_hold(state: &LibraryState, id: &str) -> UnifiedResult<LibraryHold> {
    let uuid = parse_uuid(id, "hold")?;
    state
        .holds
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Hold \"{id}\" not found")))
}

pub(crate) fn load_fine(state: &LibraryState, id: &str) -> UnifiedResult<LibraryFine> {
    let uuid = parse_uuid(id, "fine")?;
    state
        .fines
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Fine \"{id}\" not found")))
}

pub(crate) fn load_policy(state: &LibraryState, id: &str) -> UnifiedResult<LoanPolicy> {
    let uuid = parse_uuid(id, "policy")?;
    state
        .policies
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Loan policy \"{id}\" not found")))
}

pub(crate) fn load_audit(state: &LibraryState, id: &str) -> UnifiedResult<InventoryAudit> {
    let uuid = parse_uuid(id, "inventory audit")?;
    state
        .audits
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Inventory audit \"{id}\" not found")))
}

pub(crate) fn load_category(state: &LibraryState, id: &str) -> UnifiedResult<LibraryCategory> {
    let uuid = parse_uuid(id, "category")?;
    state
        .categories
        .get(&uuid)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Category \"{id}\" not found")))
}

/// Next FIFO position for a new hold on an item.
pub(crate) fn next_hold_position(state: &LibraryState, item_id: Uuid) -> i64 {
    state
        .holds
        .iter()
        .filter(|entry| {
            entry.value().item_id == item_id
                && (entry.value().status == HOLD_QUEUED || entry.value().status == HOLD_READY)
        })
        .map(|entry| entry.value().position)
        .max()
        .unwrap_or(0)
        + 1
}

/// First queued hold for an item, in FIFO order.
pub(crate) fn first_queued_hold(state: &LibraryState, item_id: Uuid) -> Option<LibraryHold> {
    let mut queued: Vec<LibraryHold> = state
        .holds
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|hold| hold.item_id == item_id && hold.status == HOLD_QUEUED)
        .collect();
    queued.sort_by_key(|hold| (hold.position, hold.created_at));
    queued.into_iter().next()
}

/// Offer a returned item to the next queued hold: the hold becomes
/// ready with an expiry from settings, and the member is notified.
pub(crate) fn offer_to_next_hold(state: &LibraryState, item: &LibraryItem) {
    let Some(hold) = first_queued_hold(state, item.id) else {
        return;
    };
    let settings = state.settings();
    let now = Utc::now();
    let mut updated = hold.clone();
    updated.status = HOLD_READY.to_string();
    updated.ready_at = Some(now);
    updated.expires_at = Some(now + chrono::Duration::days(settings.hold_expiry_days));
    updated.updated_at = now;
    state.holds.insert(updated.id, updated.clone());
    state.emit(
        "library.hold.fulfilled",
        json!({
            "hold_id": updated.id.to_string(),
            "member_id": updated.member_id.to_string(),
            "user_id": updated.user_id,
            "item_id": item.id.to_string(),
            "title": item.title,
            "ready_at": now.to_rfc3339(),
            "expires_at": updated.expires_at.map(|value| value.to_rfc3339()),
        }),
    );
    state.notify(
        "library.hold_ready",
        &updated.user_id,
        "Reserved item is ready",
        &format!(
            "The item \"{}\" you reserved is ready for pickup.",
            item.title
        ),
        Some(format!("/library/items/{}", item.id)),
    );
}

/// Create a pending fine for a loan and request the charge from
/// billing. Returns `None` when the amount is zero (no fine owed).
///
/// Atomicity: the "one pending fine per loan and reason" guard
/// holds the loan's fine lock across the scan and the insert, so
/// two concurrent overdue sweeps (or a sweep racing the return
/// path) cannot insert duplicate pending fines for the same loan
/// and reason. The lock is released before the billing event is
/// emitted: the insert has already happened by then, so a second
/// caller scanning afterwards sees the fine.
pub(crate) fn create_fine_for_loan(
    state: &LibraryState,
    loan: &LibraryLoan,
    amount_minor: i64,
    currency: &str,
    reason: &str,
) -> Option<LibraryFine> {
    if amount_minor <= 0 {
        return None;
    }
    let fine = {
        let fine_lock = state.fine_lock(loan.id);
        let _guard = fine_lock.lock().expect("fine lock poisoned");
        // One pending fine per loan and reason: a second sweep must not
        // double-charge the same overdue loan.
        let existing = state.fines.iter().any(|entry| {
            entry.value().loan_id == loan.id
                && entry.value().reason == reason
                && entry.value().status == FINE_PENDING
        });
        if existing {
            return None;
        }
        let now = Utc::now();
        let fine = LibraryFine {
            id: Uuid::new_v4(),
            loan_id: loan.id,
            member_id: loan.member_id,
            user_id: loan.user_id.clone(),
            amount_minor,
            currency: currency.to_string(),
            reason: reason.to_string(),
            status: FINE_PENDING.to_string(),
            waived_by: None,
            paid_reference: None,
            company_id: loan.company_id.clone(),
            created_at: now,
            updated_at: now,
        };
        state.fines.insert(fine.id, fine.clone());
        fine
    };
    state.request_fine(&fine);
    Some(fine)
}

/// Register one member, atomically. Holds the user id's
/// registration lock across the duplicate scan and the insert, so
/// two concurrent registrations for the same user cannot both
/// create an active membership; the loser gets a 409. The caller
/// has already validated `member_type` against settings.
pub(crate) fn insert_member_once(
    state: &LibraryState,
    user_id: String,
    member_type: String,
    expires_at: Option<DateTime<Utc>>,
    notes: Option<String>,
    company_id: Option<String>,
) -> UnifiedResult<LibraryMember> {
    let registration_lock = state.registration_lock(&user_id);
    let _guard = registration_lock
        .lock()
        .expect("registration lock poisoned");
    let duplicate = state.members.iter().any(|entry| {
        entry.value().user_id == user_id && entry.value().status != MEMBER_EXPIRED
    });
    if duplicate {
        return Err(UnifiedError::conflict(format!(
            "User \"{user_id}\" already has an active library membership"
        )));
    }
    let now = Utc::now();
    let member = LibraryMember {
        id: Uuid::new_v4(),
        user_id,
        member_type,
        status: MEMBER_ACTIVE.to_string(),
        joined_at: now,
        expires_at,
        notes,
        company_id,
        created_at: now,
        updated_at: now,
    };
    state.members.insert(member.id, member.clone());
    Ok(member)
}

/// Check a copy (or a digital item) out to a member, atomically.
///
/// The caller has already validated that the member is active, the
/// item is active, and a policy covers the pair; this function
/// re-checks every racy condition under lock, so the pre-checks
/// are a fast path and the checks here are authoritative.
///
/// Locking, in order:
///   1. The member's checkout lock, held from the concurrent-loan
///      count check through the loan insert, so two concurrent
///      checkouts for one member cannot both pass the policy
///      `max_concurrent` limit.
///   2. The copy's `DashMap` entry (`get_mut`), held from the
///      status check through the loan insert and the status mark,
///      so two concurrent checkouts of the same available copy
///      cannot both pass: the second blocks on the entry, then
///      sees `on-loan` and fails with a 409.
/// No other path takes the member lock while holding a copy entry,
/// so this ordering cannot deadlock.
pub(crate) fn checkout_copy(
    state: &LibraryState,
    member: &LibraryMember,
    item: &LibraryItem,
    copy_id: Option<Uuid>,
    policy: &LoanPolicy,
    company_id: Option<String>,
) -> UnifiedResult<LibraryLoan> {
    let member_lock = state.checkout_lock(member.id);
    let _member_guard = member_lock.lock().expect("checkout lock poisoned");
    let now = Utc::now();
    // Authoritative concurrent-loan count, under the member lock.
    let active_count = state
        .loans
        .iter()
        .filter(|entry| {
            entry.value().member_id == member.id
                && (entry.value().status == LOAN_ACTIVE || entry.value().status == LOAN_OVERDUE)
        })
        .count() as i64;
    if active_count >= policy.max_concurrent {
        return Err(UnifiedError::conflict(format!(
            "The member already has {} active loans, the policy maximum for this member type",
            policy.max_concurrent
        )));
    }
    let already_has_item = state.loans.iter().any(|entry| {
        entry.value().member_id == member.id
            && entry.value().item_id == item.id
            && (entry.value().status == LOAN_ACTIVE || entry.value().status == LOAN_OVERDUE)
    });
    if already_has_item {
        return Err(UnifiedError::conflict(
            "The member already has an active loan for this item",
        ));
    }
    // A hold queue blocks checkout for anyone who is not next.
    let queued = first_queued_hold(state, item.id);
    if let Some(hold) = queued {
        if hold.member_id != member.id {
            return Err(UnifiedError::conflict(
                "The item has a hold queue; the next member in the queue checks out first",
            ));
        }
    }
    // Claim the copy: hold its map entry from the status check
    // through the loan insert and the status mark.
    let mut copy_entry = match copy_id {
        Some(copy_uuid) => Some(
            state
                .copies
                .get_mut(&copy_uuid)
                .ok_or_else(|| UnifiedError::not_found(format!("Copy \"{copy_uuid}\" not found")))?,
        ),
        None => None,
    };
    if let Some(entry) = copy_entry.as_deref() {
        let copy = entry;
        if copy.item_id != item.id {
            return Err(UnifiedError::bad_request(
                "The copy does not belong to the requested item",
            ));
        }
        if copy.status == COPY_ON_LOAN {
            return Err(UnifiedError::conflict("The copy is already on loan"));
        }
        if copy.status == COPY_RESERVED {
            let held_for_member = state.holds.iter().any(|entry| {
                entry.value().member_id == member.id
                    && entry.value().item_id == item.id
                    && entry.value().status == HOLD_READY
            });
            if !held_for_member {
                return Err(UnifiedError::conflict(
                    "The copy is reserved for another member",
                ));
            }
        }
        if copy.status != COPY_AVAILABLE && copy.status != COPY_RESERVED {
            return Err(UnifiedError::conflict(format!(
                "The copy cannot be loaned (status: {})",
                copy.status
            )));
        }
    }
    let due_at = now + chrono::Duration::days(policy.loan_days);
    let loan = LibraryLoan {
        id: Uuid::new_v4(),
        member_id: member.id,
        user_id: member.user_id.clone(),
        item_id: item.id,
        copy_id,
        status: LOAN_ACTIVE.to_string(),
        checked_out_at: now,
        due_at,
        returned_at: None,
        renewal_count: 0,
        last_reminded_at: None,
        last_overdue_noticed_at: None,
        company_id,
        created_at: now,
        updated_at: now,
    };
    state.loans.insert(loan.id, loan.clone());
    if let Some(entry) = copy_entry.as_deref_mut() {
        entry.status = COPY_ON_LOAN.to_string();
        entry.updated_at = now;
    }
    drop(copy_entry);
    // A ready hold for this member is consumed by the checkout.
    let ready_holds: Vec<Uuid> = state
        .holds
        .iter()
        .filter(|entry| {
            entry.value().member_id == member.id
                && entry.value().item_id == item.id
                && entry.value().status == HOLD_READY
        })
        .map(|entry| *entry.key())
        .collect();
    for hold_id in ready_holds {
        if let Some(mut entry) = state.holds.get_mut(&hold_id) {
            entry.value_mut().status = HOLD_FULFILLED.to_string();
            entry.updated_at = now;
        }
    }
    Ok(loan)
}

/// Enforce the catalog-search rate limit for one caller. The limit
/// and the window come from settings; exceeding it answers 429.
pub(crate) fn check_search_rate(state: &LibraryState, actor: &str) -> UnifiedResult<()> {
    let settings = state.settings();
    let limit = settings.public_search_rate_limit_per_minute;
    if limit == 0 {
        return Ok(());
    }
    let now = Utc::now().timestamp();
    let window = now / 60;
    let mut entry = state
        .search_rate
        .entry(actor.to_string())
        .or_insert((0, window));
    if entry.1 != window {
        *entry = (0, window);
    }
    entry.0 += 1;
    if entry.0 > limit {
        return Err(UnifiedError::new(
            "Catalog search rate limit exceeded; try again next minute",
            429,
        )
        .with_metadata("retry_after_secs", json!(60 - (now % 60))));
    }
    Ok(())
}

/// Substring match used by catalog search across title, authors,
/// ISBN, category name, and tags. Case-insensitive.
pub(crate) fn item_matches_query(item: &LibraryItem, query: &str) -> bool {
    let needle = query.to_lowercase();
    if item.title.to_lowercase().contains(&needle) {
        return true;
    }
    if item
        .authors
        .iter()
        .any(|author| author.to_lowercase().contains(&needle))
    {
        return true;
    }
    if let Some(isbn) = &item.isbn_normalized {
        if isbn.to_lowercase().contains(&needle) {
            return true;
        }
    }
    if let Some(category) = &item.category_name {
        if category.to_lowercase().contains(&needle) {
            return true;
        }
    }
    item.tags
        .iter()
        .any(|tag| tag.to_lowercase().contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_member(user_id: &str) -> LibraryMember {
        let now = Utc::now();
        LibraryMember {
            id: Uuid::new_v4(),
            user_id: user_id.to_string(),
            member_type: "student".to_string(),
            status: MEMBER_ACTIVE.to_string(),
            joined_at: now,
            expires_at: None,
            notes: None,
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_item(title: &str) -> LibraryItem {
        let now = Utc::now();
        LibraryItem {
            id: Uuid::new_v4(),
            title: title.to_string(),
            subtitle: None,
            authors: vec![],
            isbn: None,
            isbn_normalized: None,
            edition: None,
            series: None,
            series_position: None,
            publisher: None,
            published_year: None,
            language: "en".to_string(),
            category_id: None,
            category_name: None,
            item_type: "book".to_string(),
            status: "active".to_string(),
            description: None,
            cover_ref: None,
            digital_ref: None,
            document_ref: None,
            storage_ref: None,
            tags: vec![],
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_copy(item_id: Uuid) -> LibraryCopy {
        let now = Utc::now();
        LibraryCopy {
            id: Uuid::new_v4(),
            item_id,
            barcode: format!("BC-{}", Uuid::new_v4()),
            serial: None,
            condition: "good".to_string(),
            location: None,
            shelf: None,
            status: COPY_AVAILABLE.to_string(),
            notes: None,
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_policy(max_concurrent: i64) -> LoanPolicy {
        let now = Utc::now();
        LoanPolicy {
            id: Uuid::new_v4(),
            name: "test policy".to_string(),
            member_type: "student".to_string(),
            item_type: "book".to_string(),
            loan_days: 14,
            max_concurrent,
            max_renewals: 1,
            fine_per_day_minor: 100,
            currency: "USD".to_string(),
            grace_days: 0,
            active: true,
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_loan(member: &LibraryMember, item_id: Uuid) -> LibraryLoan {
        let now = Utc::now();
        LibraryLoan {
            id: Uuid::new_v4(),
            member_id: member.id,
            user_id: member.user_id.clone(),
            item_id,
            copy_id: None,
            status: LOAN_ACTIVE.to_string(),
            checked_out_at: now,
            due_at: now,
            returned_at: None,
            renewal_count: 0,
            last_reminded_at: None,
            last_overdue_noticed_at: None,
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Eight threads checking out the same available copy at once
    /// must produce exactly one loan; the rest fail with a conflict
    /// and the copy ends up on-loan.
    #[test]
    fn concurrent_checkout_same_copy_yields_one_loan() {
        let state = Arc::new(LibraryState::default());
        let member = test_member("user-1");
        let item = test_item("Concurrency in Practice");
        let copy = test_copy(item.id);
        let policy = test_policy(5);
        state.members.insert(member.id, member.clone());
        state.items.insert(item.id, item.clone());
        state.copies.insert(copy.id, copy.clone());
        state.policies.insert(policy.id, policy.clone());
        let results: Vec<_> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                let state = Arc::clone(&state);
                handles.push(scope.spawn(move || {
                    checkout_copy(&state, &member, &item, Some(copy.id), &policy, None)
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("checkout thread panicked"))
                .collect()
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|r| r.is_err()).count(), 7);
        assert_eq!(state.loans.len(), 1);
        assert_eq!(
            state.copies.get(&copy.id).unwrap().status,
            COPY_ON_LOAN.to_string()
        );
    }

    /// Eight threads checking out different items for one member
    /// under a `max_concurrent = 1` policy must produce exactly one
    /// loan, even though every copy is distinct.
    #[test]
    fn concurrent_checkout_max_concurrent_enforced() {
        let state = Arc::new(LibraryState::default());
        let member = test_member("user-2");
        let policy = test_policy(1);
        state.members.insert(member.id, member.clone());
        state.policies.insert(policy.id, policy.clone());
        let mut targets = Vec::new();
        for index in 0..4 {
            let item = test_item(&format!("Item {index}"));
            let copy = test_copy(item.id);
            state.items.insert(item.id, item.clone());
            state.copies.insert(copy.id, copy.clone());
            targets.push((item, copy));
        }
        let results: Vec<_> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for thread in 0..8 {
                let state = Arc::clone(&state);
                let (item, copy) = targets[thread % targets.len()].clone();
                handles.push(scope.spawn(move || {
                    checkout_copy(&state, &member, &item, Some(copy.id), &policy, None)
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("checkout thread panicked"))
                .collect()
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(state.loans.len(), 1);
    }

    /// Eight threads raising the same overdue fine at once must
    /// produce exactly one pending fine.
    #[test]
    fn concurrent_duplicate_fine_guard_yields_one_fine() {
        let state = Arc::new(LibraryState::default());
        let member = test_member("user-3");
        let item = test_item("Overdue Book");
        let loan = test_loan(&member, item.id);
        state.loans.insert(loan.id, loan.clone());
        let results: Vec<_> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                let state = Arc::clone(&state);
                handles.push(scope.spawn(move || {
                    create_fine_for_loan(&state, &loan, 500, "USD", "overdue").is_some()
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("fine thread panicked"))
                .collect()
        });
        assert_eq!(results.iter().filter(|created| *created).count(), 1);
        assert_eq!(state.fines.len(), 1);
    }

    /// The fine guard is per loan and reason: a different reason
    /// still raises a fine, while repeating the same reason does not.
    #[test]
    fn fine_guard_is_per_loan_and_reason() {
        let state = LibraryState::default();
        let member = test_member("user-4");
        let item = test_item("Fine Reasons");
        let loan = test_loan(&member, item.id);
        assert!(create_fine_for_loan(&state, &loan, 500, "USD", "overdue").is_some());
        assert!(create_fine_for_loan(&state, &loan, 500, "USD", "overdue").is_none());
        assert!(create_fine_for_loan(&state, &loan, 700, "USD", "lost_replacement").is_some());
        assert_eq!(state.fines.len(), 2);
    }

    /// Eight threads registering the same user at once must produce
    /// exactly one active membership; the rest get a conflict.
    #[test]
    fn concurrent_member_registration_yields_one_member() {
        let state = Arc::new(LibraryState::default());
        let results: Vec<_> = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                let state = Arc::clone(&state);
                handles.push(scope.spawn(move || {
                    insert_member_once(
                        &state,
                        "user-5".to_string(),
                        "student".to_string(),
                        None,
                        None,
                        None,
                    )
                }));
            }
            handles
                .into_iter()
                .map(|handle| handle.join().expect("registration thread panicked"))
                .collect()
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|r| r.is_err()).count(), 7);
        assert_eq!(state.members.len(), 1);
    }

    /// A second registration for the same user still fails
    /// sequentially, and a different user still registers fine.
    #[test]
    fn duplicate_member_registration_rejected() {
        let state = LibraryState::default();
        assert!(insert_member_once(
            &state,
            "user-6".to_string(),
            "student".to_string(),
            None,
            None,
            None,
        )
        .is_ok());
        assert!(insert_member_once(
            &state,
            "user-6".to_string(),
            "staff".to_string(),
            None,
            None,
            None,
        )
        .is_err());
        assert!(insert_member_once(
            &state,
            "user-7".to_string(),
            "student".to_string(),
            None,
            None,
            None,
        )
        .is_ok());
        assert_eq!(state.members.len(), 2);
    }
}
