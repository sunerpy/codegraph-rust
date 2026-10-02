# CodeGraph viewer · direction D "Modern Tech" — numeric spec

**Source.** Penpot 2.17.2 (self-hosted), file `CodeGraph Rust Design`
(`9a290f99-84bd-8087-8008-a5835d3d098e`), page `D · Modern Tech` (`cb2954bc-8298-80f6-8008-ba2019510df0`).
Every number below was read back from the file through the plugin API on 2026-10-02 after the last write
(layer dumps in `dumps/`, grammar in §15), or is the builder constant that drew it. Nothing is estimated:
all 15 boards and all 8 state fragments report 0 unmeasured text boxes and 0 fixed text boxes narrower
than their text.

**Status.** Design draft. The owner confirmed direction D on 2026-10-02 ("风格确认没有问题"). No UI code
exists yet; when the viewer is built, the code follows these numbers (画板先改，代码随画板) and every conscious
difference goes into a deviation log (`偏离（画板 vs 落地，供 owner 复核）`).

Coordinates are board-relative CSS px at 1×. PNGs in `png/` are 2×. A name in backticks is a library colour of
the `D · Nebula` group (`fg-3`) or a typography of the `D · Type` group (`mono-500`).

## 1. Boards

| Board | id | page x,y | size | PNG (2×) | dump |
|---|---|---|---|---|---|
| D-01 Start | `cb2954bc-8298-80f6-8008-ba4c89f1d5c6` | 0,0 | 1440×900 | `png/D-01-start.png` | `dumps/D-01.dump.txt` |
| D-02 Symbol | `cb2954bc-8298-80f6-8008-ba555884af2a` | 1600,0 | 1440×900 | `png/D-02-symbol.png` | `dumps/D-02.dump.txt` |
| D-03 File | `cb2954bc-8298-80f6-8008-ba4138c74c1d` | 3200,0 | 1440×900 | `png/D-03-file.png` | `dumps/D-03.dump.txt` |
| D-04 Flow | `cb2954bc-8298-80f6-8008-ba4188bc6d7f` | 0,1200 | 1440×900 | `png/D-04-flow.png` | `dumps/D-04.dump.txt` |
| D-05 Map | `cb2954bc-8298-80f6-8008-ba41bcf16ccd` | 1600,1200 | 1440×900 | `png/D-05-map.png` | `dumps/D-05.dump.txt` |
| D-06 Type hierarchy | `cb2954bc-8298-80f6-8008-ba420fa5fc7f` | 3200,1200 | 1440×900 | `png/D-06-type-hierarchy.png` | `dumps/D-06.dump.txt` |
| D-07 Dead code | `cb2954bc-8298-80f6-8008-ba423fe8cb53` | 0,2400 | 1440×900 | `png/D-07-dead-code.png` | `dumps/D-07.dump.txt` |
| D-08 Screens | `cb2954bc-8298-80f6-8008-ba4263140383` | 1600,2400 | 1440×900 | `png/D-08-screens.png` | `dumps/D-08.dump.txt` |
| D-09 Steps | `cb2954bc-8298-80f6-8008-ba4287965edc` | 3200,2400 | 1440×900 | `png/D-09-steps.png` | `dumps/D-09.dump.txt` |
| D-10 Symbol · tablet | `cb2954bc-8298-80f6-8008-ba42a64951da` | 4800,0 | 768×1024 | `png/D-10-symbol-tablet.png` | `dumps/D-10.dump.txt` |
| D-11 Symbol · phone | `cb2954bc-8298-80f6-8008-ba42c5c79117` | 5728,0 | 375×812 ¹ | `png/D-11-symbol-phone.png` | `dumps/D-11.dump.txt` |
| D-02s Symbol · states | `cb2954bc-8298-80f6-8008-ba437e4a89b1` | 1600,3600 | 1520×3220 | `png/D-02s-symbol-states.png` ² | `dumps/D-02s*.dump.txt` |
| D-F1 Foundations · colour & effects | `cb2954bc-8298-80f6-8008-ba5bd4d32429` | 4800,1200 | 1440×900 | `png/D-F1-colour-effects.png` | `dumps/D-F1.dump.txt` |
| D-F2 Foundations · type, icons & components | `cb2954bc-8298-80f6-8008-ba430ad45f3a` | 4800,2400 | 1440×900 | `png/D-F2-type-icons-components.png` | `dumps/D-F2.dump.txt` |
| D-F3 Foundations · interaction states | `cb2954bc-8298-80f6-8008-ba57db95b601` | 4800,3600 | 1440×960 | `png/D-F3-interaction-states.png` | `dumps/D-F3.dump.txt` |

¹ Penpot stores the height as 811.9999999999999; the export is 750×1624.
² The server exporter times out on the whole 1520×3220 board, so each fragment is exported on its own at 2× and
pasted back at its board position with the board's own caption texts (`tools/compose_states.py` in the lane,
3040×6440). The Penpot board itself is complete; only its PNG is composed.

Grid: columns at x 0 / 1600 / 3200 / 4800 (5728 for the phone), rows at y 0 / 1200 / 2400 / 3600. Every main
board is strictly 1440×900; states live in D-02s, below the untouched D-02.

## 2. Shell

Desktop (1440×900), from `dumps/D-02.dump.txt`; geometry constants `nav 64 · top 56 · trail 40 · gap 8 ·
railL 288 · railR 320`.

| Part | Geometry | Style |
|---|---|---|
| Canvas | 0,0 W×H | `bg`; the nav and the command bar sit on it with no fill of their own |
| Brand | 14,10 36×36, r 10 | GRAD.brand diagonal, SH.glow 35 %; two linked squares in `on-primary` (outline 1.5 at 10,10 7×7 · filled at 19,19 7×7 · curve 1.5) |
| Nav destinations | x 12, y 66 + 46·i, 40×40, r 8 | order Start, Map, Symbol, Flow, Screens, Steps, Dead code, Saved trails; icon 16 `fg-3`; active: `primary-soft` fill, `primary-ink` icon, indicator 3×20 at x 0, y + 10, GRAD.brand vertical, r 2 |
| Nav foot | keyboard at y H−100, settings at y H−54, 40×40 | as destinations |
| Project switch | icon `box` 16 at 78,20 `fg-2` · name `body-500` `fg` at 102,18 · chevron 14 `fg-3` | branch pill at 227,17: `raised`, r 11, icon `git-branch`, `mono-sm` `fg-2` (`main · 99a8adb`) |
| Command input | x = round((W − 540) / 2) + 20 = 470, y 10, 540×36, r 10 | `raised` + 1 px `line`; search icon 16 `fg-3` at 12; placeholder `small` `fg-3`; keys ⌘ K at the right edge (18×18 `overlay` + `line-strong`, r 5, `kbd` `fg-2`, gap 4, 10 from the edge); focus: `primary-line` border + SH.glow 25 % |
| Index status | right-aligned 16 from the edge, y 17, 22 high | pill `raised`, dot 6 `green` + SH.glowGreen 60 %, `small` `fg-2` (`Index current · 438 files · 12,569 nodes`) |
| Trail ribbon | 72,56, W−80×40, r 12 | `panel` + `line-faint`; `TRAIL` micro at 16; `+N` earlier pill (`mono-sm`, 22 high); hops 28 high r 8 at y 6 (`raised` + `line`; current `primary-soft` + `primary-line` + SH.glow 25 %, name `primary-ink`), tile 18 at 8,5, name `mono` at 34, gap 6, chevron 14 `fg-4` (+20); actions right-aligned 10 from the edge, 30 high, gap 6: Clear (ghost), Save trail (secondary, icon `bookmark`), Read as flow (ghost, icon `workflow`) |
| Islands | x 72 … W−8, y 104 (64 when a view has no trail) … H−8, gap 8 | `panel` + 1 px `line-faint`, r 12, clip |

Island layouts per view (top-level rectangles from the dumps):

| View | Islands (x,y w×h) |
|---|---|
| D-01 Start | project 72,64 1360×84 · cards 3×2: x 72 / 528 / 984, w 448; row 1 y 156 h 352; row 2 y 516 h 376 |
| D-02 Symbol | called by 72,104 288×788 · symbol 368,104 736×788 · calls 1112,104 320×788 |
| D-03 File | depended on by 72,104 288×788 · outline 368,104 736×788 · depends on 1112,104 320×788 |
| D-04 Flow | header 72,104 1360×64 · canvas 72,176 1360×400 · every hop 72,584 1360×308 |
| D-05 Map | canvas 72,104 1032×788 · inspector 1112,104 320×788 |
| D-06 Type hierarchy | used by 72,104 288×788 · type 368,104 736×788 · calls 1112,104 320×788 |
| D-07 Dead code | dead code 72,104 924×788 · left off 1004,104 428×788 |
| D-08 Screens / D-09 Steps | canvas 72,64 1032×828 · inspector 1112,64 320×828 (no trail: these read a sample project) |

## 3. Colour

### 3.1 Tokens — `D · Nebula`, 39 tokens + 15 aliases = 54 library colours

WCAG 2.x contrast of each token against the four surfaces it can sit on (floored to 2 decimals).

| token | hex | panel | card | raised | overlay | role |
|---|---|---|---|---|---|---|
| `bg` | `#0A0C10` | 1.06 | 1.12 | 1.21 | 1.29 | app background, behind the islands (elevation 0) |
| `rail` | `#0D1016` | 1.03 | 1.09 | 1.18 | 1.25 | phone tab bar; the desktop nav sits on `bg` (elevation 1) |
| `panel` | `#10141B` | 1.00 | 1.05 | 1.14 | 1.22 | islands (elevation 2) |
| `card` | `#151A23` | 1.05 | 1.00 | 1.08 | 1.15 | cards, rows, the code card (elevation 3) |
| `raised` | `#1B2130` | 1.14 | 1.08 | 1.00 | 1.06 | inputs, chips, tracks, the hovered line (elevation 4) |
| `overlay` | `#1F2636` | 1.22 | 1.15 | 1.06 | 1.00 | popovers, palette, toolbars, legends, tooltips (elevation 5) |
| `line` | `#232A38` | 1.28 | 1.21 | 1.11 | 1.05 | card, input and chip borders |
| `line-strong` | `#303A4D` | 1.61 | 1.52 | 1.40 | 1.32 | tooltip and palette borders, resting map edges, kbd keys |
| `line-faint` | `#191F29` | 1.11 | 1.05 | 1.02 | 1.09 | island borders, rules inside cards |
| `fg` | `#E8ECF4` | 15.58 | 14.73 | 13.57 | 12.77 | titles, names, code |
| `fg-2` | `#A9B2C4` | 8.65 | 8.18 | 7.53 | 7.09 | body copy, secondary labels, keyboard selection border |
| `fg-3` | `#838DA0` | 5.52 | 5.21 | 4.80 | 4.52 | meta, captions, resting icons |
| `fg-4` | `#4A5468` | 2.42 | 2.29 | 2.11 | 1.98 | line numbers, disabled, hints — decorative only (§3.5) |
| `primary` | `#7C93FF` | 6.56 | 6.20 | 5.71 | 5.38 | selection, focus ring, current hop, kind tile of methods |
| `primary-2` | `#A07CFF` | 5.99 | 5.66 | 5.22 | 4.91 | second stop of GRAD.brand |
| `primary-soft` | `#1A2140` | 1.17 | 1.10 | 1.02 | 1.03 | selected / hot / origin fills |
| `primary-line` | `#3B4A8C` | 2.23 | 2.11 | 1.94 | 1.83 | borders of selected / hot / origin |
| `primary-ink` | `#B9C6FF` | 11.05 | 10.45 | 9.62 | 9.06 | text on `primary-soft` (9.42 there) |
| `primary-deep` | `#4465FF` | 3.99 | 3.78 | 3.48 | 3.27 | GRAD.button stop 1 — fill only, never text |
| `primary-2-deep` | `#7F4EFF` | 3.89 | 3.68 | 3.39 | 3.19 | GRAD.button stop 2 — fill only, never text |
| `on-primary` | `#FFFFFF` | — | — | — | — | text and marks on GRAD.button (4.61 / 4.73 on its stops) and on the brand tile |
| `cyan` | `#3DD6E8` | 10.52 | 9.95 | 9.16 | 8.62 | data moving: calls, ports, call capsules' names, connectors, the hot edge |
| `cyan-soft` | `#0E2A31` | 1.22 | 1.15 | 1.06 | 1.00 | call capsules, cyan pills |
| `cyan-line` | `#1F5966` | 2.35 | 2.22 | 2.05 | 1.92 | resting code-port connectors, hot capsule border |
| `green` | `#3FD68E` | 9.86 | 9.32 | 8.58 | 8.07 | proven: tests reach, index current, entry points |
| `green-soft` | `#0E2A1F` | 1.20 | 1.13 | 1.04 | 1.01 | green pills |
| `green-deep` | `#2BB673` | 7.06 | 6.67 | 6.15 | 5.79 | GRAD.green stop 1 |
| `amber` | `#F5B544` | 10.17 | 9.61 | 8.85 | 8.33 | conditional or stale: WHEN guards, drift, the unreached band, "nothing depends on this" |
| `amber-soft` | `#2E240E` | 1.20 | 1.14 | 1.05 | 1.01 | amber callouts and pills |
| `red` | `#F2706B` | 6.41 | 6.06 | 5.59 | 5.26 | failure only: the error card, a 401 arm |
| `red-soft` | `#2F1517` | 1.09 | 1.03 | 1.05 | 1.11 | error tile, red pills |
| `violet` | `#B48CFF` | 7.14 | 6.75 | 6.22 | 5.85 | structure: hubs, traits, polymorphism, edges that point back up |
| `violet-soft` | `#231A3D` | 1.13 | 1.06 | 1.01 | 1.07 | violet pills |
| `syn-kw` | `#B79CFF` | 8.09 | 7.65 | 7.04 | 6.63 | keywords, weight 500 |
| `syn-str` | `#8FD9A8` | 11.12 | 10.51 | 9.68 | 9.11 | strings |
| `syn-num` | `#F5B57A` | 10.34 | 9.77 | 9.00 | 8.47 | numbers |
| `syn-com` | `#808AA2` | 5.34 | 5.05 | 4.65 | 4.37 | comments; ≥ 4.5 on card, the hovered line (`raised` 4.65) and the hot line (`primary-soft` 4.55) |
| `syn-type` | `#7FCFE6` | 10.51 | 9.94 | 9.15 | 8.61 | types |
| `syn-punct` | `#8C96AB` | 6.20 | 5.86 | 5.40 | 5.08 | punctuation and operators |

Pill text on its own soft fill: primary-ink 9.42 · cyan 8.59 · green 8.19 · amber 8.41 · red 5.88 · violet 6.32.

### 3.2 Aliases (15)

They exist so the shared A/B/C helpers draw on D boards; code should use the token names.
`paper` = panel · `paper-2` = raised · `press` = raised · `press-2` = line · `ink` = fg · `ink-2` = fg-2 ·
`ink-3` = fg-3 · `ink-4` = fg-4 · `rule` = line-strong · `rule-soft` = line · `rule-faint` = line-faint ·
`accent` = primary · `accent-soft` = primary-soft · `accent-line` = primary-line · `code-comment` = syn-com.

### 3.3 Rules

- **One meaning per accent** (D-F1): primary = you are here; cyan = data moving; green = proven; amber =
  conditional or stale; red = failure only, never decoration; violet = structure.
- A state never rests on colour alone: selection adds a border, focus a ring, hot a fill + border + glow,
  dimming an opacity (§9).
- Charts on Start use the accents as a **categorical** series palette (nodes: cyan, primary, violet, amber,
  green, fg-4; edges: cyan, line-strong, primary, violet, green, fg-4). The one-meaning rule governs UI state,
  not chart series.
- Raw hexes are only allowed inside gradients and the canvas PNG (§4); every fill and stroke elsewhere references
  a library colour.

### 3.4 AA corrections made on 2026-10-02 (after the style was confirmed)

Computing the D-F1 contrast table exposed three pairs below 4.5:1. The boards were redrawn with:

| What | Was | Now | Ratio was → now |
|---|---|---|---|
| `fg-3` (captions and meta on cards, inputs, overlays) | `#737E94` | `#838DA0` | overlay 3.70 → 4.52, raised 3.93 → 4.80, card 4.27 → 5.21 |
| `syn-com` (comments) | `#5D6880` | `#808AA2` | card 3.12 → 5.05, raised → 4.65, primary-soft → 4.55 |
| Primary button and the selected flow badge (white text) | GRAD.brand `#7C93FF → #A07CFF` | GRAD.button `#4465FF → #7F4EFF` | 2.80 / 3.07 → 4.61 / 4.73 |

GRAD.brand keeps its brighter stops everywhere no text sits on it (bars, indicators, selected strokes, brand
tile). New tokens `primary-deep`, `primary-2-deep`, `on-primary`, `green-deep` name the gradient stops that
used to be raw hexes.

### 3.5 Known gaps, left as drawn

- `fg-4` (2.42 on panel) carries line numbers, disabled controls and hints. Same choice as the upstream spec's
  `--ink-4`: a known gap, not text anyone must read to use the tool.
- Component boundaries are below the 3:1 non-text ratio (`line` 1.28 on panel, input fill `raised` 1.14). An input
  is identified by its fill, icon and placeholder; focus adds `primary-line` + SH.glow 25 %.

## 4. Effects — tokens, never per-screen

| Token | Value | Used by |
|---|---|---|
| SH.card | drop-shadow 0 6 20 0 `#000000` 32 % | cards on canvases, the code card, map nodes, flow steps |
| SH.pop | 0 16 40 0 `#000000` 50 % | popovers, the search palette, toolbars, legends, minimap, tooltips, the phone sheet |
| SH.glow(op) | 0 0 18 0 `primary` @ op | selected outline row 20 %, origin row 22 %, current hop / focused input 25 %, hot row 30 %, primary button + brand 35 %, selected flow step 40 %, focus ring + selected map or screen node 45 %, step badge / hierarchy focus tile / the selected screen's back edges 50 %, hovered primary button 55 % |
| SH.glowCyan(op) | 0 0 8 0 `cyan` @ op | flow links 45 %, map edges touching the selection 55 %, Screens and Steps focus-path lines 60 %, hot connector 80 %, hot port 90 % |
| SH.glowGreen(op) | 0 0 8 0 `green` @ op | index-status dot 60 %, entry dot 80 % |
| GRAD.brand(dir) | linear `primary` → `primary-2` in the shape's unit box; `h`: (0,0.5)→(1,0.5), any other dir: (0,0)→(1,1) — on a 2–3 px bar that reads as vertical | nav and tab indicators, origin and hot-line bars (2 px), selected stroke of map nodes and flow steps (1.5 px, inner), brand tile, size bars, scroll thumb |
| GRAD.button(dir) | linear `primary-deep` → `primary-2-deep` | under `on-primary` text only: primary buttons, the selected flow badge |
| GRAD.data | linear `cyan` → `primary` | weight bars, confidence bars, use bars, reason bars, stat bars |
| GRAD.green | linear `green-deep` → `green` | test bars, the test-files stat bar |

Canvas background (Map, Flow, Screens, Steps; D-F1 sample): a PNG drawn at 2× — base `#10131A`, a dot grid of
pitch 20 px, radius 1.05 px, `#2C3445`; two ambient glows (`#3A4CA8` centred at 10 %, 6 % with r = 0.32 W;
`#553F9E` at 95 %, 98 % with r = 0.26 W), Gaussian-blurred by 0.10 W and blended at 35 %. In code: `panel`
plus two radial gradients and a CSS dot pattern; the base is 1 step off `panel` (`#10131A` vs `#10141B`) and
should simply be `panel`.

## 5. Type — `D · Type`, 20 roles + 20 aliases = 40 typographies

Inter for the interface, JetBrains Mono for code, names and numbers (both OFL). Line height is the multiplier
the asset stores; px is size × multiplier.

| role | family | size | line height (×) | weight | used for |
|---|---|---|---|---|---|
| `display` | Inter | 24 | 31.92 (1.33) | 600 | project name on Start |
| `h1` | Inter | 20 | 28 (1.4) | 600 | view titles (Flow, Dead code), foundation titles |
| `h2` | Inter | 15 | 22.05 (1.47) | 600 | card titles in empty and error states, board titles |
| `label` | Inter | 13 | 17.94 (1.38) | 600 | island and card titles |
| `body` | Inter | 13 | 20.02 (1.54) | 400 | docstrings, prose |
| `body-500` | Inter | 13 | 20.02 (1.54) | 500 | project switch, palette result |
| `small` | Inter | 12 | 15.96 (1.33) | 400 | crumbs, placeholders, pills, callouts |
| `small-500` | Inter | 12 | 15.96 (1.33) | 500 | buttons, tone pills, state labels |
| `micro` | Inter | 10.5 | 13.97 (1.33) | 600 | section labels, upper case, letter-spacing 0.6 |
| `caption` | Inter | 11 | 13.97 (1.27) | 400 | meta lines, notes, legends |
| `big-num` | Inter | 22 | 27.94 (1.27) | 600 | stat values |
| `title-mono` | JetBrains Mono | 20 | 28 (1.4) | 600 | symbol title |
| `code` | JetBrains Mono | 12.5 | 20 (1.6) | 400 | source lines |
| `code-500` | JetBrains Mono | 12.5 | 20 (1.6) | 500 | keywords |
| `mono` | JetBrains Mono | 12 | 15.96 (1.33) | 400 | trail hops, table calls |
| `mono-500` | JetBrains Mono | 12 | 15.96 (1.33) | 500 | row names, file names |
| `mono-sm` | JetBrains Mono | 11 | 13.97 (1.27) | 400 | paths, line refs, hexes, conditions |
| `kbd` | JetBrains Mono | 10.5 | 13.97 (1.33) | 500 | key caps |
| `tile` | JetBrains Mono | 10 | 12 (1.2) | 700 | kind-tile letters |
| `lineno` | JetBrains Mono | 11 | 20 (1.818) | 400 | line numbers (on the 20 px code grid) |

Aliases (same values under the shared names): `ui` = body · `ui-500` = body-500 · `doc` = body · `body-sm` =
small · `meta` = caption · `h1-sans` = h1 · `title` = title-mono · `trail` = mono · `chip` = mono-sm ·
`glyph` = tile. Own sizes: `hint` 11.5/14.95 · `badge` 11.5/14.95 500 · `name` 12.5/16.25 mono · `name-600`
13/16.9 600 mono · `sig` 12/18 mono · `pill` 10.5/13.97 500 mono · `map` 13/16.9 600 mono · `window` 12/19 mono ·
`window-no` 11/19 mono · `glyph-sans` 10/12 600.

Mono advance is 0.6 em, so 12.5 px code is a 7.5 px column grid: capsules, ports and truncation are arithmetic.
Text never wraps inside code; a long line ends in `…` at `floor((card width − gutter − 34) / 7.5)` characters.

## 6. Icons

Lucide 0.544 (ISC), 62 names (`D-F2` lists them). `stroke-width` 1.5 on the 24 viewBox, round caps and joins —
1 px at 16, 0.875 at 14, 0.75 at 12. Sizes: 12 inline, 14 in pills and chevrons, 16 in the UI, 20 on the phone
tab bar. Resting colour `fg-3`; in a tone pill the tone; active nav `primary-ink`.

## 7. Components

| Component | Geometry | Style |
|---|---|---|
| Kind tile | size 18 / 20 / 22 / 26 / 28, r = round(0.3 × size); letter `tile` centred | soft fill of the kind colour: function, component, module `cyan`; method `primary`; struct, class, union, type_alias `green`; enum, enum_member, route `amber`; trait, interface, protocol `violet`; constant, variable, property, field, namespace, file `fg-2` on `raised`. file and module draw a 12 px icon (`file-code-2`, `package`) instead of a letter |
| Pill | 22 high (18–24 where noted), r = h / 2, padding 9, icon 14 + gap 6, dot 6 + gap 6 | neutral `raised` + `fg-2`; tone `X-soft` + `X` text (primary: `primary-ink`); optional 1 px border (`line` or `X-line`) |
| Key cap | 18 high, width max(18, text + 10), r 5 | `overlay` + `line-strong`, `kbd` `fg-2` |
| Button | 30 high (32–34 on Start, sheets), width text + 24 (+ 22 with icon 16), r 8 | primary GRAD.button + SH.glow 35 %, `on-primary`; secondary `raised` + `line`, `fg`; ghost no fill, `fg-2`; states §9 |
| Icon button | 28 / 30 / 32 / 36 / 40 square, r 8, icon 16 centred | rest no fill `fg-3`; bordered 1 px `line`; active `primary-soft` + `primary-ink` |
| Input | 32 high r 8 (command 36 high r 10) | `raised` + `line`; icon 16 at 12, text at 36; focus `primary-line` + SH.glow 25 % |
| Stat tile | h 64–76, r 10, label micro at 14,12, value `big-num` at 14,28 | `card` + `line`; bar track 4 px `raised` at h − 14, fill GRAD.data (GRAD.green for tests) proportional to the scale |
| Callee row (Calls rail) | 44 high, r 10, inset 12 | `card` + `line-faint`; tile 22 at 11,11; name `mono-500` at 42,6; meta `caption` `fg-3` at 42,24 (`same file · :121`) |
| Caller row (Called by rail) | 46 high (origin 64), r 10, inset 10, gap 6; grouped under a `mono-sm` path with a folder icon and a count | tile 22 at 12,12; name `mono-500` at 44,7; `calls` caption + call-site pill `:24` (mono-sm, 18 high); origin: `primary-soft` + `primary-line` + SH.glow 22 % + 2 px GRAD.brand bar at 0,10, `you came from here` in `primary-ink` |
| Tests fold | 44 high closed, r 10 | `raised` + `line`; flask 16 `green`; `Tests` body-500 + `103 calls · 32 files` caption; opened: file rows 26 apart, `showing 8 of 32 files · scroll for the rest` |
| Code card | x 24, y 176 (216 with the drift callout), w = island − 48, h = 40 + lines × 20 + 14, r 12 | `card` + `line` + SH.card; header 40 (file icon, `mono-500` name, `mono-sm` range, cyan `N calls · 1 hot` pill, copy button) and a `line-faint` rule; lines from y 47, 20 high; gutter 48 (numbers `lineno` `fg-4` right-aligned in 34); code `code` at x 48 |
| Call capsule | behind each callee name: x = 48 + col × 7.5 − 3, h 18, w = len × 7.5 + 6, r 5 | `cyan-soft` at 85 %; on the hot line 100 % + 1 px `cyan-line`; the name itself `cyan` |
| Port | 7 px dot at x = card width − 18, centred on the line | `cyan`; hot + SH.glowCyan 90 % |
| Hot line | full card width × 20 | `primary-soft` + 2 px GRAD.brand bar at x 0 |
| Connector | cubic from the port's right edge to the row's left edge, control points at the horizontal midpoint | `cyan-line` 1.5; hot `cyan` 2 + SH.glowCyan 80 %, drawn last |
| Search palette (empty state) | under the command input, same x and width, y 52, h 176, r 12 | `overlay` + `line-strong` + SH.pop; result rows 40 high `card` + `line-faint` r 8; key hints at y 148; the page under it dims: `bg` at 60 % from y 56 |
| Tooltip (long name) | at 62,78 under the title, h 32, w = text + 24, r 8 | `overlay` + `line-strong` + SH.pop, `mono` `fg`; drawn last so it sits above the metric row |
| Callouts | 36–44 high, r 10 | drift: `amber-soft`, triangle-alert 16 `amber`, `small` `amber`; error card: `card` + `line` + SH.card, 40 tile `red-soft` r 12 with `circle-x` 20 `red`, title `h2`; sample: `raised` + `line`, `info` 16 `fg-3`, `caption`/`small` `fg-2` |
| Map node | 52 high, width by label, r 12, clip | `card` + `line` + SH.card; tile 22 at 12,15 (module); label `mono-500` at 44,8; meta `caption` at 44,27; weight bar 2 px GRAD.data at y 48, width ∝ dependent files |
| Flow step card | 190×168, r 12, gap 34 | `card` + `line` + SH.card; badge 22 r 11 at 12,12 (`raised`; selected GRAD.button + SH.glow 50 %, `on-primary`); tile 20 at 40,13; location `mono-sm` right-aligned; name `mono-500` at 12,46; crate caption at 12,64; `calls` micro at 12,88 + cyan pill 22 at 12,104; condition pill at 12,134 (amber with `route` icon, or neutral `always`) |
| Floating toolbar / legend / minimap | toolbar 276×36 (Map) or 200×36 at 14 from the top, 16 from the right; legend 34 high at 16 from the bottom-left; minimap 176×116 at the bottom-right | `overlay` + `line` + SH.pop, r 10 |

## 8. Views

Data on every board is real (§11). Recipes not repeated from §7.

- **D-01 Start.** Project island: tile 48 `raised` + `line` r 12 at 20,18 (`box` 20 `primary-ink`); name
  `display`; status pill `Current` (green, dot + glow); meta `small` `fg-3`; actions right-aligned, 32 high:
  Open the map (primary), Search, Read a flow. Cards: icon 16 at 18,18, title `label` at 42,16, sub caption at
  18,40 `fg-4`. Bar rows: label + value right-aligned, 4 px track `raised` at +20, pitch 36. Index composition:
  three stat tiles 66 high, stacked bars 8 high with a legend of 8 px swatches. Saved trails: rows 64 high, the
  renamed-hop row 126 with an `amber-soft` callout 60 high. Entry functions: rows 52 high with a ghost `Flow` button.
- **D-02 Symbol.** Left: `Called by 4`, filter input, caller groups, Tests fold, blast radius pinned to the bottom
  (two rows of stat tiles, scale `vs widest 2,339`). Centre: crumb, tile 28 + `title-mono` title + kind pills,
  icon actions 30, metric pills (Hub, callees, Tests reach, lines), doc `body` 2 lines, code card, Confidence
  legend + `+4 calls leave the index`. Right: `Calls 5`, `Leaves the index` card with std calls as bordered
  pills, callee rows anchored to their lines: `y = max(lineY − 22, previous + 52)`, so a row never sits above
  its line's predecessor. The hub pill appears at 40 direct callers or more (upstream rule).
- **D-03 File.** Left: files that depend on this one, rows 28 high pitch 30, count right-aligned. Centre: file
  header, outline card (rows 28 pitch 30; columns name / signature `mono-sm` `fg-4` / ← in count + 3 px GRAD.data
  bar / → out / line), the open symbol highlighted like an origin row. Right: `Depends on`, outside-the-index
  pills, `This file` stats.
- **D-04 Flow.** Header 64: tile 32 `primary-soft` with `workflow`, title `h1`, route select 36 (from → to, tiles
  18), stat pills, export buttons. Canvas 400 on the canvas PNG: six step cards at x 20 + 224·i, y 36; links
  `cyan` 1.5 with 8 px arrowheads + SH.glowCyan 45 %, `:line` labels in the gaps. Step detail 1320×156 at 20,226:
  title, location, the full condition as an amber pill 24 high, ±2 lines around the call (18 px lines) with the
  call line tinted. Every-hop table: columns at 20 / 64 / 430 / 1040 / 1140, header micro at 44 + rule at 64, rows
  40 apart from 72, the selected row `primary-soft` + `primary-line` at 85 %, confidence bar 56×4 + value +
  resolvedBy pill.
- **D-05 Map.** Crates as nodes in dependency rows (row pitch 96 = 52 + 44, gap 28, first row at 64); `ENTRY` and
  `FOUNDATIONS · DEPEND ON NOTHING BELOW` bands in `fg-4` micro. Edges: `line-strong`, width
  `min(3, 1 + log2(count) × 0.4)` at 70 % (35 % while a selection exists); edges touching the selection `cyan`,
  width `min(3.5, 1.2 + log2(count) × 0.45)`, 100 % + SH.glowCyan 55 %. Non-neighbours of the selection 45 %.
  Inspector: tabs (active `fg` + 2 px GRAD.brand underline), tile 32, three stat tiles, ranked lists with 3 px
  GRAD.data bars, `Open crate` (primary) + `Copy image`.
- **D-06 Type hierarchy.** Polymorphism callout 40 high (`violet-soft`, `zap` 16 `violet`, `body` `fg`); hierarchy
  card with rows 26 high indented 28 per level, tiles 20 (the focus tile + SH.glow 50 %, name `mono-500`
  `primary-ink`), guides `line-strong` 1.2 — solid from the supertrait to the focus, dashed `4 3` below; the
  supertrait outside the index (`Sync`) has its tile at 50 % and its name in `fg-3`; `+19 more implementations`
  button 28 high; members card; most-used members with 3 px GRAD.data bars.
- **D-07 Dead code.** `Internal only / Including exported` segmented control; groups per file with a size bar
  (GRAD.brand, 140 max); right island explains what the list leaves out, with reason bars (GRAD.data).
- **D-08 Screens / D-09 Steps.** Sample project (proshop_mern), labelled as such in the inspector and the status
  pill. Screens: route nodes 48 high (§7 map-node look, dashed frame for shared chrome), the focus path in `cyan`
  2 + SH.glowCyan 60 %, link markup `fg-2` dashed 5 3, back edges `violet` dashed 4 3, everything off the focus
  path at 22–35 %, the band nothing static reaches in `amber-soft` 55 % with a dashed amber frame; condition pills
  on the focus edges. Steps: anchor `POST /api/users/login`, a fork chip (`overlay` + 1 px `amber`, r 18), yes/no
  arms in `green`/`red`, dashed boxes for steps that leave the index; inspector lists the steps with their
  conditions.

## 9. States

### 9.1 Rules (D-F3)

| Element | rest | hover | keyboard selected | focus | hot / current | other |
|---|---|---|---|---|---|---|
| Row | `card` + `line-faint` | `raised` + `line` | 1 px `fg-2` border | selected + 2 px `primary` ring 4 px outside, r 13, SH.glow 45 % | `primary-soft` + `primary-line` + SH.glow 30 %, name `primary-ink` | origin: as hot at 22 % + GRAD.brand bar; dimmed 45 % (Map) / 55 % (Screens); loading: skeleton bars `raised`, radius h / 2 |
| Button (primary) | GRAD.button + SH.glow 35 % | SH.glow 55 % | — | ring 2 px `primary` 3 px outside, r 11, SH.glow 45 % | — | pressed: no glow; disabled: 40 %, no glow |
| Button (secondary) | `raised` + `line` | `overlay` + `line-strong` | — | ring as above | — | pressed `card`; disabled 40 % |
| Button (ghost) | none | `raised` | — | ring as above | — | pressed `overlay`; disabled 40 % |
| Input | `raised` + `line` | border `line-strong` | — | `primary-line` + SH.glow 25 % | — | filled: value in `fg` |
| Code line | — | `raised` | — | — | `primary-soft` + 2 px GRAD.brand bar, port SH.glowCyan 90 % | drift: capsules, ports, the hot line and connectors switch off |
| Nav item | no fill, icon `fg-3` | `raised` | — | not drawn | `primary-soft`, `primary-ink`, 3×20 GRAD.brand indicator | — |
| Trail hop | `raised` + `line` | — | — | — | `primary-soft` + `primary-line` + SH.glow 25 % | — |
| Map node | `card` + `line` + SH.card | — | — | — | selected: `primary-soft` + 1.5 GRAD.brand stroke + SH.glow 45 % | dimmed 45 %; island meta in `amber` |

Edges (how sure the graph is): calls `line-strong` 1–3 px by volume · touching the selection `cyan` 2.5 + glow 55 %
· code port → callee row `cyan-line` 1.5 · hot connector `cyan` 2 + glow 80 % · name-only, confidence < 0.6 `fg-3`
dashed 2 3 · synthesized `fg-2` dashed 6 3 (Symbol) / 5 3 (Screens) · points back up `violet` dashed 4 3 ·
dimmed 22–35 % · leaves the index: dashed box `fg-4` 4 3.

### 9.2 D-02s fragments

Each fragment is the D-02 Symbol board drawn in one state (`d-symbol.js` with `opts.state`), nested into a
clipping board at a crop of the 1440×900 variant. Captions `label` at fragment y − 40, notes `caption` `fg-3` at
y − 22.

| State | fragment id | at (board) | crop of the 1440×900 variant | what changes |
|---|---|---|---|---|
| loading | `cb2954bc-8298-80f6-8008-ba438989fdf5` | 40,110 | 0,0 1440×560 | skeleton rows in both rails and the code card; header pill `Reading IndexPaths::resolve from the index…` |
| drift | `cb2954bc-8298-80f6-8008-ba55c98b5a54` | 40,760 | 360,104 1080×640 | amber callout above the code card (which moves to y 216); decorations off; callee rows in source order with a note |
| large | `cb2954bc-8298-80f6-8008-ba43df9e1d40` | 1160,760 | 64,104 304×740 | Tests fold open: 8 of 32 files, the count of the rest; blast radius hidden |
| error | `cb2954bc-8298-80f6-8008-ba4426a6a29b` | 40,1560 | 0,0 1440×560 | error card in place of the code: what was found (extraction 19 vs 20), that nothing changed, `codegraph sync .` with copy, Retry |
| longname | `cb2954bc-8298-80f6-8008-ba58b6c330b7` | 40,2210 | 360,104 760×116 | a real 81-character test fn (`crates/codegraph-cli/tests/batch_m_uninit.rs:1022`, crumb shows that file): the title is cut at the measured width so it ends 12 px before its kind pills, and the pills end at least 12 px before the actions; full name in the tooltip; no metric row |
| empty | `cb2954bc-8298-80f6-8008-ba44a7457658` | 840,2210 | 446,0 588×252 | palette open on `kubernetes`: no match in 12,569 names, entry points offered, page dimmed |
| focus | `cb2954bc-8298-80f6-8008-ba4509946399` | 40,2560 | 812,312 628×588 | `lexical_normalize` keyboard-selected + focus ring; line 121 hovered; key hints `↑↓ move · ⏎ follow · ←→ rail · ⌫ back` |
| readonly | `cb2954bc-8298-80f6-8008-ba45520cf4e4` | 720,2560 | 680,56 760×48 | `--read-only`: Save trail is not offered; a bordered pill with `lock` says why, placed 12 px left of the leftmost action |

## 10. Responsive

| | Tablet D-10 (768×1024) | Phone D-11 (375×812) |
|---|---|---|
| Navigation | rail 56: brand 32 at 12,10; destinations 36×36 at x 10, y 58 + 42·i (7 shown); settings at y H−50 | tab bar 64 at the bottom (`rail` + `line-faint`): Start, Map, Symbol, Flow, More; icon 20, caption label; active `primary-ink` + 28×3 GRAD.brand indicator at the top |
| Top | project switch at 72; search icon button 32 + `Current` pill right | brand 30, symbol name `mono-500`, search + more icon buttons 32 |
| Trail | ribbon 64,52 696×36 with `+4` and the last two hops | a back chip (r 15, 30 high) with the previous hop + `trail · 6 hops` |
| Content | symbol island 64,96 440×920 + calls island 512,96 248×920; `Called by 4` becomes a header button; code card gutter 44; connectors as desktop (hot SH.glowCyan 80 %) | header (tile 26, qualified name, location), metric pills, segmented control Code / Called by 4 / Calls 5 (34 high, active `primary-soft` + `primary-line`), code card gutter 36 (truncated, scrolls) |
| Detail | callee rows 44 anchored to their lines; blast radius as two stat tiles | bottom sheet 375×206 at y 562 (`overlay` + `line` + SH.pop, r 18, grabber 36×4): `Calls on line 122`, the hot row, Open (primary) + Read as flow |

## 11. Data provenance

All numbers come from codegraph-rust's own index at `99a8adb` (v0.50.3 build, extraction version 12, indexed
2026-09-02): 438 files, 12,569 nodes, 40,752 edges, read with `sqlite3 …?mode=ro&immutable=1` plus the checked-out
sources (`extract_data.py` in the lane). The focus symbol is `IndexPaths::resolve`
(`crates/codegraph-core/src/index_paths.rs:109–130`); the flow is the daemon start chain
`run_foreground → … → IndexPaths::resolve` with the conditions read from the source around each call; the type
hierarchy is `LanguageSpec` (29 implementations); the file view is `index_paths.rs`.

Simplifications, recorded in the data notes: test detection = `tests/`, `benches/`, fixture paths plus
`#[cfg(test)]` items; Map edges = cross-crate calls at confidence ≥ 0.9 that agree with the Cargo dependency graph
(the 0.50.3 index also holds name-only matches that point against it), test code excluded; Dead code = a stand-in
for the viewer's rule (no incoming edge but `contains`, minus test code, language-called names and trait-impl
members), draft data rather than the shipped algorithm.

Screens and Steps use the proshop_mern sample (`bradtraversy/proshop_mern` @ `1d20b5b`, routes and the `authUser`
handler read from its sources), because codegraph-rust has no screens and route/navigation extraction is not
ported (the UI family is deferred). Both boards say so.

## 12. Deviations from the upstream spec (colbymchenry/codegraph v1.6.1, `docs/design/codegraph-ui-design-spec.md`)

Direction D was chosen by the owner as a deliberately more modern look; these are the conscious differences.

| Upstream rule | D |
|---|---|
| Paper/ink editorial, light and dark themes | dark only (`D · Nebula`); a light variant is an open question |
| Square corners everywhere, no shadows, no gradients | radius 8–18, shadow tokens SH.card / SH.pop, glow tokens, four gradient tokens (§4) |
| One oxblood accent for focus, selection and edges; amber only for the untested badge; drift never amber | six accents with one meaning each (§3.3); amber marks drift, WHEN conditions, the unreached band and modules nothing depends on |
| Near-monochrome syntax so edges are the only colour in code | coloured syntax (`syn-*`); call names `cyan` in capsules so calls still stand out |
| No tiny all-caps tracked labels | `micro` section labels, upper case, 10.5/600, +0.6 tracking |
| Archivo + IBM Plex Mono | Inter + JetBrains Mono |
| Top bar 48 with view tabs, trail bar 34; left rail 300 | icon nav rail 64 + command bar 56 with a ⌘K palette + trail ribbon 40; left rail 288; islands with 8 px gaps |
| Focus ring `outline: 2px solid accent; offset 1px` | 2 px `primary` ring 4 px outside (3 px on buttons) + SH.glow 45 % |
| Kind glyphs: 16 px hollow squares, ink letter | soft filled tiles coloured by kind (§7) |
| Dimmed map nodes 0.1, edges 0.06 | 45 % nodes, 35 % edges on the Map; 55 % / 22–35 % on Screens, so the route labels stay readable |
| Code font without ligatures | JetBrains Mono renders `->` as an arrow on the boards; open rule below |

## 13. Not drawn / open rules

- **Ligatures in code.** The boards show JetBrains Mono's contextual ligatures (`->` → `→`). Source fidelity
  argues for `font-variant-ligatures: none` in code, names and line refs; the owner decides.
- **Light variant** of D, and whether `prefers-color-scheme` switches it.
- **Breakpoints between 1440 and 768** (upstream's ≤ 1100 rail widths) are not drawn; nor are 1280 and 1920
  layouts.
- **Motion.** None is drawn. If added: hover/focus transitions ≤ 150 ms, glow fades, all disabled under
  `prefers-reduced-motion`.
- States drawn only for Symbol (D-02s). Loading, empty and error for Map, Flow, File, Type hierarchy, Dead code,
  Screens and Steps follow the same recipes and are not drawn.
- The untested badge (`No test reaches this within 3 caller hops`) is not on any D board; by §3.3 it would be an
  amber pill.
- Export (SVG/PNG of a flow or the map), the saved-trail editor, settings and the keyboard-shortcut sheet are not
  drawn.
- Start's stacked edge bar clips its last two series (implements 90, other 41) to 1 px and 0 px: a real 1440-wide
  card cannot show 0.3 % of 40,752 edges; the legend carries the numbers.

## 14. Penpot write status (2026-10-02, read back after the last write)

Page `D · Modern Tech` holds exactly the 15 boards of §1 at the listed positions and sizes, and D-02s holds the 8
fragments of §9.2 with their captions and notes. All 54 `D · Nebula` colours and 40 `D · Type` typographies exist
in the file's local library. Text: 0 unmeasured, 0 overflowing fixed boxes on every board and fragment.
Builders and helpers (`lib.js`, `lib-d.js`, `d-*.js`, `dump.js`) live in the lane outside any repository; the
plugin keeps `storage.build` and `storage.dump` for re-runs. Pages `00 Foundations`, `A · Technical Paper`,
`B · Graphite Workbench` and `C · Hybrid Focus` are untouched by the D work (see `INDEX.md`).

## 15. Dump grammar (`dumps/*.dump.txt`)

One line per shape, indent = depth, coordinates relative to the board (fragment-relative in `D-02s-*`, clipped to
the fragment's window):

```
name [type] x,y w×h fill=… stroke=…/w(o|c) r=… op=… shadow[dx dy blur spread #hex@op ; …] clip | "characters" family size/weight lhN lsN align grow
```

- `#838DA0{fg-3}` — the fill references library colour `fg-3`; a bare hex is a raw value (gradients, SVG strokes).
- `grad[linear c@op:offset,… x0,y0->x1,y1]` — a linear gradient; `image(2064x1560)` — the canvas PNG fill.
- `stroke=…/1` inner (Penpot's default), `/1o` outer, `/1c` centre; `@0.35` after a colour is its opacity.
- `[icon]` — a Lucide group collapsed to its box with the stored stroke; `[svg] paths=N` — an SVG group
  (connectors, edges, links) collapsed to its box with its stroke colours and `glow` when a path carries a shadow.
- Omitted: per-range text styles (syntax colours, bold spans), path data, dash patterns of SVG strokes (listed in
  §9.1), blur, blend modes.
