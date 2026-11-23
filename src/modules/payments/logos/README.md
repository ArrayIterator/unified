# Payment logos

One SVG logo per payment provider (12 `PaymentProviderKind`), card
scheme (6), and Indonesian payment method (8) — 25 files in total.
The catalog endpoints (`GET /api/v1/payments/providers` and
`GET /api/v1/payments/providers/{kind}`) return one per provider kind
as `"logo": "payment-paypal"` — a stem, never a URL. The demo copies
these files to `demo/assets/logos/` and shows them in the provider
picker grid.

## Why these files live here

The module-icon sprite (`assets/icons/modules-sprite.svg`) is built
from module `icon.svg` files for the 156 modules. Provider brands
are not modules, so their logos do not belong in that sprite. They
follow the same convention instead (plugin model: the icon
belongs to the thing it pictures):

- flat solid fills, 24x24 viewBox, at most two colours,
  `var(--icon-primary, #brand)` for the main shape and
  `var(--icon-secondary, #fff)` for the detail;
- no external fetches, no strokes;
- no text elements — EXCEPT the lettermarks: a rounded rect plus
  one `<text>` element with initials (Arial/Helvetica, bold, 7–8pt).
  Lettermarks are clean initial monograms, not trademarked artwork
  redrawn in detail. No other text elements are allowed;
- the files in this directory are the source of truth.

## Sprite

`sprite.svg` is generated from the files in this directory by:

```
python3 scripts/build-provider-logos-sprite.py --src src/modules/payments/logos --glob 'payment-*.svg'
```

One `<symbol id="provider-logo-payment-<id>" viewBox="0 0 24 24">`
per logo, sorted by id. Re-run the script after adding or changing
a logo. `logos.rs` embeds the sprite at compile time
(`PAYMENT_LOGOS_SPRITE`) and renders one logo with
`payment_logo_use("payment-paypal")`. A missing logo renders nothing
— brand logos have no placeholder, unlike module icons.

## The three groups

**Providers (12).** One logo per `PaymentProviderKind`; the catalog
`logo` stem is `PaymentProviderKind::logo_stem()`.

**Card schemes (6).** Visa, Mastercard, American Express, Discover,
Diners Club, JCB — real brand marks from simple-icons. Mastercard is
both a provider kind and a scheme; its one file serves both roles.

**Indonesian payment methods (8).** DANA, OVO, GoPay, QRIS,
ShopeePay, LinkAja, Indomaret, Alfamart. These are METHODS offered
through Midtrans/Xendit, not providers. None of them has a
simple-icons entry (verified 404 on the current simple-icons release
— same for BCA/Mandiri/BRI/BNI), so they are lettermark monograms
in the same style as the provider fallbacks, not official artwork.

## Real brand marks (12)

From [simple-icons](https://github.com/simple-icons/simple-icons),
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

Real brand marks (file → simple-icons slug):

| File | simple-icons slug |
| :--- | :--- |
| `payment-xendit.svg` | `xendit` |
| `payment-mastercard.svg` | `mastercard` |
| `payment-googlepay.svg` | `googlepay` |
| `payment-applepay.svg` | `applepay` |
| `payment-paypal.svg` | `paypal` |
| `payment-stripe.svg` | `stripe` |
| `payment-visa.svg` | `visa` |
| `payment-americanexpress.svg` | `americanexpress` |
| `payment-discover.svg` | `discover` |
| `payment-dinersclub.svg` | `dinersclub` |
| `payment-jcb.svg` | `jcb` |

Plus one official vendor mark (not simple-icons): `payment-hyperswitch.svg`
— the Hyperswitch switch mark, icon only (wordmark removed), extracted
from the official logo `docs/imgs/hyperswitch-logo-light.svg` in
[juspay/hyperswitch](https://github.com/juspay/hyperswitch) (Apache 2.0).
The mark itself is Juspay's trademark and is used here only to identify
the service; no endorsement is implied. The two blue fills (`#0099FF`
+ `#0561E2`) are the brand's own colours.

## Lettermark fallbacks (12)

Providers without a simple-icons entry (4): `twocheckout` (2C),
`card` (C), `midtrans` (M), `custom` (CU). `banktransfer` is a
custom-drawn generic bank building, not a lettermark and not a brand
mark. Indonesian methods (8): `dana` (DA), `ovo`
(OVO), `gopay` (GP), `qris` (QR), `shopeepay` (SP), `linkaja` (LA),
`indomaret` (IM), `alfamart` (AM). Colours are brand-ish
approximations, not trademarked artwork.

## Wikimedia Commons availability (checked 2026-11-24, task 380)

The user asked to source real marks for the Indonesian methods from
Wikipedia/Wikimedia Commons (icon mark ONLY, no wordmark text —
"reconstruct" where the file is a full lockup). Existence + licence
were verified per file page; the SVG sources themselves could NOT be
fetched in this run (every `upload.wikimedia.org` original-SVG fetch
failed — timeout / no extractable content — and must not be retried),
so no Commons mark was vendored. The parent can fetch via the live
browser or another route.

Exists on Commons, fetch blocked (icon-only extraction still owed):
`dana` — File:Logo dana blue.svg (512x146, 2020-01-24); licence block
did not survive the page fetch, re-check before use. `gopay` —
File:Logo Gopay.svg (512x495, 2024-08-15, author Moconaid) — CC0 1.0
Universal; near-square, likely already icon-only. `qris` —
File:Logo QRIS.svg (163x62, Bank Indonesia source) — Public Domain in
Indonesia (Art 43 Law 28/2014) + Trademarked. `shopeepay` —
File:Shopee.svg (1000x320, 2022-07-25) — PD-textlogo + Trademarked;
wide lockup, extract the orange bag only. `alfamart` —
File:Alfamart logo.svg (3543x1134, 2018-07-03, "Current logo") —
Public Domain + Trademarked; wide lockup, extract the mark only.
(The older File:Alfamart.svg is the 2003-2015 logo — do not use.)

Not found on Commons: `ovo` (only OVO Energy UK + OVO Sound; the
user-provided id.wikipedia URL Berkas:Logo_ovo_purple.svg could not
be opened), `linkaja` (only the LINK interbank network logo,
File:Link logo (2019).svg, PT Jalin Pembayaran Nusantara — NOT the
LinkAja e-wallet mark), `indomaret` (no SVG file, store photos only),
`midtrans` (only an unrelated shipping company), `twocheckout`
(nothing found; now Verifone).

Available on Commons, no repo usage — noted only, NOT added:
`Bank Central Asia.svg` (File:Bank Central Asia.svg, 1000x313,
2020-03-18, source bca.co.id) — Public Domain + Trademarked; wide
lockup. BCA appears in the repo only as an example bank code in an
error message (`xendit.rs`), never as a logo. Same for BRI/BNI/BTN/
HSBC: the category "SVG logos of banks in Indonesia" holds 79 files,
but no bank is a repo payment method/provider, so no bank logo files
were added.

Card schemes: all 6 files are the genuine simple-icons real marks,
verified from file content (Visa #1A1F71, Mastercard #EB001B two
circles, Amex #2E77BC, Discover #FF6000, Diners #004C97 split circle,
JCB #0B4EA2). Mastercard was additionally cross-checked against its
Commons file and matches. No replacement was needed; individual
Commons file-page cross-checks for the other five were not done in
this run.
