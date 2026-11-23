//! Module icons: one SVG sprite for every registered module.
//!
//! The sprite lives in `assets/icons/modules-sprite.svg` and holds one
//! `<symbol id="module-icon-<module>">` per module (142 core modules plus the
//! 5 native external crates) and one `module-icon-placeholder` symbol used
//! as the fallback for valid names without an icon yet. Pages embed the
//! sprite once, then render any icon with [`module_icon_use`]:
//!
//! ```handlebars
//! {{{module_icons_sprite}}}
//! {{{module_icon_use "billing"}}}
//! ```
//!
//! Where an icon comes from (WordPress-plugin model: the icon belongs
//! to the module) — lookup order, implemented by the sprite builder
//! (`scripts/build-module-icons-sprite.py`), which discovers icons by
//! scanning the filesystem, never from a literal module list:
//!   1. module-owned: `<module>/icon.svg` alongside the module code,
//!   2. central: `assets/icons/modules/<module>.svg` (override/fallback),
//!   3. placeholder, when a module has no icon yet.
//!
//! Icons are flat solid fills in the project tracker's language
//! (one semantic colour per meaning, fully rounded, no linework).
//! Their colours are CSS custom properties with fallbacks
//! (`fill="var(--icon-primary, #2563EB)"`), so a parent
//! `[data-icon-theme="grayscale"]` redefines them to grays; see
//! `assets/icons/README.md` for the palette. The `data-icon` attribute
//! on the emitted `<svg>` names the icon actually shown (or
//! `"placeholder"` on fallback). Icons declare their default animation
//! with `data-animation="..."` in their icon file; the builder copies
//! it onto the `<symbol>` and [`module_icon_animation`] reads it back,
//! so [`module_icon_use`] can emit it as `data-icon-animation` —
//! `assets/css/icons.css` only animates under a parent trigger, so
//! animation stays off by default (see `assets/icons/ANIMATION.md`).
//!
//! Lookup rule: an invalid name (not `[a-z0-9-]`, empty, or over
//! 64 characters) renders nothing; a valid name with no symbol yet
//! renders the placeholder. Use [`module_icon_exists`] when a template
//! needs to branch on whether a real icon exists.
//!
//! Icon names are validated to lowercase ASCII letters, digits, and
//! hyphens only, so a caller-supplied name can never break out of the
//! generated markup. An unknown-but-well-formed name renders the
//! placeholder, never invalid HTML.

/// The whole module-icon SVG sprite, embedded at compile time.
///
/// Embed this string once per page (unescaped), ideally right after the
/// opening `<body>` tag, then reference icons with [`module_icon_use`].
pub const MODULE_ICONS_SPRITE: &str = include_str!("../../assets/icons/modules-sprite.svg");

/// The symbol id used when a module has no icon yet.
pub const PLACEHOLDER_ICON: &str = "module-icon-placeholder";

/// Animation names an icon file may declare with `data-animation`.
const KNOWN_ANIMATIONS: [&str; 4] = ["spin", "wiggle", "pulse", "bounce"];

/// True when the sprite holds a real icon for `module`.
///
/// Returns false for invalid names and for valid names without a symbol.
pub fn module_icon_exists(module: &str) -> bool {
    if module.is_empty()
        || module.len() > 64
        || !module
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return false;
    }
    MODULE_ICONS_SPRITE.contains(&format!("id=\"module-icon-{module}\""))
}

/// The default animation an icon declares, if any.
///
/// Icons declare it with `data-animation="..."` on their icon file's
/// `<svg>` root; the sprite builder copies it onto the `<symbol>`.
/// Returns `None` when the icon declares no animation or does not exist.
pub fn module_icon_animation(module: &str) -> Option<&str> {
    if !module_icon_exists(module) {
        return None;
    }
    let id = format!("id=\"module-icon-{module}\"");
    let start = MODULE_ICONS_SPRITE.find(id.as_str())?;
    let rest = &MODULE_ICONS_SPRITE[start..];
    let tag_end = rest.find('>')?;
    let tag = &rest[..tag_end];
    let key = "data-animation=\"";
    let value_start = tag.find(key)? + key.len();
    let value = tag[value_start..].split('"').next()?;
    if KNOWN_ANIMATIONS.contains(&value) {
        Some(value)
    } else {
        None
    }
}

/// Render the inline `<svg><use>` markup for one module icon.
///
/// `module` is the kebab-case module name, e.g. `"billing"` or
/// `"external-whatsapp"`. Returns an empty string when the name is
/// invalid (anything outside `[a-z0-9-]`, empty, or over 64 characters),
/// so untrusted input can never inject markup here. A valid name with
/// no icon yet renders the placeholder symbol instead, with
/// `data-icon="placeholder"`. When the icon declares a default
/// animation it is emitted as `data-icon-animation`; the CSS only
/// animates it under a parent trigger (`.icon-animated`, `:hover`, or
/// an explicit parent `data-icon-animation`).
pub fn module_icon_use(module: &str) -> String {
    if module.is_empty()
        || module.len() > 64
        || !module
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return String::new();
    }
    let (id, shown) = if module_icon_exists(module) {
        (format!("module-icon-{module}"), module.to_string())
    } else {
        (PLACEHOLDER_ICON.to_string(), "placeholder".to_string())
    };
    let anim_attr = module_icon_animation(module)
        .map(|anim| format!(" data-icon-animation=\"{anim}\""))
        .unwrap_or_default();
    format!(
        "<svg class=\"module-icon\" data-icon=\"{shown}\"{anim_attr} aria-hidden=\"true\"><use href=\"#{id}\"></use></svg>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sprite_contains_all_expected_symbols() {
        // Spot-check a few symbols across core modules and external crates,
        // plus the placeholder fallback symbol. These are test spot-checks
        // only; resolution itself never uses a literal module list.
        for id in [
            "module-icon-ai",
            "module-icon-billing",
            "module-icon-store",
            "module-icon-security",
            "module-icon-external-whatsapp",
            "module-icon-external-bi",
            "module-icon-placeholder",
        ] {
            assert!(
                MODULE_ICONS_SPRITE.contains(&format!("id=\"{id}\"")),
                "sprite is missing symbol {id}"
            );
        }
    }

    #[test]
    fn icon_use_renders_expected_markup() {
        assert_eq!(
            module_icon_use("billing"),
            "<svg class=\"module-icon\" data-icon=\"billing\" aria-hidden=\"true\"><use href=\"#module-icon-billing\"></use></svg>"
        );
    }

    #[test]
    fn icon_use_falls_back_to_placeholder() {
        assert_eq!(
            module_icon_use("not-a-real-module"),
            "<svg class=\"module-icon\" data-icon=\"placeholder\" aria-hidden=\"true\"><use href=\"#module-icon-placeholder\"></use></svg>"
        );
    }

    #[test]
    fn icon_exists_reports_correctly() {
        assert!(module_icon_exists("billing"));
        assert!(module_icon_exists("external-whatsapp"));
        assert!(!module_icon_exists("not-a-real-module"));
        assert!(!module_icon_exists("Billing"));
        assert!(!module_icon_exists(""));
    }

    #[test]
    fn icon_animation_comes_from_the_icon_file() {
        // app-sync declares data-animation="spin" in its icon file;
        // billing declares none.
        assert_eq!(module_icon_animation("app-sync"), Some("spin"));
        assert_eq!(module_icon_animation("billing"), None);
        assert_eq!(module_icon_animation("not-a-real-module"), None);
    }

    #[test]
    fn icon_use_rejects_unsafe_names() {
        for bad in [
            "",
            "Billing",
            "billing\" onload=\"alert(1)",
            "billing</svg><script>alert(1)</script>",
            "a b",
            "billing.svg",
            "x".repeat(65).as_str(),
        ] {
            assert_eq!(module_icon_use(bad), "", "name {bad:?} must be rejected");
        }
    }
}
