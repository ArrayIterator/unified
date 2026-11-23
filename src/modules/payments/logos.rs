//! payments - provider, card-scheme, and method logos.
//!
//! One SVG logo per payment provider / card scheme / payment method,
//! in `logos/` next to this file. The convention follows the email
//! provider logos (`src/modules/email_providers/logos/`): flat solid
//! fills, 24x24, at most two colours (`var(--icon-primary, #brand)`
//! + `var(--icon-secondary, #fff)`), no external fetches, no strokes.
//! The files are the source of truth; the sprite (`logos/sprite.svg`,
//! symbols `provider-logo-payment-<id>`) is generated from them by
//! `scripts/build-provider-logos-sprite.py --src src/modules/payments/logos
//! --glob 'payment-*.svg'` — re-run that script after adding or
//! changing a logo.
//!
//! Three groups live in this directory:
//!
//! * **Providers** (12 `PaymentProviderKind`): the catalog returns the
//!   stem only (`"logo": "payment-paypal"`), never a URL.
//! * **Card schemes** (5): Visa, Mastercard, American Express,
//!   Discover, Diners Club, JCB — real brand marks from simple-icons.
//!   Mastercard is both a provider kind and a scheme; its file serves
//!   both roles.
//! * **Indonesian payment methods** (8): DANA, OVO, GoPay, QRIS,
//!   ShopeePay, LinkAja, Indomaret, Alfamart. These are METHODS
//!   offered through Midtrans/Xendit, not providers — none of them
//!   has a simple-icons entry (verified 404 on the current
//!   simple-icons release), so they are lettermark monograms in the
//!   same style as the provider fallbacks, not official artwork.
//!
//! Lettermark exception (same as email): providers and methods without
//! a simple-icons entry use a rounded rect plus a `<text>` element
//! with initials (Arial/Helvetica, bold, 7–8pt). That is the ONE
//! deviation from the "no text elements" rule: lettermarks are clean
//! initial monograms, not trademarked artwork redrawn in detail.
//! No other text elements are allowed in this directory.
//!
//! Templates embed the sprite once (unescaped), then render a logo
//! with [`payment_logo_use`]:
//!
//! ```handlebars
//! {{{payment_logos_sprite}}}
//! {{{payment_logo_use "payment-paypal"}}}
//! ```

/// The whole payment-logo SVG sprite, embedded at compile time.
///
/// Embed this string once per page (unescaped), ideally right after
/// the opening `<body>` tag, then reference logos with
/// [`payment_logo_use`].
pub const PAYMENT_LOGOS_SPRITE: &str = include_str!("logos/sprite.svg");

/// True when the sprite holds a real logo for `logo_id` (a stem
/// such as `"payment-paypal"`).
///
/// Returns false for invalid ids and for valid ids without a logo.
pub fn payment_logo_exists(logo_id: &str) -> bool {
    if logo_id.is_empty()
        || logo_id.len() > 64
        || !logo_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return false;
    }
    PAYMENT_LOGOS_SPRITE.contains(&format!("id=\"provider-logo-{logo_id}\""))
}

/// Render one payment logo as an inline `<svg><use .../></svg>`.
///
/// The `data-logo` attribute on the emitted `<svg>` names the logo
/// shown. Returns an empty string for an invalid id or a missing
/// logo — brand logos have no placeholder (unlike module icons,
/// where an unknown-but-valid name renders the placeholder).
pub fn payment_logo_use(logo_id: &str) -> String {
    if !payment_logo_exists(logo_id) {
        return String::new();
    }
    format!(
        "<svg class=\"provider-logo\" aria-hidden=\"true\" data-logo=\"{logo_id}\"><use href=\"#provider-logo-{logo_id}\"></use></svg>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_holds_all_payment_logos() {
        for logo_id in [
            // 12 provider kinds.
            "payment-xendit",
            "payment-hyperpay",
            "payment-twocheckout",
            "payment-card",
            "payment-mastercard",
            "payment-googlepay",
            "payment-applepay",
            "payment-paypal",
            "payment-stripe",
            "payment-midtrans",
            "payment-custom",
            "payment-banktransfer",
            // 5 card schemes.
            "payment-visa",
            "payment-americanexpress",
            "payment-discover",
            "payment-dinersclub",
            "payment-jcb",
            // 8 Indonesian payment methods.
            "payment-dana",
            "payment-ovo",
            "payment-gopay",
            "payment-qris",
            "payment-shopeepay",
            "payment-linkaja",
            "payment-indomaret",
            "payment-alfamart",
        ] {
            assert!(
                payment_logo_exists(logo_id),
                "logo {logo_id} missing from the sprite"
            );
            let rendered = payment_logo_use(logo_id);
            assert!(rendered.contains(&format!("#provider-logo-{logo_id}")));
        }
    }

    #[test]
    fn invalid_or_missing_logos_render_nothing() {
        assert!(!payment_logo_exists(""));
        assert!(!payment_logo_exists("Payment-PayPal"));
        assert!(!payment_logo_exists("payment_paypal"));
        assert!(!payment_logo_exists("payment-no-such-provider"));
        assert!(payment_logo_use("payment-no-such-provider").is_empty());
        assert!(payment_logo_use("Bad Id").is_empty());
    }
}
