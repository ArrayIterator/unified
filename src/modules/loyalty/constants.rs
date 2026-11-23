//! Loyalty - limits, capability and event names.
//!
//! Part of the `loyalty` module split; see `src/modules/loyalty.rs`.

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_REASON_LEN: usize = 1000;

pub(crate) const MAX_USER_ID_LEN: usize = 128;

pub(crate) const MAX_COMPANY_ID_LEN: usize = 128;

pub(crate) const MAX_PRODUCT_ID_LEN: usize = 128;

pub(crate) const MAX_ORDER_ID_LEN: usize = 200;

pub(crate) const MAX_POINTS_NAME_LEN: usize = 64;

/// Default currency value of one point when a program does not set
/// its own conversion rate.
pub(crate) const DEFAULT_DISCOUNT_PER_POINT: f64 = 0.01;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_LOYALTY_VIEW: &str = "loyalty.view";

pub(crate) const PERM_LOYALTY_MANAGE: &str = "loyalty.manage";

/// Largest single points award. Computed awards are clamped to
/// this value so a huge or absurd order total can never award
/// `i64::MAX` points through an `f64 as i64` saturation cast.
pub(crate) const MAX_AWARD_POINTS: i64 = i64::MAX / 2;

/// Hook event names this module listens to (literals repeated here
/// so the module stays decoupled from the users / store modules).
pub(crate) const USER_CREATED: &str = "user.created";

pub(crate) const STORE_ORDER_COMPLETED: &str = "store.order.completed";

/// Store-side confirmation of a redeemed discount. The store
/// module does not emit these yet (follow-up: the store must emit
/// `store.discount.applied` with `redemption_id` once it applies
/// the discount to the order, or `store.discount.failed` when it
/// cannot); until then a discount redemption stays `pending` and
/// is only completed by this listener or cancelled (refunded) by a
/// manager.
pub(crate) const STORE_DISCOUNT_APPLIED: &str = "store.discount.applied";

pub(crate) const STORE_DISCOUNT_FAILED: &str = "store.discount.failed";
