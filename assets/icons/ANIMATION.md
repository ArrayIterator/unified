# Module icon animation

Animation is **off by default** and opt-in only. Only the icons below
animate, each because the motion means something for that module.
Every other icon stays static — no random motion.

## Where the mapping lives (no hard-coded list)

Which icons animate is **data, declared in the icon files**: an icon
declares its default animation with `data-animation="spin"` (and
friends) on its per-module SVG root (or its module-owned `icon.svg`).
The sprite builder copies the attribute onto the `<symbol>`, the Rust
accessor reads it back (`module_icon_animation`) and emits it as
`data-icon-animation` on the rendered `<svg>`. `assets/css/icons.css`
contains no module names at all — it only defines the keyframes and
the generic triggers.

## Triggers

- Parent class `.icon-animated`: runs each icon's own declared
  animation.
- `:hover` on the icon itself: same, its own declared animation.
- Parent (or icon) attribute `data-icon-animation="spin|wiggle|pulse|
  bounce"`: explicit choice — every icon inside animates that way,
  overriding declared defaults.
- `prefers-reduced-motion: reduce` disables all animation.

## Animated icons

| Icon | Declared | Why it animates |
|---|---|---|
| `app-sync` | spin | Syncing is rotation; the circular arrows turning reads as "sync in progress". |
| `backup` | spin | A backup run is a circular process; rotation reads as "backup running". |
| `notifications` | wiggle | A bell ringing — the wiggle is the bell swinging. |
| `downloader` | bounce | The download arrow dips down, the direction the file travels. |
| `service-monitor` | pulse | A live heartbeat — the monitor is alive and watching. |
| `geo` | pulse | A location ping radiating outward. |

## Deliberately static

Everything else. In particular: `ai` (a brain should not spin),
`security` (a shield must look steady, not nervous), `search`
(a magnifier pulsing reads as a loading bug), `scheduler` (a clock
spinning implies time passing, not scheduling), and all content /
commerce / settings icons — motion there would be decoration, not
meaning.
