# Email provider logos

One SVG logo per provider (9 existing + 4 added by task 350 = 13).
The catalog endpoint (`GET /api/v1/email-providers/catalog`) returns
one per provider kind as `"logo": "email-gmail"` — a stem, never a
URL. The demo settings page copies these files to
`demo/assets/logos/` and shows them in the provider picker grid.

## Why these files live here

The module-icon sprite (`assets/icons/modules-sprite.svg`) is built
from module `icon.svg` files for the 150 modules. Provider brands
are not modules, so their logos do not belong in that sprite. They
follow the same convention instead (WordPress-plugin model: the icon
belongs to the thing it pictures):

- flat solid fills, 24x24 viewBox, at most two colours,
  `var(--icon-primary, #brand)` for the main shape and
  `var(--icon-secondary, #fff)` for the detail;
- no external fetches, no text elements, no strokes;
- the files in this directory are the source of truth.

## Sprite

`sprite.svg` is generated from the files in this directory by:

```
python3 scripts/build-provider-logos-sprite.py
```

One `<symbol id="provider-logo-email-<id>" viewBox="0 0 24 24">`
per logo, sorted by id. Re-run the script after adding or changing
a logo. `logos.rs` embeds the sprite at compile time
(`PROVIDER_LOGOS_SPRITE`) and renders one logo with
`provider_logo_use("email-gmail")`. A missing logo renders nothing
— brand logos have no placeholder, unlike module icons.

## The marks

Simple, clean, single-colour-friendly marks (no trademark artwork
redrawn in detail): envelopes for the SMTP family (Gmail's flap
forms an M), letterforms for Yahoo (Y!), Mailchimp (M), Brevo (B)
and Mailgun (M), a paper plane for SendGrid, a stamp for Postmark,
a cloud for SES, a send arrow for Resend, a spark for SparkPost,
and a terminal prompt on the envelope for local sendmail.
