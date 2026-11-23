# Payment logos

One SVG logo per payment provider (12 `PaymentProviderKind`), card
scheme (5), and Indonesian payment method (8) — 25 files in total.
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

**Card schemes (5).** Visa, Mastercard, American Express, Discover,
Diners Club, JCB — real brand marks from simple-icons. Mastercard is
both a provider kind and a scheme; its one file serves both roles.

**Indonesian payment methods (8).** DANA, OVO, GoPay, QRIS,
ShopeePay, LinkAja, Indomaret, Alfamart. These are METHODS offered
through Midtrans/Xendit, not providers. None of them has a
simple-icons entry (verified 404 on the current simple-icons release
— same for BCA/Mandiri/BRI/BNI), so they are lettermark monograms
in the same style as the provider fallbacks, not official artwork.

## Real brand marks (11)

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

## Lettermark fallbacks (14)

Providers without a simple-icons entry (6): `hyperpay` (HP),
`twocheckout` (2C), `card` (C), `midtrans` (M), `custom` (CU),
`banktransfer` (BT). Indonesian methods (8): `dana` (DA), `ovo`
(OVO), `gopay` (GP), `qris` (QR), `shopeepay` (SP), `linkaja` (LA),
`indomaret` (IM), `alfamart` (AM). Colours are brand-ish
approximations, not trademarked artwork.
