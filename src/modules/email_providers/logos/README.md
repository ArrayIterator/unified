# Email provider logos

One SVG logo per provider (9 existing + 4 added by task 350 = 13,
+ 43 added by task 354 = 56, − 1 removed by task 381 = 55).
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

## Real brand marks (task 381)

13 more files now use the real brand mark, copied from TrayDigita
Core (`~/workspace/traydigita-core-git/assets/images/providers/`,
repo ArrayIterator/traydigita-core-plugin, head 3281f60) and
normalized to this directory's convention: `viewBox="0 0 24 24"`,
an English `<title>` as first child, every shape with its own
explicit fill, and the source artwork kept verbatim inside one
`<g transform="...">` that maps the source viewBox into the 24 box
(uniform scale, centered). Gradient and clipPath ids are prefixed
with the provider stem (e.g. `courier-g`) so they cannot collide
inside `sprite.svg`. The normalization script is kept outside the
repo at `~/workspace/unified-check/normalize-email-logos-381.py`.

Provenance per file (from Core's `assets/images/providers/README.md`):

| Provider id | Source | Licence / status |
| :--- | :--- | :--- |
| `aol` | Wikimedia Commons, `AOL logo (2024).svg` (current mark since 6 Dec 2024) | Public domain (per file page). Wordmark path had no fill; explicit `fill="#000000"` added (black is the brand's own colour). |
| `courier` | https://www.courier.com/favicon.svg (paper-plane mark) | Brand's own published mark, trademarked; used unaltered for provider identification, no open licence claimed. Gradient id prefixed (`courier-g`). |
| `dreamhost` | https://www.dreamhost.com/icon.svg (32x32, "D" swirl) | Trademarked, as above. The file's own white rounded-square background is kept as published (the dark-navy crescent would be near-invisible on a dark tile without it). |
| `fastmail` | https://www.fastmail.com/favicon.svg (blue ring, yellow/black envelope) | Trademarked, as above. |
| `kolabnow` | https://kolabnow.com/themes/kolabnow/images/logo.svg (orange ring) | Trademarked, as above. Fill came from a `<style>` class (`.st0 #F3A628`); replaced with an explicit fill, `<style>` dropped. |
| `mailersend` | https://www.mailersend.com/images/logo-symbol.svg (teal/blue symbol) | Trademarked, as above. |
| `mailfence` | https://mailfence.com/c/mailfence/images/favicon/safari-pinned-tab.svg (mono black envelope) | Trademarked, as above. Path inherited `fill="#000000"` from its `<g>`; made explicit. Black mark: low-contrast on a dark tile, like `resend`. |
| `mxroute` | https://mxroute.com/brand-mark.svg (pink envelope) | Trademarked, as above. Gradient ids prefixed (`mxroute-Background_1_`, `mxroute-Foreground_1_`); the source gradients are direct `<svg>` children (no `<defs>` wrap). |
| `onesignal` | https://onesignal.com/favicon.svg (navy `#051B2C` only) | Trademarked, as above. Navy mark: low-contrast on a dark tile. |
| `plunk` | https://www.useplunk.com/_next/static/media/logo.02awr5i-j9sof.svg (black "P") | Trademarked, as above. Black mark: low-contrast on a dark tile, like `resend`. |
| `sendpulse` | https://sendpulse.com/templates/sendpulsev1/img/favicons/favicon.svg (blue `#009FC1` pulse) | Trademarked, as above. Near-white background rect removed (the pulse path itself untouched). |
| `socketlabs` | https://www.socketlabs.com/images/favicon.svg (orange `#fc6423` puzzle) | Trademarked, as above. clipPath id prefixed (`socketlabs-clipPath26`). |
| `zeptomail` | https://www.zohowebstatic.com/sites/zweb/images/productlogos/zeptomail.svg | Trademarked, as above. **Icon only**: the source is a wide lockup (1142x296, 3.86:1); only the icon group (blue envelope/Z + yellow inner envelope) is kept and the "Zoho ZeptoMail" text paths are dropped. Class fills resolved from the source `<style>` (`.cls-1 #f9b21d`, `.cls-2 #226db4`). |

The brand marks themselves are the trademarks of their respective
owners and are used here only to identify their services; no
endorsement is implied. The 13 files are pure vector: no `<text>`,
no `<image>`, no base64.

## Lettermark fallbacks (task 360)

The remaining 20 providers have no entry in simple-icons, so their
existing lettermark / hand-drawn files were left untouched — the set
stays visually consistent and has no gaps. 13 of these are the
task-354 lettermarks (initials + `<text>`); 7 are the original
hand-drawn marks (`outlook`, `postmark`, `sendgrid`, `sendmail`,
`ses`, `smtp`, `yahoo`). (Task 381 removed one lettermark provider
and replaced 13 lettermarks with the real marks above, so this list
fell from 34 to 20.)

Lettermark fallbacks (20 ids): `elastic-email`, `mail-126`,
`mail-163`, `mailjet`, `outlook`, `posteo`, `postmark`, `runbox`,
`sender-net`, `sendgrid`, `sendlayer`, `sendmail`, `ses`, `sina`,
`smtp`, `smtp-com`, `smtp2go`, `startmail`, `yahoo`, `yandex`.

Checked against the simple-icons slugs table and deliberately NOT
substituted with a near-miss brand: `yandex` (only `yandexcloud`
exists — a different product), `sina` (only `sinaweibo` — Weibo,
not Sina Mail), `elastic-email` (`elastic` is a different brand),
`smtp` / `sendmail` / `smtp2go` / `smtp-com` (no generic SMTP brand
in simple-icons), `ses` (no Amazon SES entry). If simple-icons adds
any of these brands later, the fallback can be swapped for the real
mark following the same fetch-and-normalize steps above.
