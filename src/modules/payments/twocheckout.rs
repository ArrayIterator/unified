//! payments - twocheckout.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use super::{events::*, card_validation::*, card_settings::*, limits::*, providers::*, types::*, reference_ids::*, credentials::*, records::*, adapter::*, dispatch::*, stripe::*, paypal::*, xendit::*, midtrans::*, hyperswitch::*, mpgs::*, generic_card::*, custom_provider::*, wallets::*, state::*, requests::*, handlers_catalog::*, handlers_intents::*, handlers_charges::*, handlers_methods::*, handlers_subscriptions::*, handlers_checkout::*, handlers_webhooks::*, handlers_split_bills::*, hooks::*, module::*};



// 2Checkout.
impl ProviderAdapter {
    /// 2Checkout charge: places an order with a client-side token
    /// (EesToken). Billing details are required by 2CO and must be
    /// supplied on the charge request; they are never invented here.
    pub(crate) async fn two_checkout_charge(&self, input: &ChargeInput) -> UnifiedResult<ChargeResult> {
        let token = validate_token(input.payment_method_token.as_deref(), "2checkout")?;
        let billing = input.billing.clone().ok_or_else(|| {
            UnifiedError::bad_request(
                "2Checkout orders need billing details (name, email, country) on the charge request",
            )
        })?;
        if billing.email.trim().is_empty() || billing.country.trim().is_empty() {
            return Err(UnifiedError::bad_request(
                "2Checkout orders need a billing email and country",
            ));
        }
        let url = self.url("/rest/6.0/orders")?;
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&json!({
                    "Items": [{
                        "Name": input.description.clone().unwrap_or_else(|| "Payment".to_string()),
                        "Quantity": 1,
                        "Price": {
                            // Exact JSON number from the decimal
                            // string: never a float, and never a
                            // silent 0.0 on a malformed amount.
                            "Amount": major_decimal_value(input.amount_minor, &input.currency)?,
                            "Currency": input.currency,
                        },
                    }],
                    "BillingDetails": {
                        "FirstName": billing.name,
                        "Email": billing.email,
                        "Country": billing.country,
                        "City": billing.city.unwrap_or_default(),
                        "Address": billing.address.unwrap_or_default(),
                    },
                    "PaymentDetails": {
                        "Type": "CC",
                        "Currency": input.currency,
                        "PaymentMethod": { "EesToken": token },
                    },
                })),
            "2checkout",
        )
        .await?;
        let ref_no = body
            .get("RefNo")
            .and_then(|value| value.as_str())
            .or_else(|| body.get("OrderNumber").and_then(|value| value.as_str()))
            .ok_or_else(|| {
                UnifiedError::internal("2Checkout order response has no reference number")
            })?
            .to_string();
        let raw_status = json_str(&body, &["Status"]).unwrap_or("unknown");
        Ok(ChargeResult {
            provider_transaction_id: ref_no,
            status: map_status(PaymentProviderKind::TwoCheckout, raw_status),
            amount_minor: input.amount_minor,
            currency: input.currency.clone(),
            fee_minor: fee_from(&body),
            redirect_url: None,
            requires_action: false,
            authorization_code: None,
            message: None,
        })
    }

    pub(crate) async fn two_checkout_refund(&self, input: &RefundInput) -> UnifiedResult<RefundResult> {
        let url = self.url(&format!(
            "/rest/6.0/orders/{}/refund",
            input.provider_transaction_id
        ))?;
        let mut body_json = json!({
            "comment": input.reason.clone().unwrap_or_else(|| "Refund requested".to_string()),
        });
        if let Some(amount) = input.amount_minor {
            body_json["amount"] = Value::from(major_amount(amount, &input.currency));
            body_json["currency"] = Value::String(input.currency.clone());
        }
        let body = send_json(
            self.client
                .request(Method::POST, url)
                .headers(self.auth_headers()?)
                .json(&body_json),
            "2checkout",
        )
        .await?;
        let id = json_str(&body, &["RefNo"])
            .unwrap_or(&input.provider_transaction_id)
            .to_string();
        Ok(RefundResult {
            provider_refund_id: id,
            status: PaymentStatus::Refunded,
            amount_minor: input.amount_minor.unwrap_or(0),
            currency: input.currency.clone(),
        })
    }

    pub(crate) async fn two_checkout_test(&self) -> UnifiedResult<String> {
        let url = self.url("/rest/6.0/orders?limit=1")?;
        let (status, _) = call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "2checkout",
        )
        .await?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(UnifiedError::unauthorized(
                "2Checkout rejected the credentials (HTTP 401/403)",
            ));
        }
        if !status.is_success() {
            return Err(UnifiedError::internal(format!(
                "2Checkout orders check failed with HTTP {status}"
            )));
        }
        Ok("2Checkout API reachable; credentials accepted".to_string())
    }

    /// 2Checkout webhook verification: server-side. The native INS
    /// signature is MD5-based and no MD5 implementation is available
    /// in this codebase, so a webhook is only trusted after the sale
    /// it names is re-fetched from the 2Checkout API.
    /// Fetch a 2Checkout order, so the webhook outcome can be read
    /// from the provider instead of trusting the body.
    pub(crate) async fn two_checkout_fetch_order(
        &self,
        ref_no: &str,
    ) -> UnifiedResult<(StatusCode, Value)> {
        let url = self.url(&format!("/rest/6.0/orders/{ref_no}"))?;
        call_raw(
            self.client
                .request(Method::GET, url)
                .headers(self.auth_headers()?),
            "2checkout",
        )
        .await
    }

    pub(crate) async fn two_checkout_webhook_verify(&self, raw_body: &[u8]) -> UnifiedResult<bool> {
        let body: Value = serde_json::from_slice(raw_body).map_err(|_| {
            UnifiedError::bad_request("2Checkout webhook body is not valid JSON")
        })?;
        let ref_no = json_str(&body, &["sale_id", "RefNo", "order_number", "refno"])
            .ok_or_else(|| {
                UnifiedError::bad_request("2Checkout webhook body carries no sale reference to verify")
            })?;
        let (status, _) = self.two_checkout_fetch_order(ref_no).await?;
        Ok(status.is_success())
    }

    /// Server-side outcome of a 2Checkout webhook: the status of the
    /// fetched order, mapped the same way the charge path maps order
    /// statuses. A fetch failure is an error (fail closed).
    pub(crate) async fn two_checkout_webhook_outcome(&self, raw_body: &[u8]) -> UnifiedResult<PaymentStatus> {
        let body: Value = serde_json::from_slice(raw_body).map_err(|_| {
            UnifiedError::bad_request("2Checkout webhook body is not valid JSON")
        })?;
        let ref_no = json_str(&body, &["sale_id", "RefNo", "order_number", "refno"])
            .ok_or_else(|| {
                UnifiedError::bad_request("2Checkout webhook body carries no sale reference to verify")
            })?;
        let (status, order) = self.two_checkout_fetch_order(ref_no).await?;
        if !status.is_success() {
            return Err(UnifiedError::internal(
                "2Checkout order lookup failed while confirming the webhook outcome",
            ));
        }
        let raw_status = json_str(&order, &["Status"]).unwrap_or("unknown");
        Ok(map_status(PaymentProviderKind::TwoCheckout, raw_status))
    }
}
