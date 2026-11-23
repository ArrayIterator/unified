# Brand icons

General brand collection for brands referenced across the UI and
code, none of them covered by the email or payment logo sets. One
SVG per brand in `brand-<slug>.svg`, combined into
`brands-sprite.svg` (symbols `brand-<slug>`) by:

```
python3 scripts/build-provider-logos-sprite.py --src assets/icons/brands --glob 'brand-*.svg' --prefix '' --out assets/icons/brands-sprite.svg
```

Demo copies live in `demo/assets/logos/brand-*.svg`.

Real brand marks are from
[simple-icons](https://github.com/simple-icons/simple-icons),
fetched from the simple-icons CDN (`https://cdn.simpleicons.org/{slug}`)
in the brand's own colour. Simple Icons is released under the
[CC0 1.0 Universal](https://github.com/simple-icons/simple-icons/blob/develop/LICENSE.md)
license. The brand marks themselves are the trademarks of their
respective owners and are used here only to identify them; no
endorsement is implied. Normalization is the same as for the
provider logos: the brand hex is set as the `fill` on the `<path>`
itself, `viewBox="0 0 24 24"`, English `<title>` first, no `<text>`,
no stroke.

## Real brand marks (11)

| Brand | simple-icons slug |
| :--- | :--- |
| Cloudflare | `cloudflare` |
| WhatsApp | `whatsapp` |
| Telegram | `telegram` |
| Google | `google` |
| Apple | `apple` |
| GitHub | `github` |
| GitLab | `gitlab` |
| Facebook | `facebook` |
| X | `x` |
| Zoom | `zoom` |
| Bitbucket | `bitbucket` |

## Lettermark fallbacks (3)

Microsoft, Slack, and Amazon were checked against the current
simple-icons release (npm 16.38.1) and the `develop` branch on
2026-11-23: all three return 404 — simple-icons removed those
brands in the current release (only an older, stale jsDelivr
`@latest` cache still serves them, which is not a source to rely
on). They are lettermark monograms instead, in the same style as
the provider fallbacks. Colours are brand-ish approximations, not
trademarked artwork.

| Brand | Letters | Colour |
| :--- | :--- | :--- |
| Microsoft | MS | `#00A4EF` |
| Slack | SL | `#4A154B` |
| Amazon | AM | `#FF9900` |

