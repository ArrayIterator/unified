# SVGRepo Icon Sets — Research & Replacement Proposal (Flat Color Icons)

Date: 2026-11-23
Status: EXECUTED 2026-11-23 (task 372) — see §10 execution record. Nothing committed or pushed yet (parent does the push).
Context: the 156 module icons in `assets/icons/modules/*.svg` are currently
ColorTicons (Tabler 2.11.0 single-colour outlines, `currentColor`). The user
rejected them as one-colour ("kok one color?") and too small, and asked to
search SVGRepo (https://www.svgrepo.com/) for multi-tone colour icons.
The user believes every SVGRepo icon is open/free to use — that is NOT
automatically true: SVGRepo is an aggregator and licences vary per icon.
Every licence below was verified at the original source repo.

## 0. Blocker — CLEARED by the user 2026-11-23

The source clone failed an execution-risk check, so the research agent
stopped before vendoring. The user then cleared it in plain words:
"Hapus colorticons!! buanh!! ambil dari svg repo" — explicit authorisation
to process the `icons8/flat-color-icons` content and delete ColorTicons.
The build was executed the same day (see §10).

## 1. The collection: Icons8 Flat Color Icons

- **SVGRepo collection:** "Flat Color Icons" — 311 icons
  (https://www.svgrepo.com/collection/flat-color-icons/)
- **Original source repo:** `icons8/flat-color-icons` (GitHub, default branch
  `master`, 329 SVG files in `svg/`, last updated 2026-11-20)
- **Style:** flat, genuinely multi-tone colour. Verified first-hand by
  reading three sample SVGs: `calendar.svg` (5 fills: #CFD8DC, #F44336,
  #B71C1C, #B0BEC5, #90A4AE), `safe.svg` (6 fills: #455A64, #78909C,
  #90A4AE, #37474F, #B0BEC5, #CFD8DC), `workflow.svg` (4 fills: #00BCD4,
  #3F51B5, #448AFF, #90A4AE). Grid 48x48 (`viewBox="0 0 48 48"`), flat
  fills, no strokes, no `currentColor` — the icons carry their own fixed
  palette, so they render identically on light and dark backgrounds.
- **Live preview (whole set, one page):**
  https://icons8.github.io/flat-color-icons/

## 2. Licence verification (exact)

Source repo README (`icons8/flat-color-icons`, read 2026-11-23):

> "312 free icons for personal and commercial use. No credits required."
> "We've released the icon pack either under MIT or the Good Boy License."

Good Boy Licence (https://icons8.com/good-boy-license/, read 2026-11-23):
permitted use is Download / Change / Fork; the page states the licence
"applies to most of our open-source freebies", listing "300+ Flat Color
icons" first.

- **Licence to record: MIT (per README) — or the Good Boy Licence as the
  stated alternative. Commercial use: YES. Attribution: NONE required.**
- **Caveats, recorded honestly:**
  - There is NO `LICENSE` file in the repo (raw `LICENSE` returns 404);
    the MIT claim rests on the README text alone. Vendor the README
    licence statement alongside the icons if the user approves.
  - The SVGRepo-side licence statement was NOT verified: the SVGRepo
    icon show page fetch timed out, and the collection page text carries
    no licence line. The source-repo verification above is what the
    proposal relies on.
  - The set is a free sample of Icons8's paid 13,000+ "Color icons" pack;
    only the 329 free SVGs are in scope, nothing from the paid pack.

## 3. Ranking

1. **Icons8 Flat Color Icons — RECOMMENDED.** 329 icons, genuinely
   multi-tone, one coherent 48-grid flat style, MIT-or-Good-Boy licence,
   no attribution, commercial OK. Coverage 152/156 (§5).
2. **Fluent Emoji (Microsoft) — MIT, verified.** ~1,538 glyphs, genuinely
   multi-colour, but emoji: only ~45–60 of 156 module concepts usable,
   muddy at 16–20px. Cannot carry the system.
3. **Twemoji — graphics CC-BY 4.0 (code MIT).** Same emoji coverage shape
   (~45–60), attribution required.
4. **IconPark (ByteDance) — Apache 2.0.** Genuinely multi-colour, 2,658
   icons, ~125–135/156 coverage — but the user already rejected it as ugly
   ("busuk semua iconnya", 2026-11-23). Superseded.
5. **ColorTicons (Tabler 2.11.0) — MIT.** The current set: single-colour
   outlines, NOT multi-tone. Rejected by the user for exactly that reason.
   Superseded if this proposal is approved.
6. **OpenMoji — CC-BY-SA 4.0.** ShareAlike binds derivatives — excluded
   (only MIT / Apache 2.0 / CC0 / CC-BY 4.0 are acceptable here).

## 4. Sample — the 12 hard modules (see the style live)

The full set is visible in one page at
https://icons8.github.io/flat-color-icons/ (also the SVGRepo collection
above). The 12 hardest concepts map as follows (no rendered grid could be
built from the files — see the blocker in §0):

| Module | Flat Color icon | Why it works |
| --- | --- | --- |
| load-balancer | CUSTOM | No traffic-distribution icon in the set (same trap as Tabler's `scale`); draw arrow splitting into 3 server bars in flat-colour style |
| vault | `safe` | A real safe with dial — exact |
| tenant-destroy | CUSTOM | No destroy concept; draw building + red X badge in flat-colour style |
| site-builder | CUSTOM | No builder concept; draw browser window + layout blocks in flat-colour style |
| licensing | `registered_trademark` | Licence/trademark mark — good |
| billing | `paid` | Invoice paid — good |
| ai | `automatic` | Automation/AI — good |
| geo | `radar_plot` | Map/radar plot — weak, see §6 |
| hr (attendance) | `manager` | People manager — good |
| exams | `questions` | Exam questions — good |
| library | `library` | Exact |
| workflows | `workflow` | Boxes + flow arrows — exact |

## 5. Coverage — 152/156 vendored, 4 custom

Decision rule from the brief: one coherent collection covering ≥120/156
means BUILD. This set covers **152/156** — the threshold is cleared.
(Build is still blocked by §0 until the user approves processing the
repo's content.)

Fit key: exact = the icon IS the concept; good = obvious mainstream
metaphor; ok = reasonable; weak = usable but not ideal (flagged in §6).

| Module | Flat Color icon | Fit |
| --- | --- | --- |
| academic | `graduation_cap` | exact |
| admissions | `diploma_1` | good |
| advisor | `assistant` | good |
| affiliates | `share` | good |
| ai-support | `online_support` | good |
| ai | `automatic` | good |
| analytics | `area_chart` | good |
| announcements | `speaker` | good |
| api-manager | `command_line` | good |
| apikeys | `key` | exact |
| app-sync | `synchronize` | exact |
| appointments | `calendar` | exact |
| approvals | `approval` | exact |
| assets | `briefcase` | good |
| audit | `inspection` | good |
| auditors | `binoculars` | good |
| auth-manager | `lock` | exact |
| backup | `data_backup` | exact |
| bank-transfer | `money_transfer` | good |
| bi | `pie_chart` | good |
| billing | `paid` | good |
| cache | `refresh` | ok |
| calendar | `clock` | weak |
| cards | `sim_card` | ok |
| certificates | `diploma_2` | good |
| chat | `comments` | good |
| cloudflare | `globe` | good |
| clusters | `parallel_tasks` | good |
| community | `collaboration` | good |
| company | `business` | exact |
| compliance | `rules` | good |
| content | `document` | good |
| contracts | `disclaimer` | ok |
| crm | `business_contact` | good |
| curriculum | `reading` | good |
| dashboards | `display` | good |
| delivery-notes | `shipped` | good |
| departments | `department` | exact |
| documents | `file` | exact |
| domain-verification | `approve` | good |
| domains | `link` | good |
| downloader | `download` | exact |
| elearning | `reading_ebook` | good |
| email-providers | `voicemail` | weak |
| email-templates | `template` | exact |
| entitlements | `vip` | good |
| esg | `biomass` | good |
| events | `podium_with_audience` | good |
| exams | `questions` | good |
| expenses | `debt` | ok |
| extensions | `puzzle` | exact |
| external-bi | `scatter_plot` | good |
| external-esignature | `fine_print` | weak |
| external-fleet | `automotive` | good |
| external-iot | `electro_devices` | good |
| external-whatsapp | `phone_android` | ok |
| field-service | `engineering` | good |
| files | `folder` | good |
| forms | `list` | ok |
| forums | `answers` | good |
| frontend | `view_details` | ok |
| gamification | `ratings` | good |
| geo | `radar_plot` | weak |
| holidays | `panorama` | good |
| hosting | `deployment` | good |
| hr | `manager` | good |
| i18n | `alphabetical_sorting_az` | ok |
| import-export | `import` | good |
| inbox | `opened_folder` | good |
| incidents | `biohazard` | ok |
| knowledgebase | `faq` | good |
| kyc | `camera_identification` | good |
| library | `library` | exact |
| licensing | `registered_trademark` | good |
| load-balancer | CUSTOM | — |
| logistics | `in_transit` | good |
| logs | `data_configuration` | weak |
| loyalty | `like` | ok |
| mail | `mms` | ok |
| maintenance | `reuse` | weak |
| manufacturing | `factory` | exact |
| marketing | `advertising` | exact |
| mcp | `circuit` | ok |
| meetings | `conference_call` | exact |
| network-tools | `broken_link` | weak |
| network | `wi-fi_logo` | good |
| newsletter | `news` | good |
| notifications | `missed_call` | weak |
| okrs | `decision` | ok |
| panels | `grid` | good |
| payments | `currency_exchange` | good |
| phone | `phone` | exact |
| placeholder | CUSTOM | — |
| planner | `planner` | exact |
| plm | `process` | good |
| portals | `internal` | ok |
| pos | `self_service_kiosk` | good |
| presets | `menu` | weak |
| pricing-tables | `line_chart` | good |
| pricing | `sales_performance` | good |
| printing | `print` | exact |
| privacy | `privacy` | exact |
| procurement | `collect` | weak |
| products | `gallery` | ok |
| projects | `mind_map` | good |
| properties | `home` | exact |
| providers | `org_unit` | ok |
| quality | `ok` | good |
| records | `filing_cabinet` | good |
| recruitment | `businessman` | good |
| reference-data | `address_book` | good |
| referral | `invite` | good |
| rental | `unlock` | ok |
| risk | `bad_decision` | good |
| roles | `businesswoman` | ok |
| routes | `flow_chart` | good |
| saas-domains | `external` | weak |
| saas-packages | `package` | good |
| saas | `upload` | ok |
| saved-views | `bookmark` | exact |
| scheduler | `alarm_clock` | good |
| search | `search` | exact |
| security | `data_protection` | good |
| seo | `doughnut_chart` | ok |
| service-monitor | `electrical_sensor` | good |
| settings | `settings` | exact |
| sign | `signature` | good |
| site-builder | CUSTOM | — |
| sms-marketing | `sms` | exact |
| social-marketing | `reddit` | weak |
| software | `android_os` | ok |
| spreadsheets | `data_sheet` | good |
| statuspage | `info` | good |
| storage-manager | `database` | good |
| storage | `data_recovery` | ok |
| store | `shop` | good |
| studio | `clapperboard` | good |
| supervision | `camera` | good |
| surveys | `survey` | exact |
| tasks | `todo_list` | exact |
| tax | `calculator` | exact |
| tenant-destroy | CUSTOM | — |
| tenants | `organization` | good |
| theme | `picture` | ok |
| tickets | `podium_without_speaker` | weak |
| timeline | `timeline` | exact |
| timetracker | `overtime` | good |
| todos | `checkmark` | good |
| users | `contacts` | good |
| vault | `safe` | exact |
| vcs | `tree_structure` | good |
| web3 | `electronics` | ok |
| webhooks | `feed_in` | good |
| webinars | `video_call` | good |
| whitelabel | `trademark` | good |
| workflows | `workflow` | exact |

Counts: 152 vendored (all verified present in the 329-file catalogue,
0 duplicates — each module gets a distinct icon), 4 custom-drawn.
Fit split: 32 exact, 83 good, 24 ok, 13 weak, 4 custom.

## 6. Weak picks (flagged, not hidden)

- `calendar` → `clock` (no second calendar icon; `planner` belongs to the
  planner module)
- `geo` → `radar_plot` (no map/pin icon in the set)
- `email-providers` → `voicemail` (no mail/envelope icon in the set;
  `sms`/`mms` belong to sms-marketing/mail)
- `logs` → `data_configuration`, `maintenance` → `reuse`,
  `procurement` → `collect`, `presets` → `menu`,
  `notifications` → `missed_call`, `network-tools` → `broken_link`,
  `saas-domains` → `external`, `social-marketing` → `reddit`,
  `tickets` → `podium_without_speaker`,
  `external-esignature` → `fine_print`
- If the user dislikes any weak pick, the fix is a custom draw in the
  same flat-colour style — not a forced re-mapping to a wrong icon.

## 7. Custom icons (4)

Drawn in the same flat-colour style (48 grid, flat fills, no strokes):
`load-balancer` (down arrow splitting into 3 server bars),
`tenant-destroy` (building + red X badge), `site-builder` (browser
window + layout blocks), `placeholder` (circle + "?").

## 8. Execution plan (only after the user approves §0)

1. Vendor the 152 mapped SVGs into `assets/icons/modules/<id>.svg` from
   the source repo (or a fresh fetch of it); custom-draw the 4 gaps.
2. Delete `assets/icons/LICENSE.colorticons` and remove all live
   ColorTicons references (same full-removal rule as IconPark before;
   historical docs may mention it as superseded).
3. Vendor the licence: copy the README licence statement
   ("released either under MIT or the Good Boy License", "free icons for
   personal and commercial use, no credits required") as
   `assets/icons/LICENSE.flat-color-icons`, plus the Good Boy Licence
   terms; record both in `assets/icons/README.md`.
4. Sprite builder: `scripts/build-module-icons-sprite.py` viewBox
   24 → 48 (the set is 48x48; the Tabler-era 24 is wrong for it);
   multi-colour fixed fills need no palette rules. Rebuild
   `assets/icons/modules-sprite.svg` (156 symbols, viewBox 48).
   Regenerate `demo/assets/sprite.js` from the new sprite.
5. Regenerate `assets/icons/ICONS.md` from disk (keep the 64px preview
   size). Update `assets/icons/README.md` (new provenance + licences +
   palette/style notes: fixed fills, identical on light/dark).
6. Do NOT touch the sub-module icons (582), payment logos (25) or brand
   icons (14) — the user did not complain about those.
7. Verify: sprite rebuilds clean; render grids at 48px and 16px on light
   and dark backgrounds and inspect; lint (well-formed XML, viewBox
   `0 0 48 48`); grep touched files for Indonesian strings / tnode /
   WordPress / TrayDigita (must be clean); `npm run check` passes.

## 9. What the user must decide

1. Proceed with processing the `icons8/flat-color-icons` content despite
   the execution-risk flag on the clone? (Required before any vendoring.
   The content itself is ordinary SVG files from a public repo; the flag
   is a runtime precaution, not a finding of malicious content — but only
   the user can clear it.)
2. Accept the licence as verified in §2 (MIT per README, no LICENSE file
   in repo, Good Boy Licence as alternative, no attribution required)?
3. Accept the 13 weak picks in §6, or name replacements / custom draws?


## 10. Execution record (2026-11-23, task 372)

- Vendored: **143** Flat Color Icons (the 152-row mapping minus the 9
  weak picks below that became custom draws), copied verbatim except the
  XML prolog / SVG 1.1 DOCTYPE / Adobe Illustrator generator comment
  were stripped (inert noise; the DOCTYPE URL only looks like an
  external reference). All 143 verified: XML parses, `viewBox`
  `0 0 48 48`, no `<image>` / `<script>` / `currentColor` / `url(`.
- Custom-drawn: **13** in the same flat-colour style (48 grid, flat
  fills, no strokes except the megaphone sound arcs): the 4 planned
  (`load-balancer`, `tenant-destroy`, `site-builder`, `placeholder`)
  plus 9 weak picks fixed by a custom draw instead of a forced
  re-mapping: `calendar` (blue-header calendar, distinct from the
  red-header `calendar` used by `appointments`), `geo` (map pin),
  `email-providers` (envelope), `logs` (terminal window),
  `notifications` (bell + red badge), `social-marketing` (megaphone),
  `presets` (sliders), `network-tools` (RJ45 plug), `tickets` (ticket).
- Swapped: `maintenance` `reuse` → `support` (a wrench — exact for
  maintenance; `support.svg` in this set is a wrench, not a headset).
- Swapped: `mail` `mms` → `feedback` (an envelope with an up-arrow —
  the `mms` photo bubble read as gallery, not mail; found during the
  visual render check).
- Kept as proposed: `procurement` → `collect`, `saas-domains` →
  `external`, `external-esignature` → `fine_print` (no better icon in
  the set; all usable).
- Honest caveat: 15 of the 143 vendored icons are single-fill shapes
  (one solid colour, e.g. `like` red heart, `bookmark`, `download`);
  the set as a whole is multi-tone (most icons carry 3–6 fills) and
  every icon is a solid colour shape, not a `currentColor` outline.
- Licence vendored as `assets/icons/LICENSE.flat-color-icons` (repo
  `LICENSE.md` verbatim + README statement + source URL). The repo
  HAS a `LICENSE.md` — the earlier "no LICENSE file" caveat was about
  a file literally named `LICENSE` (404); `LICENSE.md` exists and
  states MIT-or-Good-Boy.
- ColorTicons deleted: all 156 old module files replaced,
  `LICENSE.colorticons` deleted. The 582 sub-module icons are still
  Tabler 2.11.0 outlines; their licence now lives in
  `assets/icons/LICENSE.tabler`.
- Sprite rebuilt: 156 symbols, all `viewBox 0 0 48 48`, ids match the
  156 module files exactly. `data-animation` re-applied to the same 6
  modules as before (`app-sync`/`backup` spin, `notifications` wiggle,
  `downloader` bounce, `service-monitor`/`geo` pulse).
- `demo/assets/sprite.js` regenerated from the new sprite
  (round-trip verified: JSON-unescape is byte-identical).
- `assets/icons/ICONS.md` regenerated (64px preview).

## 11. Task 380 metaphor audit + swaps (2026-11-24)

The user looked at the ICONS.md preview and judged several icons
wrong metaphors / childish / ugly. Every one of the 157 module icons
was re-read from its file (never trust the filename — that is how
the SIM card and the `C:\` prompt slipped in) and judged on three
tests: (1) metaphor matches the module name, (2) looks decent/modern
(not childish, not ugly), (3) stylistically consistent with the rest
of the set (same flat style, same visual weight, same 48 grid).

WARNING: the §5 mapping table's "good"/"weak" ratings are
UNRELIABLE — several "good" ratings were wrong on first-hand
reading (advisor → `assistant` "good" was in fact the same headset
person; cards → `sim_card` "ok" was a SIM card; api-manager →
`command_line` "good" was a DOS prompt). Judge from the files.

### 11.1 Swaps executed (13)

All customs are 48x48 viewBox, flat multi-tone fills, no strokes,
no `currentColor`, no `<text>` (except the placeholder "?").

| Module | Old | New | Failed test | Note |
|---|---|---|---|---|
| `cards` | Flat Color `sim_card_chip` (teal rect + orange chip — a SIM card) | CUSTOM credit card (blue #1565C0 body, dark stripe, gold chip, number lines) | 1 metaphor | The set has NO credit-card icon at all (329 files checked). |
| `advisor` | Flat Color `support` (person, orange hair, headset + mic) | CUSTOM person + tie + speech bubble (blue suit, red tie, bubble with 3 dots) | 1 + 2 | The set's `assistant.svg` is IDENTICAL to the old icon. Suited people already used by `businessman`/`manager`/`businesswoman`, so a custom was needed. |
| `api-manager` | Flat Color `command_line` (window with green `C:\` text) | CUSTOM `</>` code window (dark window, traffic-light dots, cyan `<`, amber `/`, orange `>`) | 1 metaphor | The set has no code-brackets icon. |
| `elearning` | Flat Color `reading` (person reading a book) | CUSTOM laptop + play button (dark frame, blue screen, white play triangle) | 1 + 2 | Childish/school-like per the user. `reading_ebook.svg` is ALSO a person reading, not a device. `graduation_cap` is taken by `academic`. |
| `logistics` | Flat Color `in_transit` (box truck) | CUSTOM warehouse (gable-roof building, door, windows, orange parcel) | 1 + 2 | Reads as a generic car at preview size. `package`/`shipped` taken. |
| `okrs` | Flat Color `decision` (face with blue `?`) | CUSTOM target (red/white concentric circles + blue arrow) | 1 metaphor | |
| `products` | Flat Color `gallery` (two landscape pictures) | CUSTOM shopping bag (teal bag, dark handle, fold band) | 1 metaphor | `package`/`shop`/`grid`/`briefcase` all taken. |
| `whitelabel` | Purple circle with `TM` letters | CUSTOM tag + sparkle (purple tag, white hole, white 4-point sparkle) | 3 consistency | Near-duplicate of licensing's purple `®` circle. Licensing keeps `®`. |
| `web3` | Flat Color `web` (orange lattice + green square) | CUSTOM blockchain blocks (blue/purple/teal blocks + link bars + dots) | 1 metaphor | Abstract lattice, not recognisably web3. |
| `ai` | Flat Color `automatic` (dark square + light-blue gear) | CUSTOM sparkle (dark rounded square, big white 4-point star + 2 small blue stars) | 1 metaphor | Gear reads as settings/automation, not AI. |
| `calendar` | CUSTOM sparse grid (white body, blue header, 5 grey squares + 1 blue date — user: ugly) | Flat Color `calendar.svg` vendored (red header, full date grid, rings) | 2 modernity | Custom is not automatically better. |
| `appointments` | Flat Color `calendar.svg` (identical to the new `calendar.svg`) | CUSTOM variant: set calendar + blue clock badge (blue circle, white face, dark hands) | 3 consistency | Appointment = scheduled time; keeps the two calendars distinct. |
| `bank-transfer` | Flat Color `envelope` (mail icon) | CUSTOM bank building (grey-blue pediment roof + white dot, 4 columns, 2-step base) | 1 metaphor | User rule: bank transfer uses a BANK icon. The set has no bank/landmark icon. The payment logo `payment-banktransfer.svg` was a "BT" lettermark — replaced with a custom bank building in the logos convention too (24x24, single colour). |

Verified kept (first-hand reads correcting the table): `notifications`
is ALREADY a custom bell (not `missed_call`); `social-marketing` is a
red megaphone + broadcast waves (distinct from `marketing`); `webhooks`
is a blue hook/pipe with down-arrows; `webinars` is a green video
camera; `delivery-notes` is a green truck WITH a white checkmark
(distinct from plain-truck logistics); `licensing` is the purple `®`.

Sprite rebuilt after the swaps: 157 symbols, all `viewBox 0 0 48 48`.
`demo/assets/modules-sprite.svg` re-copied and `demo/assets/sprite.js`
regenerated from it (round-trip verified).

### 11.2 Placeholder audit — modules with no icon (2026-11-24)

Measured, not assumed. 145 module ids were extracted from
`fn id(&self)` in `src/modules/*/module.rs` (tenants = `tenants`,
maps = `maps`, both via a `MODULE_ID` const). 0 module-owned
`icon.svg` files exist; all icons come from central
`assets/icons/modules/*.svg` (157 files).

The lookup (`src/themes/module_icons.rs::module_icon_use`, same rule
in `assets/react/src/sprite.ts::hasModuleIcon`) accepts ONLY
`[a-z0-9-]` names and does NOT normalise snake_case to kebab-case:

- 19 module ids are snake_case, so `module_icon_use` returns an
  EMPTY string (no icon at all) on the Handlebars track, and the React
  track shows an initials tile. They do NOT render the "?" placeholder
  (that only renders for a valid kebab name with no symbol — and every
  kebab id has a file, so no module currently hits the placeholder):
  `bank_transfer`, `data_tools`, `delivery_notes`,
  `domain_verification`, `email_providers`, `field_service`,
  `import_export`, `load_balancer`, `network_tools`, `pricing_tables`,
  `reference_data`, `saas_domains`, `saas_packages`, `saved_views`,
  `sms_marketing`, `social_marketing`, `storage_manager`,
  `tenant_destroy`, `widgets`.
- 17 of those 19 DO have a kebab central file (`bank-transfer.svg`
  etc.) sitting in the sprite — unreachable, because nothing looks
  them up by the kebab name. The kebab files are effectively dead
  until the ids are unified (separate pending user decision — modules
  were NOT renamed in this task).
- 2 of the 19 have no icon file in either spelling: `data_tools`,
  `widgets`. These need a new icon whenever the kebab unification
  lands (warehouse/package for `data_tools` is ambiguous — the icon
  should wait for the module's scope to be named; `widgets` wants a
  grid/puzzle icon).
- 32 central files have no module id (orphans): the 17 dead kebab
  files above + `audit`, `backup`, `domains`, `external-bi`,
  `external-esignature`, `external-fleet`, `external-iot`,
  `external-whatsapp`, `hosting`, `newsletter`, `providers`, `store`,
  `tenants`, `whitelabel`, `workflows`. These are either icons for
  modules whose ids live elsewhere (external crates, sub-areas) or
  genuinely unused — do not delete on this audit alone.

### 11.3 Alternative-set proposal — NOT executed (2026-11-24)

The user asked: if the Flat Color Icons set itself is the childish
problem, research ONE modern flat multi-colour alternative and
PROPOSE it, do not swap. Verdict: **stay with Flat Color Icons**
(+ custom draws in the same style, as in §11.1).

- Icons8 **Fluency** (the obvious modern candidate, Microsoft Fluent
  style) was verified first-hand via search (icons8.com + Icons8 help
  centre, 2026-11-24): free use requires a link back to Icons8 on
  every page (two hyperlinks per the Universal License Agreement);
  the free tier is PNG only, up to 100px — SVG vectors need a paid
  plan (except the "Popular" category). That fails the repo licence
  rule (MIT/Apache/CC0/CC-BY only) AND the format rule (the sprite
  needs SVG). A Fluency swap would need either a paid Icons8 plan or
  a user-approved licence exception. Not executed.
- Emoji sets (Noto Emoji Apache 2.0, Twemoji CC BY 4.0, OpenMoji
  CC BY-SA) are colourful but read playful/childish — the opposite of
  what the user wants.
- The large monochrome sets (Lucide/Feather/Tabler/Iconoir, MIT)
  fail the multi-colour requirement.
- No open-licensed modern multi-colour flat business set with 150+
  metaphors was found that clearly beats Flat Color Icons. The
  "childish" feel is a property of individual picks (fixed in §11.1),
  not of the whole set.

