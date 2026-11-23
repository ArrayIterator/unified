//! In-memory product store and its query helpers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus};
use unified_core::json;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::collections::{HashSet};
use std::sync::{Arc, RwLock};
use super::money::{format_minor, mul_div_round_half_up};
use super::types::{BundleItem, Inventory, InventorySummary, PriceQuote, PriceQuoteRequest, PriceTier, Product, ProductDetail, ProductFile, ProductOption, ProductVariant, ProductVersion, RegionalPrice, Tag, TaxRule, TaxonomyNode, UsagePrice};

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct ProductsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) products: DashMap<String, Product>,
    pub(crate) options: DashMap<String, ProductOption>,
    pub(crate) variants: DashMap<String, ProductVariant>,
    pub(crate) bundle_items: DashMap<String, BundleItem>,
    pub(crate) tiers: DashMap<String, PriceTier>,
    pub(crate) regional_prices: DashMap<String, RegionalPrice>,
    pub(crate) usage_prices: DashMap<String, UsagePrice>,
    pub(crate) tax_rules: DashMap<String, TaxRule>,
    pub(crate) inventory: DashMap<String, Inventory>,
    pub(crate) files: DashMap<String, ProductFile>,
    pub(crate) versions: DashMap<String, ProductVersion>,
    pub(crate) taxonomy: DashMap<String, TaxonomyNode>,
    pub(crate) tags: DashMap<String, Tag>,
    /// product id -> assigned tag ids.
    pub(crate) product_tags: DashMap<String, HashSet<String>>,
}

impl ProductsState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            products: DashMap::new(),
            options: DashMap::new(),
            variants: DashMap::new(),
            bundle_items: DashMap::new(),
            tiers: DashMap::new(),
            regional_prices: DashMap::new(),
            usage_prices: DashMap::new(),
            tax_rules: DashMap::new(),
            inventory: DashMap::new(),
            files: DashMap::new(),
            versions: DashMap::new(),
            taxonomy: DashMap::new(),
            tags: DashMap::new(),
            product_tags: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    pub(crate) fn product_detail(&self, product: &Product) -> ProductDetail {
        let tag_ids = self
            .product_tags
            .get(&product.id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let mut tags: Vec<Tag> = tag_ids
            .iter()
            .filter_map(|id| self.tags.get(id).map(|entry| entry.value().clone()))
            .collect();
        tags.sort_by(|a, b| a.name.cmp(&b.name));
        let variant_count = self
            .variants
            .iter()
            .filter(|entry| entry.value().product_id == product.id)
            .count() as u64;
        let tier_count = self
            .tiers
            .iter()
            .filter(|entry| entry.value().product_id == product.id)
            .count() as u64;
        let bundle_item_count = self
            .bundle_items
            .iter()
            .filter(|entry| entry.value().bundle_product_id == product.id)
            .count() as u64;
        let inventory = if product.kind == "physical" {
            let records: Vec<&Inventory> = self
                .inventory
                .iter()
                .filter(|entry| entry.value().product_id == product.id)
                .map(|entry| entry.value())
                .collect();
            if records.is_empty() {
                None
            } else {
                let total_quantity: i64 = records.iter().map(|record| record.quantity).sum();
                let total_reserved: i64 = records.iter().map(|record| record.reserved).sum();
                let low_stock = records
                    .iter()
                    .any(|record| record.quantity - record.reserved <= record.low_stock_threshold);
                Some(InventorySummary {
                    total_quantity,
                    total_reserved,
                    total_available: total_quantity - total_reserved,
                    low_stock,
                })
            }
        } else {
            None
        };
        ProductDetail {
            price_major: format_minor(product.price.amount_minor, &product.price.currency),
            product: product.clone(),
            tags,
            variant_count,
            tier_count,
            bundle_item_count,
            inventory,
        }
    }

    /// True when the referenced company exists. When no
    /// `company.exists` handler is registered the reference is
    /// accepted unchecked, because this module cannot see the
    /// company store directly. Mirrors the software module.
    pub(crate) fn vendor_exists(&self, company_id: &str) -> bool {
        if !self.bus().has("company.exists") {
            return true;
        }
        let context = self.bus().emit(
            "company.exists",
            json!({ "company_id": company_id }),
        );
        context.results.iter().any(|result| {
            result.get("exists").and_then(|v| v.as_bool()).unwrap_or(false)
        })
    }

    /// Answer the entitlement question for one user and product.
    ///
    /// The real check path is the store's entitlement data, asked
    /// through the `store.entitlement.check` hook with payload
    /// `{"product_id", "user_id"}`. No handler for that event exists
    /// in the store module today (its `register_hooks` only listens
    /// to `software.release.published`, payment, and bank-transfer
    /// events), so until the store (or another module) answers it,
    /// this honestly reports `entitled: false` with reason
    /// `no_entitlement_source` instead of guessing from local data.
    /// A result counts as entitled when it carries `"entitled": true`
    /// or an `entitlements` array containing an `active` entry.
    pub(crate) fn check_entitlement(&self, product_id: &str, user_id: &str) -> Value {
        if !self.products.contains_key(product_id) {
            return json!({
                "entitled": false,
                "product_id": product_id,
                "user_id": user_id,
                "source": "none",
                "reason": "product_not_found",
            });
        }
        if !self.bus().has("store.entitlement.check") {
            return json!({
                "entitled": false,
                "product_id": product_id,
                "user_id": user_id,
                "source": "none",
                "reason": "no_entitlement_source",
            });
        }
        let context = self.bus().emit(
            "store.entitlement.check",
            json!({ "product_id": product_id, "user_id": user_id }),
        );
        let entitled = context.results.iter().any(|result| {
            result.get("entitled").and_then(|v| v.as_bool()).unwrap_or(false)
                || result
                    .get("entitlements")
                    .and_then(|v| v.as_array())
                    .is_some_and(|items| {
                        items.iter().any(|item| {
                            item.get("status").and_then(|v| v.as_str()) == Some("active")
                        })
                    })
        });
        json!({
            "entitled": entitled,
            "product_id": product_id,
            "user_id": user_id,
            "source": "store.entitlement.check",
            "reason": if entitled { "entitled" } else { "no_active_entitlement" },
        })
    }

    /// Compute the exact price quote billing needs for an invoice
    /// line. All arithmetic is integer minor units; tax is exclusive
    /// and rounded half-up from the configured tax rule.
    pub(crate) fn quote_price(
        &self,
        product: &Product,
        request: &PriceQuoteRequest,
    ) -> UnifiedResult<PriceQuote> {
        // A quantity of zero is rejected instead of silently becoming
        // 1: quoting zero units must never look like quoting one.
        let quantity = match request.quantity {
            Some(0) => {
                return Err(UnifiedError::bad_request(
                    "Quantity must be at least 1",
                ))
            }
            Some(quantity) => quantity,
            None => 1,
        };
        let currency = product.price.currency.clone();
        // A tier, when named, must belong to this product and be active.
        let tier = match &request.tier_id {
            Some(tier_id) => {
                let tier = self
                    .tiers
                    .get(tier_id.as_str())
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found("Pricing tier not found"))?;
                if tier.product_id != product.id {
                    return Err(UnifiedError::bad_request(
                        "Pricing tier does not belong to this product",
                    ));
                }
                if tier.status != "active" {
                    return Err(UnifiedError::unprocessable("Pricing tier is disabled"));
                }
                if tier.price.currency != currency {
                    return Err(UnifiedError::bad_request(
                        "Pricing tier currency does not match the product currency",
                    ));
                }
                Some(tier)
            }
            None => None,
        };
        // A variant, when named, must belong to this product and be active.
        let variant = match &request.variant_id {
            Some(variant_id) => {
                let variant = self
                    .variants
                    .get(variant_id.as_str())
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found("Product variant not found"))?;
                if variant.product_id != product.id {
                    return Err(UnifiedError::bad_request(
                        "Product variant does not belong to this product",
                    ));
                }
                if variant.status != "active" {
                    return Err(UnifiedError::unprocessable("Product variant is disabled"));
                }
                Some(variant)
            }
            None => None,
        };
        // Regional price, when a region is named and one exists.
        let regional = request.region.as_ref().and_then(|region| {
            self.regional_prices.iter().find_map(|entry| {
                let price = entry.value();
                if price.product_id == product.id
                    && price.region.eq_ignore_ascii_case(region)
                {
                    Some(price.clone())
                } else {
                    None
                }
            })
        });
        // A regional price in a currency other than the product's is
        // rejected, exactly like a mismatched tier currency above:
        // without this the quote would silently switch currency.
        if tier.is_none() {
            if let Some(regional) = &regional {
                if regional.price.currency != currency {
                    return Err(UnifiedError::bad_request(
                        "Regional price currency does not match the product currency",
                    ));
                }
            }
        }
        let mut unit_price_minor: i64 = match (&tier, &regional) {
            (Some(tier), _) => tier.price.amount_minor,
            (None, Some(regional)) => regional.price.amount_minor,
            (None, None) => product.price.amount_minor,
        };
        let mut quote_currency = match (&tier, &regional) {
            (Some(tier), _) => tier.price.currency.clone(),
            (None, Some(regional)) => regional.price.currency.clone(),
            (None, None) => currency.clone(),
        };
        // Bundle pricing replaces the unit price with the rule result.
        if product.kind == "bundle" && tier.is_none() {
            unit_price_minor = self.bundle_unit_price(product)?;
            quote_currency = currency.clone();
        }
        // The variant delta applies on top of the unit price.
        if let Some(variant) = &variant {
            unit_price_minor = unit_price_minor
                .checked_add(variant.price_delta_minor)
                .ok_or_else(|| UnifiedError::bad_request("Variant price overflows"))?;
            if unit_price_minor < 0 {
                return Err(UnifiedError::bad_request(
                    "Variant price delta makes the price negative",
                ));
            }
        }
        // Usage pricing: the base price plus metered units above the
        // included allowance, at the first usage price of the product.
        let mut subtotal_minor: i64 = unit_price_minor
            .checked_mul(quantity as i64)
            .ok_or_else(|| UnifiedError::bad_request("Quantity makes the price overflow"))?;
        if product.pricing_model == "usage" {
            let usage = self
                .usage_prices
                .iter()
                .find(|entry| entry.value().product_id == product.id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::unprocessable(
                        "Usage-priced product has no usage price configured",
                    )
                })?;
            if usage.currency != quote_currency {
                return Err(UnifiedError::bad_request(
                    "Usage price currency does not match the product currency",
                ));
            }
            let units = request.usage_units.unwrap_or(1);
            let billable = units.saturating_sub(usage.included_units);
            let usage_total = (usage.unit_price_minor as i128 * billable as i128)
                .clamp(i64::MIN as i128, i64::MAX as i128) as i64;
            subtotal_minor = subtotal_minor
                .checked_add(usage_total)
                .ok_or_else(|| UnifiedError::bad_request("Usage price overflows"))?;
        }
        let tax_rate_bps = self.tax_rate_bps_for(request.region.as_deref());
        let tax_minor = mul_div_round_half_up(subtotal_minor, tax_rate_bps, 10_000);
        let total_minor = subtotal_minor
            .checked_add(tax_minor)
            .ok_or_else(|| UnifiedError::bad_request("Tax makes the price overflow"))?;
        Ok(PriceQuote {
            product_id: product.id.clone(),
            tier_id: request.tier_id.clone(),
            variant_id: request.variant_id.clone(),
            region: request.region.clone(),
            currency: quote_currency.clone(),
            quantity,
            unit_price_minor,
            unit_price: format_minor(unit_price_minor, &quote_currency),
            subtotal_minor,
            subtotal: format_minor(subtotal_minor, &quote_currency),
            tax_rate_bps,
            tax_minor,
            tax: format_minor(tax_minor, &quote_currency),
            total_minor,
            total: format_minor(total_minor, &quote_currency),
        })
    }

    /// Unit price of a bundle under its pricing rule, from the
    /// children's base prices (or per-item overrides).
    pub(crate) fn bundle_unit_price(&self, product: &Product) -> UnifiedResult<i64> {
        let items: Vec<BundleItem> = self
            .bundle_items
            .iter()
            .filter(|entry| entry.value().bundle_product_id == product.id)
            .map(|entry| entry.value().clone())
            .collect();
        if items.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Bundle product has no bundle items",
            ));
        }
        let mut sum: i64 = 0;
        for item in &items {
            let child = self
                .products
                .get(&item.child_product_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::internal("Bundle item references a missing product")
                })?;
            if child.price.currency != product.price.currency {
                return Err(UnifiedError::bad_request(
                    "Bundle child currency does not match the bundle currency",
                ));
            }
            let unit = item
                .price_override_minor
                .unwrap_or(child.price.amount_minor);
            let line = unit
                .checked_mul(item.quantity as i64)
                .ok_or_else(|| UnifiedError::bad_request("Bundle price overflows"))?;
            sum = sum
                .checked_add(line)
                .ok_or_else(|| UnifiedError::bad_request("Bundle price overflows"))?;
        }
        match product.bundle_rule.as_str() {
            "sum" => Ok(sum),
            "fixed" => Ok(product.price.amount_minor),
            "discount_percent" => {
                let discount_bps = product.bundle_discount_bps.unwrap_or(0);
                Ok(sum - mul_div_round_half_up(sum, discount_bps, 10_000))
            }
            _ => Err(UnifiedError::internal("Bundle product has an invalid pricing rule")),
        }
    }

    /// Tax rate in basis points for a region: the exact country rule
    /// first, then the `GLOBAL` fallback, then zero.
    pub(crate) fn tax_rate_bps_for(&self, region: Option<&str>) -> i64 {
        if let Some(region) = region {
            if let Some(rule) = self.tax_rules.iter().find(|entry| {
                entry.value().country.eq_ignore_ascii_case(region)
            }) {
                return rule.value().rate_bps;
            }
        }
        self.tax_rules
            .iter()
            .find(|entry| entry.value().country == "GLOBAL")
            .map(|entry| entry.value().rate_bps)
            .unwrap_or(0)
    }

    /// True when adding `child_id` to `bundle_id` would create a
    /// bundle cycle (the child already contains the bundle,
    /// directly or transitively).
    pub(crate) fn bundle_would_cycle(&self, bundle_id: &str, child_id: &str) -> bool {
        if bundle_id == child_id {
            return true;
        }
        let mut stack: Vec<String> = vec![child_id.to_string()];
        let mut seen: HashSet<String> = HashSet::new();
        while let Some(current) = stack.pop() {
            if current == bundle_id {
                return true;
            }
            if !seen.insert(current.clone()) {
                continue;
            }
            for entry in self.bundle_items.iter() {
                if entry.value().bundle_product_id == current {
                    stack.push(entry.value().child_product_id.clone());
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::products::money::Money;
    use crate::modules::products::types::LicenseTemplate;
    use chrono::Utc;

    fn test_product() -> Product {
        Product {
            id: "prod-1".to_string(),
            slug: "test-product".to_string(),
            name: "Test Product".to_string(),
            description: String::new(),
            kind: "digital".to_string(),
            status: "active".to_string(),
            pricing_model: "one_time".to_string(),
            billing_period: None,
            price: Money {
                amount_minor: 1000,
                currency: "USD".to_string(),
            },
            renewal_price: None,
            trial_days: 0,
            proration_policy: "prorate".to_string(),
            grace_period_days: 0,
            dunning_max_retries: 3,
            dunning_retry_interval_days: 3,
            vendor_company_id: None,
            group_id: None,
            category_id: None,
            software_id: None,
            license_template: LicenseTemplate::default(),
            support_department_id: None,
            support_department_name: None,
            bundle_rule: "sum".to_string(),
            bundle_discount_bps: None,
            listed: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn quote_request(quantity: Option<u32>, region: Option<&str>) -> PriceQuoteRequest {
        PriceQuoteRequest {
            tier_id: None,
            variant_id: None,
            quantity,
            region: region.map(str::to_string),
            usage_units: None,
        }
    }

    #[test]
    fn quote_price_rejects_a_zero_quantity() {
        let state = ProductsState::new();
        let product = test_product();
        let error = state
            .quote_price(&product, &quote_request(Some(0), None))
            .unwrap_err();
        assert!(error.message.contains("Quantity must be at least 1"));
    }

    #[test]
    fn quote_price_defaults_to_a_quantity_of_one() {
        let state = ProductsState::new();
        let product = test_product();
        let quote = state
            .quote_price(&product, &quote_request(None, None))
            .unwrap();
        assert_eq!(quote.quantity, 1);
        assert_eq!(quote.unit_price_minor, 1000);
    }

    #[test]
    fn quote_price_rejects_a_regional_price_in_another_currency() {
        let state = ProductsState::new();
        let product = test_product();
        state.regional_prices.insert(
            "reg-1".to_string(),
            RegionalPrice {
                id: "reg-1".to_string(),
                product_id: product.id.clone(),
                region: "DE".to_string(),
                price: Money {
                    amount_minor: 900,
                    currency: "EUR".to_string(),
                },
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        );
        let error = state
            .quote_price(&product, &quote_request(None, Some("DE")))
            .unwrap_err();
        assert!(error
            .message
            .contains("Regional price currency does not match the product currency"));
    }

    #[test]
    fn quote_price_uses_a_matching_regional_price() {
        let state = ProductsState::new();
        let product = test_product();
        state.regional_prices.insert(
            "reg-1".to_string(),
            RegionalPrice {
                id: "reg-1".to_string(),
                product_id: product.id.clone(),
                region: "DE".to_string(),
                price: Money {
                    amount_minor: 800,
                    currency: "USD".to_string(),
                },
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        );
        let quote = state
            .quote_price(&product, &quote_request(None, Some("DE")))
            .unwrap();
        assert_eq!(quote.unit_price_minor, 800);
        assert_eq!(quote.currency, "USD");
    }
}
