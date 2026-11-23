//! payments - wallets.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::reference_id;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use std::time::Duration;
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, twocheckout::*, mpgs::*, generic_card::*, custom_provider::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// ---------------------------------------------------------------------------
// Wallet providers (Google Pay / Apple Pay) and local schedules.
// ---------------------------------------------------------------------------
impl ProviderAdapter {
    /// Wallet charge: the wallet payment token is forwarded to the
    /// delegate gateway, which performs the charge. The delegate's
    /// currency list applies (checked in `wallet_currency` by the
    /// caller path through `charge` — see `wallet_charge`).
    pub(crate) async fn wallet_charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        let delegate = self.delegate()?;
        let mut input = input.clone();
        input.currency = self.wallet_currency(&input.currency)?;
        delegate.charge(&input).await
    }

    pub(crate) async fn wallet_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let delegate = self.delegate()?;
        delegate.refund(input).await
    }

    /// Wallet connection test: the delegate connection must exist,
    /// be enabled, and pass its own connection test.
    pub(crate) async fn wallet_test(&self) -> UnifiedResult<String> {
        let delegate = self.delegate()?;
        let result = delegate.test_connection().await?;
        Ok(format!(
            "Wallet delegates to {} ({}); delegate test: {}",
            delegate.kind().as_str(),
            result.mode,
            result.detail
        ))
    }

    // ------------------------------------------------------------------
    // Bank transfer (manual verification): no provider API is ever
    // called. A charge / authorization only records a pending manual
    // verification: the shopper transfers the money to a configured
    // bank account and uploads proof, and an administrator approves
    // it in the bank-transfer module. That approval is the capture
    // and emits `payment.captured`; this adapter deliberately has no
    // provider-side capture to call. Refund and void are manual
    // operations: the adapter records the request with an audit note
    // and the administrator moves the money through their own bank.
    // ------------------------------------------------------------------

    pub(crate) async fn bank_transfer_test(&self) -> UnifiedResult<String> {
        Ok("Bank transfer is a manual method: no provider API to test. Verification is done by an administrator against the configured bank accounts.".to_string())
    }

    pub(crate) fn bank_transfer_transaction_id() -> String {
        reference_id::generate_with_prefix("BTRF")
    }

    pub(crate) async fn bank_transfer_charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        validate_money(input.amount_minor, &input.currency)?;
        Ok(ChargeResult {
            provider_transaction_id: Self::bank_transfer_transaction_id(),
            status: PaymentStatus::Pending,
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: None,
            redirect_url: None,
            requires_action: true,
            authorization_code: None,
            message: Some(
                "Bank transfer selected. Transfer the amount to a configured bank account, then upload the payment proof for administrator verification.".to_string(),
            ),
        })
    }

    pub(crate) async fn bank_transfer_authorize(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        // An authorization holds nothing at a bank: it opens the same
        // pending manual verification as a charge. The administrator
        // approval is the capture.
        self.bank_transfer_charge(input).await
    }

    pub(crate) async fn bank_transfer_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        // Manual refund: no provider API moves the money. The returned
        // record is the audit trail entry for the administrator, who
        // transfers the amount back to the customer through their own
        // bank and keeps the proof with the verification record.
        // Honest caveat: the refund endpoint marks the original
        // transaction refunded and emits `payment.refunded` as soon
        // as this call succeeds, even though the money only moves
        // when the administrator sends it — the refund record stays
        // in status `pending` until then, so the administrator must
        // complete the bank transfer promptly. The integrator should
        // gate the original-transaction status change on a
        // `Refunded` status (see the coordinator report).
        let amount = input.amount_minor.ok_or_else(|| {
            UnifiedError::bad_request("A manual bank-transfer refund needs an explicit amount_minor")
        })?;
        if amount <= 0 {
            return Err(UnifiedError::bad_request(
                "Refund amount must be greater than zero",
            ));
        }
        Ok(RefundResult {
            provider_refund_id: reference_id::generate_with_prefix("RFND"),
            status: PaymentStatus::Pending,
            amount_minor: amount,
            currency: input.currency.clone(),
        })
    }

    pub(crate) async fn bank_transfer_void(&self, provider_transaction_id: &str) -> UnifiedResult<()> {
        // Manual void: there is no authorization held at a provider to
        // release. Cancelling the pending verification is the void;
        // the caller (bank-transfer module) records the audit entry.
        if provider_transaction_id.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "A provider transaction id is required to void",
            ));
        }
        Ok(())
    }

    /// Local-schedule subscription: for providers without a native
    /// subscription object (Xendit, Hyperswitch, 2Checkout, Mastercard,
    /// generic card), the subscription is a record in this module and
    /// the first period is charged immediately against the stored
    /// token. The provider subscription id is the initial charge id,
    /// so a renewal can always be traced back to a real charge.
    pub(crate) async fn local_schedule_subscription_create(
        &self,
        input: &SubscriptionInput,
    ) -> UnifiedResult<SubscriptionResult> {
        let token = input.payment_method_token.clone().ok_or_else(|| {
            UnifiedError::bad_request(format!(
                "Provider \"{}\" subscriptions need a tokenized payment method for the first period charge",
                self.kind.as_str()
            ))
        })?;
        let charge_input = ChargeInput {
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            customer_id: input.customer_id.clone(),
            order_id: input.order_id.clone(),
            reference_id: None,
            invoice_id: None,
            description: Some(format!(
                "Subscription first period (plan {})",
                input.plan_id.clone().unwrap_or_default()
            )),
            payment_method_token: Some(token),
            idempotency_key: input.idempotency_key.clone(),
            three_ds: false,
            billing: None,
        };
        let charge = self.charge(&charge_input).await?;
        // Only a captured first period activates the subscription.
        // A pending charge has moved no money yet: the subscription
        // waits in `pending` and activates when the capture is
        // confirmed, instead of granting a paid period for free.
        let status = first_period_status(charge.status)?;
        Ok(SubscriptionResult {
            provider_subscription_id: charge.provider_transaction_id,
            status: status.to_string(),
            current_period_end: Some(period_end(input.interval.as_str(), input.interval_count, input.trial_days)),
        })
    }
}


/// Status of a local-schedule subscription after its first-period
/// charge: `active` only on a confirmed capture, `pending` while
/// the charge is still in flight, and an error for anything else
/// (a failed first period creates no subscription at all).
pub(crate) fn first_period_status(status: PaymentStatus) -> UnifiedResult<&'static str> {
    match status {
        PaymentStatus::Captured => Ok("active"),
        PaymentStatus::Pending => Ok("pending"),
        other => Err(UnifiedError::bad_request(format!(
            "Subscription first period charge failed with status \"{}\"",
            other.as_str()
        ))),
    }
}


/// End of the next billing period, counting one interval from
/// `anchor` (plus any trial days). Calendar arithmetic, not fixed
/// day counts: a month is a calendar month and a year is a calendar
/// year, so subscriptions do not drift from calendar months and
/// leap years are honoured (31 January + 1 month ends 28 February,
/// or 29 in a leap year).
///
/// Renewals must anchor to `max(previous period end, now)`, never
/// to `now` alone: a late sweep would otherwise grant free days
/// (a monthly period renewed a week late would end a week late),
/// and an early renewal would shorten the paid period. First
/// periods have no previous end, so they anchor to now.
pub(crate) fn period_end_from(
    anchor: DateTime<Utc>,
    interval: &str,
    interval_count: u32,
    trial_days: u32,
) -> DateTime<Utc> {
    let base = match interval {
        "day" => anchor + chrono::Duration::days(interval_count as i64),
        "week" => anchor + chrono::Duration::days(interval_count as i64 * 7),
        "year" => add_calendar_months(anchor, interval_count * 12),
        _ => add_calendar_months(anchor, interval_count),
    };
    base + chrono::Duration::days(trial_days as i64)
}


/// End of the current billing period for a local-schedule
/// subscription, counting from now (plus any trial days).
pub(crate) fn period_end(interval: &str, interval_count: u32, trial_days: u32) -> DateTime<Utc> {
    period_end_from(Utc::now(), interval, interval_count, trial_days)
}


/// Add whole calendar months to a timestamp, clamping the day to
/// the target month's length (31 January + 1 month is 28 or 29
/// February, never 3 March). The time of day is preserved.
pub(crate) fn add_calendar_months(from: DateTime<Utc>, months: u32) -> DateTime<Utc> {
    let date = from.date_naive();
    let total_months = date.year() * 12 + date.month() as i32 - 1 + months as i32;
    let year = total_months.div_euclid(12);
    let month = (total_months.rem_euclid(12) + 1) as u32;
    let day = date.day().min(days_in_month(year, month));
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(year, month, 1).expect("valid year and month"));
    DateTime::<Utc>::from_naive_utc_and_offset(naive.and_time(from.time()), Utc)
}


/// Days in a calendar month, with the leap-year rule for February.
pub(crate) fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, 10, 30, 0).unwrap()
    }

    #[test]
    fn first_period_status_only_activates_on_capture() {
        assert_eq!(
            first_period_status(PaymentStatus::Captured).unwrap(),
            "active"
        );
        assert_eq!(
            first_period_status(PaymentStatus::Pending).unwrap(),
            "pending"
        );
        assert!(first_period_status(PaymentStatus::Failed).is_err());
        assert!(first_period_status(PaymentStatus::Refunded).is_err());
    }

    #[test]
    fn period_end_from_counts_one_interval_from_the_anchor() {
        // The anchor is the previous period end: a renewal a week
        // late still ends one month after the previous end, never
        // a week late (the old now-anchored end granted free days).
        assert_eq!(
            period_end_from(utc(2026, 1, 31), "month", 1, 0),
            utc(2026, 2, 28)
        );
        assert_eq!(
            period_end_from(utc(2026, 1, 15), "week", 2, 0),
            utc(2026, 1, 29)
        );
        assert_eq!(
            period_end_from(utc(2026, 1, 15), "day", 10, 0),
            utc(2026, 1, 25)
        );
        assert_eq!(
            period_end_from(utc(2026, 1, 15), "year", 1, 0),
            utc(2027, 1, 15)
        );
        // Trial days still extend the anchored end.
        assert_eq!(
            period_end_from(utc(2026, 1, 15), "month", 1, 7),
            utc(2026, 2, 22)
        );
    }

    #[test]
    fn calendar_months_do_not_drift_from_the_calendar() {
        // 31 January + 1 month ends in February, clamped to the
        // month's last day — never 3 March (the old +30 days).
        assert_eq!(
            add_calendar_months(utc(2026, 1, 31), 1),
            utc(2026, 2, 28)
        );
        // Leap years are honoured.
        assert_eq!(
            add_calendar_months(utc(2024, 1, 31), 1),
            utc(2024, 2, 29)
        );
        // Year boundaries roll over.
        assert_eq!(
            add_calendar_months(utc(2026, 12, 15), 2),
            utc(2027, 2, 15)
        );
        // Ordinary months keep the day and the time of day.
        assert_eq!(
            add_calendar_months(utc(2026, 3, 10), 1),
            utc(2026, 4, 10)
        );
        // Twelve months is a calendar year, not 365 days: 29
        // February 2024 + 12 months is 28 February 2025.
        assert_eq!(
            add_calendar_months(utc(2024, 2, 29), 12),
            utc(2025, 2, 28)
        );
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(1900, 2), 28);
        assert_eq!(days_in_month(2000, 2), 29);
    }
}
