//! email_providers - provider logos.
//!
//! One SVG logo per provider, in `logos/` next to this file. The
//! convention follows the module-icon sprite (`assets/icons/`): flat
//! solid fills, 24x24, at most two colours
//! (`var(--icon-primary, #brand)` + `var(--icon-secondary, #fff)`),
//! no external fetches, no strokes. The files are the source of
//! truth; the sprite (`logos/sprite.svg`, symbols
//! `provider-logo-email-<id>`) is generated from them by
//! `scripts/build-provider-logos-sprite.py` - re-run that script
//! after adding or changing a logo.
//!
//! Lettermark exception: the 43 providers added in task
//! 354 use simple lettermark SVGs - a rounded rect plus a `<text>`
//! element with the provider's initials (Arial/Helvetica, bold,
//! 7-8pt). That is the ONE deviation from the "no text elements"
//! rule: lettermarks are clean initial monograms, not trademarked
//! artwork redrawn in detail, and the 13 original logos already
//! use letterforms as paths (Yahoo Y!, Mailchimp M). No other text
//! elements are allowed in this directory.
//!
//! Why here and not in the module-icon sprite: that sprite is
//! generated from module `icon.svg` files for the 150 modules, and
//! provider brands are not modules. A provider logo belongs to the
//! it lives with the module code and the catalog returns only its
//! stem (`"logo": "email-gmail"`), never a URL.
//!
//! Templates embed the sprite once (unescaped), then render a logo
//! with [`provider_logo_use`]:
//!
//! ```handlebars
//! {{{provider_logos_sprite}}}
//! {{{provider_logo_use "email-gmail"}}}
//! ```

/// The whole provider-logo SVG sprite, embedded at compile time.
///
/// Embed this string once per page (unescaped), ideally right after
/// the opening `<body>` tag, then reference logos with
/// [`provider_logo_use`].
pub const PROVIDER_LOGOS_SPRITE: &str = include_str!("logos/sprite.svg");

/// True when the sprite holds a real logo for `logo_id` (a stem
/// such as `"email-gmail"`).
///
/// Returns false for invalid ids and for valid ids without a logo.
pub fn provider_logo_exists(logo_id: &str) -> bool {
    if logo_id.is_empty()
        || logo_id.len() > 64
        || !logo_id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return false;
    }
    PROVIDER_LOGOS_SPRITE.contains(&format!("id=\"provider-logo-{logo_id}\""))
}

/// Render one provider logo as an inline `<svg><use .../></svg>`.
///
/// The `data-logo` attribute on the emitted `<svg>` names the logo
/// shown. Returns an empty string for an invalid id or a missing
/// logo - brand logos have no placeholder (unlike module icons,
/// where an unknown-but-valid name renders the placeholder).
pub fn provider_logo_use(logo_id: &str) -> String {
    if !provider_logo_exists(logo_id) {
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
    fn sprite_holds_all_fifty_six_provider_logos() {
        for logo_id in [
            "email-mailchimp",
            "email-brevo",
            "email-sendgrid",
            "email-mailgun",
            "email-postmark",
            "email-ses",
            "email-resend",
            "email-sparkpost",
            "email-smtp",
            "email-gmail",
            "email-yahoo",
            "email-outlook",
            "email-sendmail",
            // 26 mailbox SMTP presets.
            "email-zoho",
            "email-zoho-eu",
            "email-proton",
            "email-yandex",
            "email-fastmail",
            "email-icloud",
            "email-gmx",
            "email-mail-com",
            "email-aol",
            "email-qq-mail",
            "email-mail-163",
            "email-mail-126",
            "email-sina",
            "email-mail-ru",
            "email-posteo",
            "email-migadu",
            "email-mailfence",
            "email-startmail",
            "email-runbox",
            "email-kolabnow",
            "email-infomaniak",
            "email-ovh",
            "email-gandi",
            "email-namecheap",
            "email-dreamhost",
            "email-mxroute",
            // 13 dual API/SMTP relays via SMTP.
            "email-sender-net",
            "email-sendpulse",
            "email-mailjet",
            "email-mailtrap",
            "email-elastic-email",
            "email-smtp2go",
            "email-mailersend",
            "email-zeptomail",
            "email-sendlayer",
            "email-socketlabs",
            "email-smtp-com",
            "email-campaign-monitor",
            // 4 generic HTTP providers.
            "email-plunk",
            "email-courier",
            "email-loops",
            "email-onesignal",
        ] {
            assert!(
                provider_logo_exists(logo_id),
                "logo {logo_id} missing from the sprite"
            );
            let rendered = provider_logo_use(logo_id);
            assert!(rendered.contains(&format!("#provider-logo-{logo_id}")));
        }
    }

    #[test]
    fn invalid_or_missing_logos_render_nothing() {
        assert!(!provider_logo_exists(""));
        assert!(!provider_logo_exists("Email-Gmail"));
        assert!(!provider_logo_exists("email_gmail"));
        assert!(!provider_logo_exists("email-no-such-provider"));
        assert!(provider_logo_use("email-no-such-provider").is_empty());
        assert!(provider_logo_use("Bad Id").is_empty());
    }
}
