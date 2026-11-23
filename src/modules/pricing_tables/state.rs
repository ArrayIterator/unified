//! pricing tables - state.

use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, RwLock};
use super::types::*;
use super::validation::*;

pub struct PricingTablesState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) tables: DashMap<String, PricingTable>,
    pub(crate) tiers: DashMap<String, PricingTier>,
    pub(crate) features: DashMap<String, TierFeature>,
    pub(crate) vendors: DashMap<String, Vendor>,
    pub(crate) commission_rules: DashMap<String, CommissionRule>,
    pub(crate) product_approvals: DashMap<String, ProductApproval>,
    pub(crate) commissions: DashMap<String, Commission>,
    pub(crate) payouts: DashMap<String, Payout>,
    /// Invoice references seen on `invoice.created`, keyed by the
    /// invoice id, kept for payout reconciliation on the dashboard.
    pub(crate) invoice_refs: DashMap<String, Value>,
}

impl PricingTablesState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            tables: DashMap::new(),
            tiers: DashMap::new(),
            features: DashMap::new(),
            vendors: DashMap::new(),
            commission_rules: DashMap::new(),
            product_approvals: DashMap::new(),
            commissions: DashMap::new(),
            payouts: DashMap::new(),
            invoice_refs: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Tiers of one table, in sort order (ties by creation time).
    pub(crate) fn tiers_of(&self, table_id: &str) -> Vec<PricingTier> {
        let mut tiers: Vec<PricingTier> = self
            .tiers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|tier| tier.table_id == table_id)
            .collect();
        tiers.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at))
        });
        tiers
    }

    /// Features of one tier, in sort order (ties by creation time).
    pub(crate) fn features_of(&self, tier_id: &str) -> Vec<TierFeature> {
        let mut features: Vec<TierFeature> = self
            .features
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|feature| feature.tier_id == tier_id)
            .collect();
        features.sort_by(|a, b| {
            a.sort_order
                .cmp(&b.sort_order)
                .then(a.created_at.cmp(&b.created_at))
        });
        features
    }

    /// Clear the highlighted flag on every tier of a table except
    /// `except_tier_id`. This is the single place that enforces the
    /// exactly-one-highlighted-tier invariant on write.
    pub(crate) fn clear_other_highlights(&self, table_id: &str, except_tier_id: &str) {
        for mut entry in self.tiers.iter_mut() {
            if entry.value().table_id == table_id
                && entry.key() != except_tier_id
                && entry.value().highlighted
            {
                entry.value_mut().highlighted = false;
                entry.value_mut().updated_at = now();
            }
        }
    }

    /// After the highlighted tier of a table is deleted, promote the
    /// lowest sort-order remaining tier so the table keeps exactly
    /// one highlighted tier.
    pub(crate) fn promote_next_highlight(&self, table_id: &str) {
        let tiers = self.tiers_of(table_id);
        if tiers.iter().any(|tier| tier.highlighted) || tiers.is_empty() {
            return;
        }
        if let Some(first) = tiers.first() {
            if let Some(mut entry) = self.tiers.get_mut(&first.id) {
                entry.value_mut().highlighted = true;
                entry.value_mut().updated_at = now();
            }
        }
    }

    /// Resolve the commission rate for a vendor and an optional
    /// product category: vendor rule wins, then category rule, then
    /// the global rule, then the built-in default.
    pub(crate) fn commission_rate_for(&self, vendor_id: &str, category: Option<&str>) -> f64 {
        let by_scope = |scope: &str, value: &str| {
            self.commission_rules
                .iter()
                .find(|entry| entry.value().scope == scope && entry.value().scope_value == value)
                .map(|entry| entry.value().rate_percent)
        };
        if let Some(rate) = by_scope("vendor", vendor_id) {
            return rate;
        }
        if let Some(category) = category {
            if let Some(rate) = by_scope("category", category) {
                return rate;
            }
        }
        if let Some(rate) = by_scope("global", "") {
            return rate;
        }
        DEFAULT_COMMISSION_RATE_PERCENT
    }

    /// Resolve the vendor that owns a store order line: the approved
    /// product approval link first, then a vendor whose `company_id`
    /// matches the line's `company_id`. Returns the vendor id and the
    /// product category captured on the approval, when known.
    pub(crate) fn vendor_for_order_line(
        &self,
        product_id: &str,
        company_id: Option<&str>,
    ) -> Option<(String, Option<String>)> {
        if let Some(approval) = self
            .product_approvals
            .iter()
            .find(|entry| entry.value().product_id == product_id)
            .map(|entry| entry.value().clone())
        {
            if approval.status == "approved" {
                return Some((approval.vendor_id, approval.category));
            }
            // A product whose approval is pending or rejected earns no
            // commission for its vendor: the sale is not a marketplace
            // sale until approval.
            return None;
        }
        let company_id = company_id?;
        self.vendors
            .iter()
            .find(|entry| {
                entry.value().status == "approved"
                    && entry
                        .value()
                        .company_id
                        .as_deref()
                        .is_some_and(|id| id == company_id)
            })
            .map(|entry| (entry.key().clone(), None))
    }

    /// Build the structured render data for one table.
    pub(crate) fn render_data(&self, table: &PricingTable) -> PricingTableRender {
        let discount = table.yearly_discount_percent / 100.0;
        let tiers: Vec<TierRender> = self
            .tiers_of(&table.id)
            .into_iter()
            .map(|tier| {
                let yearly_base = tier.monthly_price.saturating_mul(12);
                let yearly_total_discounted =
                    apply_ratio_to_minor(yearly_base, 1.0 - discount);
                let yearly_monthly_equivalent =
                    div_rounded(i128::from(yearly_total_discounted), 12) as i64;
                let yearly_savings = yearly_base - yearly_total_discounted;
                TierRender {
                    features: self.features_of(&tier.id),
                    yearly_total_discounted,
                    yearly_monthly_equivalent,
                    yearly_savings,
                    tier,
                }
            })
            .collect();
        PricingTableRender {
            table: table.clone(),
            tiers,
            default_cycle: "monthly".to_string(),
        }
    }

    /// Aggregate the dashboard for one vendor from the commission
    /// ledger, payouts, and product approvals.
    pub(crate) fn dashboard_for(&self, vendor: &Vendor) -> VendorDashboard {
        let vendor_id = vendor.id.as_str();
        let mut total_orders_set: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut total_sales = 0i64;
        let mut total_commission = 0i64;
        let mut pending_commission = 0i64;
        let mut paid_commission = 0i64;
        let mut reversed_commission = 0i64;
        for entry in self.commissions.iter() {
            let commission = entry.value();
            if commission.vendor_id != vendor_id {
                continue;
            }
            total_orders_set.insert(commission.order_id.clone());
            total_sales += commission.order_total;
            total_commission += commission.commission_amount;
            // A partially settled commission still counts as pending
            // for its unpaid remainder, and the settled portion
            // counts as paid -- so money already paid out is never
            // counted as pending again. A reversed commission keeps
            // the same split: only its unpaid remainder is reversed.
            let settled = commission
                .paid_amount
                .clamp(0, commission.commission_amount);
            match commission.status.as_str() {
                "pending" => {
                    pending_commission += commission.commission_amount - settled;
                    paid_commission += settled;
                }
                "paid" => paid_commission += commission.commission_amount,
                "reversed" => {
                    reversed_commission += commission.commission_amount - settled;
                    paid_commission += settled;
                }
                _ => {}
            }
        }
        let mut payouts_requested = 0i64;
        let mut payouts_approved = 0i64;
        let mut payouts_paid = 0i64;
        let mut payouts_pending_count = 0u64;
        let mut invoice_ids: Vec<String> = Vec::new();
        for entry in self.payouts.iter() {
            let payout = entry.value();
            if payout.vendor_id != vendor_id {
                continue;
            }
            match payout.status.as_str() {
                "requested" => {
                    payouts_requested += payout.amount;
                    payouts_pending_count += 1;
                }
                "approved" => payouts_approved += payout.amount,
                "paid" => payouts_paid += payout.amount,
                _ => {}
            }
            if let Some(invoice_id) = &payout.invoice_id {
                invoice_ids.push(invoice_id.clone());
            }
        }
        invoice_ids.sort();
        invoice_ids.dedup();
        let mut approved_products = 0u64;
        let mut pending_products = 0u64;
        let mut rejected_products = 0u64;
        for entry in self.product_approvals.iter() {
            let approval = entry.value();
            if approval.vendor_id != vendor_id {
                continue;
            }
            match approval.status.as_str() {
                "approved" => approved_products += 1,
                "pending" => pending_products += 1,
                "rejected" => rejected_products += 1,
                _ => {}
            }
        }
        VendorDashboard {
            vendor_id: vendor.id.clone(),
            vendor_status: vendor.status.clone(),
            total_orders: total_orders_set.len() as u64,
            total_sales,
            total_commission,
            pending_commission,
            paid_commission,
            reversed_commission,
            payouts_requested,
            payouts_approved,
            payouts_paid,
            payouts_pending_count,
            approved_products,
            pending_products,
            rejected_products,
            invoice_ids,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::pricing_tables::handlers_marketplace::{
        transition_payout, transition_product_approval,
    };
    use crate::modules::pricing_tables::handlers_vendors::transition_vendor;

    fn test_vendor(status: &str) -> Vendor {
        Vendor {
            id: "v-1".to_string(),
            user_id: "u-1".to_string(),
            company_id: None,
            company_name: "Acme".to_string(),
            slug: "acme".to_string(),
            email: "a@b.co".to_string(),
            status: status.to_string(),
            application_notes: String::new(),
            review_reason: String::new(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn test_approval(status: &str) -> ProductApproval {
        ProductApproval {
            id: "pa-1".to_string(),
            product_id: "p-1".to_string(),
            vendor_id: "v-1".to_string(),
            category: "gold".to_string(),
            status: status.to_string(),
            reason: String::new(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    fn test_payout(status: &str) -> Payout {
        Payout {
            id: "pay-1".to_string(),
            vendor_id: "v-1".to_string(),
            amount: 10000,
            currency: "USD".to_string(),
            status: status.to_string(),
            invoice_id: None,
            reference: "PAY-1".to_string(),
            requested_by: "u-1".to_string(),
            reviewed_by: None,
            review_reason: String::new(),
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn transition_vendor_approves_a_pending_vendor() {
        let state = PricingTablesState::new();
        state.vendors.insert("v-1".to_string(), test_vendor("pending"));
        transition_vendor(
            &state,
            "v-1",
            &["pending"],
            "approved",
            "marketplace.vendor_approved",
            "admin-1",
            "ok",
        )
        .expect("transition succeeds");
        let vendor = state.vendors.get("v-1").expect("vendor still present");
        assert_eq!(vendor.status, "approved");
        assert_eq!(vendor.reviewed_by.as_deref(), Some("admin-1"));
    }

    #[test]
    fn transition_vendor_rejects_a_wrong_starting_status() {
        let state = PricingTablesState::new();
        state.vendors.insert("v-1".to_string(), test_vendor("approved"));
        assert!(transition_vendor(
            &state,
            "v-1",
            &["pending"],
            "approved",
            "marketplace.vendor_approved",
            "admin-1",
            "ok",
        )
        .is_err());
    }

    #[test]
    fn transition_vendor_rejects_an_unknown_vendor() {
        let state = PricingTablesState::new();
        assert!(transition_vendor(
            &state,
            "missing",
            &["pending"],
            "approved",
            "marketplace.vendor_approved",
            "admin-1",
            "ok",
        )
        .is_err());
    }

    #[test]
    fn transition_product_approval_approves_a_pending_approval() {
        let state = PricingTablesState::new();
        state.product_approvals.insert("pa-1".to_string(), test_approval("pending"));
        transition_product_approval(
            &state,
            "pa-1",
            "approved",
            "marketplace.product_approved",
            "admin-1",
            " ok ",
        )
        .expect("transition succeeds");
        let approval = state.product_approvals.get("pa-1").expect("approval still present");
        assert_eq!(approval.status, "approved");
        assert_eq!(approval.reason, "ok");
    }

    #[test]
    fn transition_product_approval_rejects_a_reviewed_approval() {
        let state = PricingTablesState::new();
        state.product_approvals.insert("pa-1".to_string(), test_approval("approved"));
        assert!(transition_product_approval(
            &state,
            "pa-1",
            "approved",
            "marketplace.product_approved",
            "admin-1",
            "ok",
        )
        .is_err());
    }

    #[test]
    fn transition_payout_approves_a_requested_payout() {
        let state = PricingTablesState::new();
        state.payouts.insert("pay-1".to_string(), test_payout("requested"));
        transition_payout(
            &state,
            "pay-1",
            &["requested"],
            "approved",
            "marketplace.payout_approved",
            "admin-1",
            "ok",
        )
        .expect("transition succeeds");
        let payout = state.payouts.get("pay-1").expect("payout still present");
        assert_eq!(payout.status, "approved");
        assert_eq!(payout.reviewed_by.as_deref(), Some("admin-1"));
    }

    #[test]
    fn transition_payout_rejects_a_wrong_starting_status() {
        let state = PricingTablesState::new();
        state.payouts.insert("pay-1".to_string(), test_payout("paid"));
        assert!(transition_payout(
            &state,
            "pay-1",
            &["requested"],
            "approved",
            "marketplace.payout_approved",
            "admin-1",
            "ok",
        )
        .is_err());
    }

    fn test_commission(
        id: &str,
        amount: i64,
        paid_amount: i64,
        status: &str,
    ) -> Commission {
        Commission {
            id: id.to_string(),
            order_id: format!("ord-{id}"),
            vendor_id: "v-1".to_string(),
            product_id: "p-1".to_string(),
            order_total: amount * 10,
            rate_percent: 10.0,
            commission_amount: amount,
            paid_amount,
            currency: "USD".to_string(),
            status: status.to_string(),
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn dashboard_splits_partially_settled_commissions() {
        let state = PricingTablesState::new();
        // Pending with 40 of 100 already settled: 60 stays pending.
        state.commissions.insert(
            "c-pending".to_string(),
            test_commission("c-pending", 10000, 4000, "pending"),
        );
        state
            .commissions
            .insert("c-paid".to_string(), test_commission("c-paid", 5000, 5000, "paid"));
        // Reversed after a partial settlement: only the unpaid 20
        // is reversed; the 10 already paid out stays paid.
        state.commissions.insert(
            "c-reversed".to_string(),
            test_commission("c-reversed", 3000, 1000, "reversed"),
        );
        let dashboard = state.dashboard_for(&test_vendor("approved"));
        assert_eq!(dashboard.pending_commission, 6000);
        assert_eq!(dashboard.paid_commission, 10000);
        assert_eq!(dashboard.reversed_commission, 2000);
        assert_eq!(dashboard.total_commission, 18000);
    }
}
