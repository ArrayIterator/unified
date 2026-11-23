# Email provider logos

One SVG logo per provider (9 existing + 4 added by task 350 = 13,
+ 43 added by task 354 = 56).
The catalog endpoint (`GET /api/v1/email-providers/catalog`) returns
one per provider kind as `"logo": "email-gmail"` — a stem, never a
URL. The demo settings page copies these files to
`demo/assets/logos/` and shows them in the provider picker grid.

## Why these files live here

The module-icon sprite (`assets/icons/modules-sprite.svg`) is built
from module `icon.svg` files for the 150 modules. Provider brands
are not modules, so their logos do not belong in that sprite. They
follow the same convention instead (plugin model: the icon
belongs to the thing it pictures):

- flat solid fills, 24x24 viewBox, at most two colours,
  `var(--icon-primary, #brand)` for the main shape and
  `var(--icon-secondary, #fff)` for the detail;
- no external fetches, no strokes;
- no text elements — EXCEPT the task-354 lettermarks: a rounded
  rect plus one `<text>` element with the provider's initials
  (Arial/Helvetica, bold, 7–8pt). Lettermarks are clean initial
  monograms, not trademarked artwork redrawn in detail; the 13
  original logos already use letterforms as paths. No other text
  elements are allowed;
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

Task 354 added 43 lettermarks (initials on a rounded rect, brand-ish
colours — e.g. Zoho #E42527, Proton #6D4AFF, Yandex #FC3F1D,
Mailjet #FFD100, Loops #FF5C00, OneSignal #E54B4D): 26 mailbox
SMTP presets, 13 dual API/SMTP relays, and 4 generic HTTP providers
(Plunk, Courier, Loops, OneSignal). The colours are approximations,
not trademarked artwork.

## Real brand marks (task 360)

22 files (21 provider ids — `zoho` and `zoho-eu` share one brand)
now use the real brand mark from
[simple-icons](https://github.com/simple-icons/simple-icons),
fetched from the simple-icons CDN (`https://cdn.simpleicons.org/{slug}`)
in the brand's own colour. Simple Icons is released under the
[CC0 1.0 Universal](https://github.com/simple-icons/simple-icons/blob/develop/LICENSE.md)
license. The brand marks themselves are the trademarks of their
respective owners and are used here only to identify their services;
no endorsement is implied.

Normalization applied when saving a brand mark (the artwork itself
is untouched): the brand hex from the CDN response is also set as
the `fill` on the `<path>` itself, because the sprite builder copies
only the children into a `<symbol fill="none">` — a path without an
explicit fill would render invisible in the sprite. Each brand file
keeps `viewBox="0 0 24 24"` and an English `<title>` as its first
child. Brand files contain no `<text>` and no stroke.

Real brand marks (provider id → simple-icons slug):

| Provider id | simple-icons slug |
| :--- | :--- |
| `brevo` | `brevo` |
| `campaign-monitor` | `campaignmonitor` |
| `gandi` | `gandi` |
| `gmail` | `gmail` |
| `gmx` | `gmx` |
| `icloud` | `icloud` |
| `infomaniak` | `infomaniak` |
| `loops` | `loops` |
| `mail-com` | `maildotcom` |
| `mail-ru` | `maildotru` |
| `mailchimp` | `mailchimp` |
| `mailgun` | `mailgun` |
| `mailtrap` | `mailtrap` |
| `migadu` | `migadu` |
| `namecheap` | `namecheap` |
| `ovh` | `ovh` |
| `proton` | `protonmail` |
| `qq-mail` | `qq` |
| `resend` | `resend` |
| `sparkpost` | `sparkpost` |
| `zoho` | `zoho` |
| `zoho-eu` | `zoho` (same brand, EU region) |

## Lettermark fallbacks (task 360)

The remaining 34 providers have no entry in simple-icons, so their
existing lettermark / hand-drawn files were left untouched — the set
stays visually consistent and has no gaps. 27 of these are the
task-354 lettermarks (initials + `<text>`); 7 are the original
hand-drawn marks (`outlook`, `postmark`, `sendgrid`, `sendmail`,
`ses`, `smtp`, `yahoo`).

Lettermark fallbacks (34 ids): `aol`, `courier`, `dreamhost`,
`elastic-email`, `fastmail`, `kolabnow`, `mail-126`, `mail-163`,
`mailersend`, `mailfence`, `mailjet`, `mxroute`, `onesignal`,
`outlook`, `pepipost`, `plunk`, `posteo`, `postmark`, `runbox`,
`sender-net`, `sendgrid`, `sendlayer`, `sendmail`, `sendpulse`,
`ses`, `sina`, `smtp`, `smtp-com`, `smtp2go`, `socketlabs`,
`startmail`, `yahoo`, `yandex`, `zeptomail`.

Checked against the simple-icons slugs table and deliberately NOT
substituted with a near-miss brand: `yandex` (only `yandexcloud`
exists — a different product), `sina` (only `sinaweibo` — Weibo,
not Sina Mail), `elastic-email` (`elastic` is a different brand),
`smtp` / `sendmail` / `smtp2go` / `smtp-com` (no generic SMTP brand
in simple-icons), `ses` (no Amazon SES entry). If simple-icons adds
any of these brands later, the fallback can be swapped for the real
mark following the same fetch-and-normalize steps above.
