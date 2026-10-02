/**
 * The Flow strip and the Map, as a standalone SVG (design spec §3.9).
 *
 * This is the distribution loop: a flow pasted into a PR review, a map pasted
 * into a README. Both have to survive leaving the app — the reader has no
 * viewer, no hover, no side panel and no way to ask a follow-up question, so
 * what the picture says has to be everything it claims.
 *
 * ## Serialised from the layout, never scraped from the DOM
 *
 * The obvious way to do this is `html-to-image`: walk the rendered nodes, inline
 * every computed style, foreignObject the result. It was not taken. The strip
 * and the map are already **pure functions of a layout object** — `buildFlowLayout`
 * and `buildMapLayout` compute every rectangle, port and curve before anything
 * renders, and both are unit-tested without a browser. Serialising that object
 * gives an export that:
 *
 * - cannot disagree with the screen, because the same arithmetic produced both;
 * - contains real `<text>`, so an SVG in a README is selectable and scales,
 *   rather than a `foreignObject` GitHub's sanitiser drops on sight;
 * - costs no dependency, and works in a test with no DOM at all.
 *
 * The price is that every visual rule the components carry in CSS has to be
 * restated here as numbers. That is the one thing to know before changing a
 * card's padding or a module box's type scale: **it is stated twice**, in the
 * component's `<style>` and in this file, and the two have to move together.
 * The measurements that actually place things — heights, widths, columns — are
 * NOT restated; they are imported from the layout models.
 *
 * ## In the theme on screen, as literal hex
 *
 * Direction D (docs/design/viewer-d.md §3, §3.6) has two token sets, and the
 * export paints the one the reader is looking at: the caller passes `theme`
 * (the viewer passes the effective theme, System resolved). Light is the
 * default, upstream's reasoning for a file read on someone else's page. Either
 * way the colours are inlined as literal hex — there is no
 * `prefers-color-scheme` in a file someone else opens, and an export must not
 * depend on a stylesheet having loaded. `ui/tests/theme-contrast.test.ts`
 * checks both palettes against `lib/theme.css`, so the two cannot drift.
 *
 * ## Fonts are stacks, not bytes
 *
 * Per the spec: no embedding. The consequence is honest and worth stating —
 * an exported SVG opened on a machine without JetBrains Mono falls back
 * through the stack to the platform's own monospace, and a PNG rasterised
 * through an `<img>` always does, because an SVG loaded as an image may not
 * fetch a webfont. Every fallback in the mono stack advances at ~0.6em like
 * JetBrains Mono does, so the monospace grid the code windows depend on
 * survives the swap; only the letterforms change. Embedding the font would add
 * tens of kB of base64 to every export for nothing.
 *
 * Tested in `__tests__/ui-export-svg.test.ts`.
 */

import {
  CARD_WIDTH,
  CODE_LINE_HEIGHT,
  CODE_PADDING,
  END_CAP_LINE,
  END_CAP_PADDING,
  END_CAP_ROW,
  END_CAP_GAP,
  HEADER_HEIGHT,
  endCapText,
  type FlowCardLayout,
  type FlowEndCapLayout,
  type FlowLayout,
  type FlowLinkLayout,
} from './flow-model';
import {
  isEdgeVisible,
  moduleMetaLabel,
  type MapEdgeLayout,
  type MapLayout,
  type MapNodeLayout,
} from './map-model';
import { tokensByLine, type Token } from './highlight';
import { assignRefs, basename, type LineRef } from './symbol-model';
import { kindLetter, FILLED_KINDS } from './kinds';

/* ---------------------------------------------------------------- tokens -- */

/** The colours an export paints with, named as upstream's tokens were. */
export interface ExportPalette {
  paper: string;
  paper2: string;
  press: string;
  ink: string;
  ink2: string;
  ink3: string;
  ink4: string;
  ruleSoft: string;
  ruleFaint: string;
  accent: string;
  accentSoft: string;
  accentLine: string;
  codeComment: string;
}

/**
 * Both D token sets (`lib/theme.css`), as literal hex under upstream's names —
 * the §3.2 aliases: paper = panel, paper-2 = raised, press = raised, ink = fg,
 * ink-2..4 = fg-2..4, rule-soft = line, rule-faint = line-faint, accent =
 * primary, accent-soft = primary-soft, accent-line = primary-line,
 * code-comment = syn-com.
 */
export const EXPORT_PALETTES: Record<'light' | 'dark', ExportPalette> = {
  // D · Daylight (§3.6)
  light: {
    paper: '#ffffff',
    paper2: '#eef1f6',
    press: '#eef1f6',
    ink: '#1a2130',
    ink2: '#434d62',
    ink3: '#5f687c',
    ink4: '#98a1b3',
    ruleSoft: '#d8dde7',
    ruleFaint: '#e6e9f0',
    accent: '#4155ee',
    accentSoft: '#e9edff',
    accentLine: '#b9c3f6',
    codeComment: '#626a7d',
  },
  // D · Nebula (§3.1)
  dark: {
    paper: '#10141b',
    paper2: '#1b2130',
    press: '#1b2130',
    ink: '#e8ecf4',
    ink2: '#a9b2c4',
    ink3: '#838da0',
    ink4: '#4a5468',
    ruleSoft: '#232a38',
    ruleFaint: '#191f29',
    accent: '#7c93ff',
    accentSoft: '#1a2140',
    accentLine: '#3b4a8c',
    codeComment: '#808aa2',
  },
};

/** The light palette — what an export paints when no theme is asked for. */
export const EXPORT_COLORS: ExportPalette = EXPORT_PALETTES.light;

/**
 * The palette the drawing in progress paints with. Set by `flowSvg` and
 * `mapSvg` for the length of one call (they are synchronous), so the drawing
 * helpers below need no extra parameter.
 */
let C: ExportPalette = EXPORT_COLORS;

function withPalette<T>(theme: 'light' | 'dark' | undefined, draw: () => T): T {
  const previous = C;
  C = EXPORT_PALETTES[theme ?? 'light'];
  try {
    return draw();
  } finally {
    C = previous;
  }
}

export const MONO_STACK =
  "'JetBrains Mono', ui-monospace, 'SF Mono', Menlo, Consolas, monospace";
export const SANS_STACK =
  "'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', 'Helvetica Neue', Arial, sans-serif";

/** Clear space between the drawing and the edge of the image (design spec §3.9). */
export const EXPORT_PADDING = 24;
/** Space between the bottom of the drawing and the mark under it. */
export const MARK_GAP = 14;
export const MARK_SIZE = 11;
export const MARK_TEXT = 'CodeGraph';
/** Device-pixel multiplier for a rasterised export. */
export const EXPORT_SCALE = 2;

/** Advance of a monospace character, as a fraction of the font size. */
const MONO_ADVANCE = 0.6;
/** Rough advance of the sans stack, for truncation guards only. */
const SANS_ADVANCE = 0.53;

const monoWidth = (chars: number, size: number): number => chars * size * MONO_ADVANCE;

/* ------------------------------------------------------------ primitives -- */

export function esc(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

interface TextOptions {
  x: number;
  y: number;
  size: number;
  fill: string;
  family?: string;
  weight?: number;
  anchor?: 'start' | 'middle' | 'end';
  /** Keep runs of spaces — every code line needs this. */
  preserve?: boolean;
}

function textOpen(o: TextOptions): string {
  const parts = [
    `x="${round(o.x)}"`,
    `y="${round(o.y)}"`,
    `font-family="${o.family ?? SANS_STACK}"`,
    `font-size="${o.size}"`,
    `fill="${o.fill}"`,
  ];
  if (o.weight && o.weight !== 400) parts.push(`font-weight="${o.weight}"`);
  if (o.anchor && o.anchor !== 'start') parts.push(`text-anchor="${o.anchor}"`);
  if (o.preserve) parts.push('xml:space="preserve"');
  return `<text ${parts.join(' ')}>`;
}

function textEl(o: TextOptions, content: string): string {
  return `${textOpen(o)}${content}</text>`;
}

/** Numbers are rounded to a tenth: an SVG full of 17 decimals is unreadable. */
function round(n: number): number {
  return Math.round(n * 10) / 10;
}

function rect(
  x: number,
  y: number,
  w: number,
  h: number,
  attrs: {
    fill?: string;
    fillOpacity?: number;
    stroke?: string;
    strokeWidth?: number;
    dash?: string;
  } = {}
): string {
  const parts = [
    `x="${round(x)}"`,
    `y="${round(y)}"`,
    `width="${round(w)}"`,
    `height="${round(h)}"`,
    `fill="${attrs.fill ?? 'none'}"`,
  ];
  if (attrs.fillOpacity !== undefined) parts.push(`fill-opacity="${attrs.fillOpacity}"`);
  if (attrs.stroke) {
    parts.push(`stroke="${attrs.stroke}"`, `stroke-width="${attrs.strokeWidth ?? 1}"`);
    if (attrs.dash) parts.push(`stroke-dasharray="${attrs.dash}"`);
  }
  return `<rect ${parts.join(' ')} />`;
}

/**
 * Cut a string to what fits, with an ellipsis — the arithmetic twin of the
 * components' `text-overflow: ellipsis`.
 *
 * Done here rather than left to a clip path because a clip cuts mid-glyph and
 * says nothing about having cut; the ellipsis is the same admission the screen
 * makes. Clip paths are still applied over the top, as insurance against a
 * fallback font that advances wider than the stack's first choice.
 */
export function truncate(text: string, maxWidth: number, size: number, advance = MONO_ADVANCE): string {
  const per = size * advance;
  const fits = Math.floor(maxWidth / per);
  if (fits >= text.length) return text;
  if (fits <= 1) return text.slice(0, Math.max(0, fits));
  return `${text.slice(0, fits - 1)}…`;
}

/**
 * Greedy word wrap at a character count.
 *
 * The end cap's height was estimated by `endCapHeight` at
 * `ceil(length / END_CAP_CHARS)` lines, which is a *character* count and can
 * come out one line short of a real word wrap. The export therefore measures
 * its caps from these lines rather than from that estimate — the screen can let
 * a `min-height` box grow, an image cannot.
 */
export function wrapText(text: string, chars: number): string[] {
  const words = text.split(/\s+/).filter(Boolean);
  if (words.length === 0) return [''];
  const lines: string[] = [];
  let line = '';
  for (const word of words) {
    if (line.length === 0) line = word;
    else if (line.length + 1 + word.length <= chars) line = `${line} ${word}`;
    else {
      lines.push(line);
      line = word;
    }
  }
  lines.push(line);
  return lines;
}

/* ------------------------------------------------------------- the frame -- */

export interface ExportOptions {
  /**
   * Device-pixel multiplier written into the root `width`/`height`.
   *
   * The `viewBox` always stays in CSS pixels, so the geometry inside is
   * identical at every scale — 1 for a file someone will open, 2 for the raster
   * step, which then draws the image at its own intrinsic size and gets crisp
   * text instead of an upscaled bitmap.
   */
  scale?: number;
  /** A line of context under the drawing, left of the mark. */
  caption?: string | null;
  /** Which token set to paint with; light when omitted. */
  theme?: 'light' | 'dark';
}

interface Frame {
  /** Tight bounds of the drawing, before the export's own padding. */
  minX: number;
  minY: number;
  width: number;
  height: number;
}

/**
 * Wrap a drawing in paper, padding and the mark.
 *
 * `body` is emitted inside a translate that moves the drawing's own origin to
 * the padded corner, so every caller can keep working in layout coordinates.
 */
function document_(frame: Frame, defs: string, body: string, options: ExportOptions): string {
  const scale = options.scale ?? 1;
  const markRow = MARK_GAP + MARK_SIZE;
  const width = Math.max(1, Math.round(frame.width + EXPORT_PADDING * 2));
  const height = Math.max(1, Math.round(frame.height + EXPORT_PADDING * 2 + markRow));
  const markY = height - EXPORT_PADDING;

  const caption =
    options.caption == null || options.caption === ''
      ? ''
      : textEl(
          {
            x: EXPORT_PADDING,
            y: markY,
            size: MARK_SIZE,
            fill: C.ink3,
            family: MONO_STACK,
          },
          esc(
            truncate(
              options.caption,
              width - EXPORT_PADDING * 2 - monoWidth(MARK_TEXT.length + 3, MARK_SIZE),
              MARK_SIZE
            )
          )
        );

  return [
    `<svg xmlns="http://www.w3.org/2000/svg" width="${Math.round(width * scale)}" height="${Math.round(height * scale)}" viewBox="0 0 ${width} ${height}" font-family="${SANS_STACK}">`,
    defs === '' ? '' : `<defs>${defs}</defs>`,
    rect(0, 0, width, height, { fill: C.paper }),
    `<g transform="translate(${round(EXPORT_PADDING - frame.minX)},${round(EXPORT_PADDING - frame.minY)})">`,
    body,
    '</g>',
    caption,
    textEl(
      {
        x: width - EXPORT_PADDING,
        y: markY,
        size: MARK_SIZE,
        fill: C.ink3,
        family: MONO_STACK,
        anchor: 'end',
      },
      MARK_TEXT
    ),
    '</svg>',
  ]
    .filter(Boolean)
    .join('\n');
}

/* ------------------------------------------------------------ flow strip -- */

/** Card header: glyph box, name, `file:line` — mirrors `FlowCard.svelte`. */
const CARD_PAD_X = 12;
const GLYPH_SIZE = 16;
const GLYPH_TOP = 9;
const HEAD_BASELINE = 21.5;
const NAME_SIZE = 13;
const LOC_SIZE = 11;
/** `.ln` grid: `40px | 1fr | 6px`, line numbers right-aligned 10px inside. */
const GUTTER = 40;
const CODE_RIGHT = 6;
const CODE_SIZE = 12;
const CODE_BASELINE = 13.5;
const LINE_NO_SIZE = 11;
/** `.flabel text` — 11px mono, stacked 13px apart above the connector. */
const LINK_LABEL_SIZE = 11;

export interface FlowExportOptions extends ExportOptions {
  /** The picked flow's id — its cards keep the accent border, as on screen. */
  activeFlowId?: string | null;
  /** More than one path is drawn, so off-path cards dim. */
  showAll?: boolean;
}

/** Where a link leaves and arrives: the vertical middle of each card's side. */
function portOf(node: { x: number; y: number; width: number; height: number }): {
  right: [number, number];
  left: [number, number];
} {
  return {
    right: [node.x + node.width, node.y + node.height / 2],
    left: [node.x, node.y + node.height / 2],
  };
}

function flowCardSvg(card: FlowCardLayout, dimmed: boolean, current: boolean): string {
  const hop = card.hop;
  const source = hop.source;
  const out: string[] = [];
  const clip = `c${card.column}-${Math.round(card.y)}`;

  out.push(
    rect(card.x, card.y, card.width, card.height, {
      fill: C.paper,
      stroke: current ? C.accent : C.ruleSoft,
    })
  );

  // --- header --------------------------------------------------------------
  const letter = kindLetter(hop.node.kind);
  const gx = card.x + CARD_PAD_X;
  const gy = card.y + GLYPH_TOP;
  out.push(
    rect(gx, gy, GLYPH_SIZE, GLYPH_SIZE, {
      fill: FILLED_KINDS.has(hop.node.kind) ? C.press : 'none',
      stroke: C.ink3,
      dash: hop.node.kind === 'file' ? '2 2' : undefined,
    })
  );
  if (letter !== '') {
    const glyphSize = letter.length > 1 ? 8.5 : 9.5;
    out.push(
      textEl(
        {
          x: gx + GLYPH_SIZE / 2,
          y: gy + GLYPH_SIZE / 2 + glyphSize * 0.36,
          size: glyphSize,
          fill: C.ink2,
          family: MONO_STACK,
          weight: 500,
          anchor: 'middle',
        },
        esc(letter)
      )
    );
  }

  const loc = `${basename(hop.node.file)}:${hop.node.line}`;
  const locWidth = monoWidth(loc.length, LOC_SIZE);
  const nameX = gx + GLYPH_SIZE + 8;
  const nameRoom = card.x + card.width - CARD_PAD_X - locWidth - 8 - nameX;
  out.push(
    textEl(
      {
        x: nameX,
        y: card.y + HEAD_BASELINE,
        size: NAME_SIZE,
        fill: C.ink,
        family: MONO_STACK,
        weight: 600,
      },
      esc(truncate(hop.node.name, nameRoom, NAME_SIZE))
    ),
    textEl(
      {
        x: card.x + card.width - CARD_PAD_X,
        y: card.y + HEAD_BASELINE,
        size: LOC_SIZE,
        fill: C.ink3,
        family: MONO_STACK,
        anchor: 'end',
      },
      esc(loc)
    ),
    `<line x1="${round(card.x)}" y1="${round(card.y + HEADER_HEIGHT)}" x2="${round(card.x + card.width)}" y2="${round(card.y + HEADER_HEIGHT)}" stroke="${C.ruleFaint}" stroke-width="1" />`
  );

  // --- source window -------------------------------------------------------
  const lines = source?.lines ?? [];
  if (lines.length === 0) {
    const why = source?.drift
      ? 'Changed on disk after the last index sync — source is not shown.'
      : (source?.reason ?? 'Source outside this slice or this index.');
    out.push(
      textEl(
        {
          x: card.x + CARD_PAD_X,
          y: card.y + HEADER_HEIGHT + 6 + CODE_BASELINE,
          size: CODE_SIZE,
          fill: C.ink3,
        },
        esc(truncate(why, card.width - CARD_PAD_X * 2, CODE_SIZE, SANS_ADVANCE))
      )
    );
    return `<g${dimmed ? ' opacity="0.4"' : ''}>${out.join('')}</g>`;
  }

  const refs = new Map<number, LineRef[]>();
  const callRef = hop.callRef;
  if (callRef) {
    refs.set(callRef.line, [
      {
        ident: callRef.name,
        col: callRef.col,
        targetId: callRef.targetId,
        uncertain: false,
        outside: false,
        title: '',
      },
    ]);
  }
  const tokens = tokensByLine(lines, source?.from ?? 1, source?.highlight);
  const textX = card.x + GUTTER;
  const textRoom = card.width - GUTTER - CODE_RIGHT;
  const maxChars = Math.floor(textRoom / (CODE_SIZE * MONO_ADVANCE));

  const body: string[] = [];
  lines.forEach((text, offset) => {
    const n = (source?.from ?? 1) + offset;
    const top = card.y + HEADER_HEIGHT + CODE_PADDING / 2 + offset * CODE_LINE_HEIGHT;
    const lineTokens: Token[] = tokens.get(n) ?? [{ cls: 'other', text, col: 0 }];
    const claimed = assignRefs(lineTokens, refs.get(n) ?? []);
    if (n === callRef?.line || n === card.stopLine) {
      body.push(
        rect(card.x, top, card.width, CODE_LINE_HEIGHT, { fill: C.accentSoft })
      );
    }
    body.push(
      textEl(
        {
          x: card.x + GUTTER - 10,
          y: top + CODE_BASELINE,
          size: LINE_NO_SIZE,
          fill: C.ink4,
          family: MONO_STACK,
          anchor: 'end',
        },
        String(n)
      )
    );

    // One <text> per line with a tspan per token: monospace flows naturally, so
    // nothing has to be positioned by column — which is also what keeps the
    // indentation intact under `xml:space="preserve"`.
    const spans: string[] = [];
    let used = 0;
    let underline: { from: number; length: number } | null = null;
    lineTokens.forEach((token, index) => {
      if (used >= maxChars) return;
      const room = maxChars - used;
      const cut = token.text.length > room;
      const shown = cut ? `${token.text.slice(0, Math.max(0, room - 1))}…` : token.text;
      const ref = claimed.get(index) ?? null;
      if (ref) underline = { from: used, length: shown.length };
      spans.push(tokenSpan(shown, token, ref !== null));
      used += token.text.length;
    });
    body.push(
      textEl(
        {
          x: textX,
          y: top + CODE_BASELINE,
          size: CODE_SIZE,
          fill: C.ink,
          family: MONO_STACK,
          preserve: true,
        },
        spans.join('')
      )
    );
    // The call site's underline, drawn rather than declared: `text-decoration`
    // on a tspan is not reliably honoured by SVG rasterisers, and this is the
    // one piece of colour in the window.
    if (underline !== null) {
      const u = underline as { from: number; length: number };
      const x1 = textX + monoWidth(u.from, CODE_SIZE);
      body.push(
        `<line x1="${round(x1)}" y1="${round(top + CODE_BASELINE + 3)}" x2="${round(x1 + monoWidth(u.length, CODE_SIZE))}" y2="${round(top + CODE_BASELINE + 3)}" stroke="${C.accentLine}" stroke-width="1" />`
      );
    }
  });

  out.push(`<g clip-path="url(#${clip})">${body.join('')}</g>`);
  return `<g${dimmed ? ' opacity="0.4"' : ''}>${out.join('')}</g>`;
}

/** A code token as a tspan — the near-monochrome ramp of design spec §2.2. */
function tokenSpan(text: string, token: Token, isRef: boolean): string {
  if (text === '') return '';
  const escaped = esc(text);
  if (isRef) return `<tspan fill="${C.accent}">${escaped}</tspan>`;
  switch (token.cls) {
    case 'comment':
      return `<tspan fill="${C.codeComment}">${escaped}</tspan>`;
    case 'string':
    case 'number':
      return `<tspan fill="${C.ink2}">${escaped}</tspan>`;
    case 'keyword':
      return `<tspan font-weight="500">${escaped}</tspan>`;
    default:
      return `<tspan>${escaped}</tspan>`;
  }
}

/** The cap's text as drawn rows, and the height they actually need. */
export interface CapRows {
  rows: Array<{ text: string; kind: 'lead' | 'form' | 'mono' | 'soft' | 'body'; gapBefore: boolean }>;
  height: number;
}

const END_CAP_CHARS_SANS = 32;
const END_CAP_CHARS_MONO = 26;

export function capRows(cap: FlowEndCapLayout): CapRows {
  const text = endCapText(cap.boundary);
  const rows: CapRows['rows'] = [];
  const push = (
    value: string,
    kind: CapRows['rows'][number]['kind'],
    gapBefore = false,
    chars = END_CAP_CHARS_SANS
  ): void => {
    wrapText(value, chars).forEach((line, i) =>
      rows.push({ text: line, kind, gapBefore: gapBefore && i === 0 })
    );
  };

  // On screen the bold lead and the sentence after it share one paragraph. At
  // 32 characters a line the lead fills one on its own anyway, so the export
  // gives it its own row and keeps the bold ink without a mid-line tspan.
  push('Where the graph stops.', 'lead');
  push(text.intro, 'body');
  for (const site of text.sites) {
    push(site.headline, 'form', true);
    if (site.key !== null) push(`key ${site.key}`, 'mono', false, END_CAP_CHARS_MONO);
    for (const note of site.notes) push(note, 'soft');
    if (site.candidateHeading !== null) {
      push(site.candidateHeading, 'soft');
      for (const candidate of site.candidates) {
        push(
          `${candidate.display}  ${basename(candidate.node.file)}:${candidate.node.line}`,
          'mono',
          false,
          END_CAP_CHARS_MONO
        );
      }
    } else if (site.candidateNote !== null) {
      push(site.candidateNote, 'soft');
    }
  }
  if (text.quiet !== null) push(text.quiet, 'soft', true);
  if (text.uncertainHeading !== null) {
    push(text.uncertainHeading, 'soft', true);
    for (const next of text.uncertain) {
      push(
        `${next.node.name}  ${next.confidence === null ? '' : next.confidence.toFixed(2)}`,
        'mono',
        false,
        END_CAP_CHARS_MONO
      );
    }
  }
  if (text.further !== null) push(text.further, 'body', true);
  if (text.missed !== null) push(text.missed, 'body', true);

  let height = END_CAP_PADDING * 2;
  for (const row of rows) {
    if (row.gapBefore) height += END_CAP_GAP;
    height += row.kind === 'mono' ? END_CAP_ROW : END_CAP_LINE;
  }
  return { rows, height: Math.round(height) };
}

function flowCapSvg(cap: FlowEndCapLayout, dimmed: boolean): string {
  const { rows, height } = capRows(cap);
  const out: string[] = [
    rect(cap.x, cap.y, cap.width, Math.max(cap.height, height), {
      fill: C.paper,
      stroke: C.ruleSoft,
      dash: '3 3',
    }),
  ];
  let y = cap.y + END_CAP_PADDING;
  const x = cap.x + END_CAP_PADDING;
  const room = cap.width - END_CAP_PADDING * 2;
  for (const row of rows) {
    if (row.gapBefore) y += END_CAP_GAP;
    const mono = row.kind === 'mono';
    const step = mono ? END_CAP_ROW : END_CAP_LINE;
    const fill =
      row.kind === 'form' || row.kind === 'lead'
        ? C.ink
        : row.kind === 'soft'
          ? C.ink3
          : C.ink2;
    out.push(
      textEl(
        {
          x,
          y: y + step * 0.75,
          size: mono ? 11.5 : 12,
          fill,
          family: mono ? MONO_STACK : SANS_STACK,
          weight: row.kind === 'lead' ? 600 : 400,
        },
        esc(truncate(row.text, room, mono ? 11.5 : 12, mono ? MONO_ADVANCE : SANS_ADVANCE))
      )
    );
    y += step;
  }
  return `<g${dimmed ? ' opacity="0.4"' : ''}>${out.join('')}</g>`;
}

function flowLinkSvg(
  link: FlowLinkLayout,
  from: { x: number; y: number; width: number; height: number },
  to: { x: number; y: number; width: number; height: number },
  dimmed: boolean
): string {
  const [sx, sy] = portOf(from).right;
  const [tx, ty] = portOf(to).left;
  const path =
    Math.abs(sy - ty) < 0.5
      ? `M${round(sx)},${round(sy)} L${round(tx)},${round(ty)}`
      : `M${round(sx)},${round(sy)} C${round((sx + tx) / 2)},${round(sy)} ${round((sx + tx) / 2)},${round(ty)} ${round(tx)},${round(ty)}`;

  const out: string[] = [
    `<path d="${path}" fill="none" stroke="${C.ink3}" stroke-width="1"${link.dash ? ` stroke-dasharray="${link.dash}"` : ''} />`,
  ];
  if (!link.cap) {
    out.push(
      `<polygon points="${round(tx - 10)},${round(ty - 4)} ${round(tx - 2)},${round(ty)} ${round(tx - 10)},${round(ty + 4)}" fill="${C.ink3}" />`
    );
  }
  const labelX = (sx + tx) / 2;
  const labelY = (sy + ty) / 2;
  link.labelLines.forEach((line, i) => {
    out.push(
      textEl(
        {
          x: labelX,
          y: labelY - 8 - (link.labelLines.length - 1 - i) * 13,
          size: LINK_LABEL_SIZE,
          fill: C.ink3,
          family: MONO_STACK,
          anchor: 'middle',
        },
        esc(line)
      )
    );
  });
  if (link.lineLabel) {
    out.push(
      textEl(
        {
          x: labelX,
          y: labelY + 17,
          size: LINK_LABEL_SIZE,
          fill: C.ink3,
          family: MONO_STACK,
          anchor: 'middle',
        },
        esc(link.lineLabel)
      )
    );
  }
  return `<g${dimmed ? ' opacity="0.4"' : ''}>${out.join('')}</g>`;
}

/** The Flow strip as a standalone SVG. */
export function flowSvg(layout: FlowLayout, options: FlowExportOptions = {}): string {
  return withPalette(options.theme, () => drawFlowSvg(layout, options));
}

function drawFlowSvg(layout: FlowLayout, options: FlowExportOptions): string {
  const showAll = options.showAll ?? false;
  const active = options.activeFlowId ?? null;
  const onActive = (flows: string[]): boolean => active === null || flows.includes(active);

  const boxes = new Map<string, { x: number; y: number; width: number; height: number }>();
  for (const card of layout.cards) boxes.set(card.id, card);
  const capHeights = new Map<string, number>();
  for (const cap of layout.endCaps) {
    const height = Math.max(cap.height, capRows(cap).height);
    capHeights.set(cap.id, height);
    boxes.set(cap.id, { x: cap.x, y: cap.y, width: cap.width, height });
  }

  const body: string[] = [];
  const defs: string[] = [];
  for (const link of layout.links) {
    const from = boxes.get(link.source);
    const to = boxes.get(link.target);
    if (!from || !to) continue;
    body.push(flowLinkSvg(link, from, to, showAll && !onActive(link.flows)));
  }
  for (const cap of layout.endCaps) {
    body.push(flowCapSvg(cap, showAll && !onActive(cap.flows)));
  }
  for (const card of layout.cards) {
    defs.push(
      `<clipPath id="c${card.column}-${Math.round(card.y)}">${rect(card.x, card.y + HEADER_HEIGHT, card.width, card.height - HEADER_HEIGHT)}</clipPath>`
    );
    body.push(
      flowCardSvg(card, showAll && card.step < 0, showAll && card.step >= 0)
    );
  }

  // Tight bounds over everything drawn, including the label stacks that sit
  // above and below a connector — the layout's own width/height cover the cards
  // but not a two-line synthesized label on the top row.
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  const grow = (x: number, y: number, w = 0, h = 0): void => {
    minX = Math.min(minX, x);
    minY = Math.min(minY, y);
    maxX = Math.max(maxX, x + w);
    maxY = Math.max(maxY, y + h);
  };
  for (const box of boxes.values()) grow(box.x, box.y, box.width, box.height);
  for (const link of layout.links) {
    const from = boxes.get(link.source);
    const to = boxes.get(link.target);
    if (!from || !to) continue;
    const y = (from.y + from.height / 2 + to.y + to.height / 2) / 2;
    grow((from.x + from.width + to.x) / 2, y - 8 - link.labelLines.length * 13);
    if (link.lineLabel) grow((from.x + from.width + to.x) / 2, y + 21);
  }
  if (!Number.isFinite(minX)) {
    minX = 0;
    minY = 0;
    maxX = 1;
    maxY = 1;
  }

  return document_(
    { minX, minY, width: maxX - minX, height: maxY - minY },
    defs.join(''),
    body.join('\n'),
    options
  );
}

/* -------------------------------------------------------------------- map -- */

const MODULE_NAME_SIZE = 13;
const MODULE_META_SIZE = 11;
const MODULE_PAD_X = 9;
const LAYER_LABEL_SIZE = 12;
/** How far a layer rule runs past the boxes it sits under. */
const LAYER_RULE_BLEED = 12;

export interface MapExportOptions extends ExportOptions {
  /** The selected module, so the export draws the same edges the screen does. */
  selected?: string | null;
}

function mapNodeSvg(node: MapNodeLayout, selected: boolean, dimmed: boolean): string {
  const module = node.module;
  const strokeWidth = selected ? 2 : 1;
  const out: string[] = [
    rect(node.x, node.y, node.width, node.height, {
      fill: selected ? C.press : C.paper,
      stroke: dimmed
        ? C.ink4
        : module.test
          ? C.ink3
          : C.ink,
      strokeWidth,
      dash: module.test ? '4 3' : undefined,
    }),
  ];
  const room = node.width - MODULE_PAD_X * 2;
  out.push(
    textEl(
      {
        x: node.x + MODULE_PAD_X,
        y: node.y + 17,
        size: MODULE_NAME_SIZE,
        fill: dimmed ? C.ink4 : C.ink,
        family: MONO_STACK,
        weight: 500,
      },
      esc(truncate(module.id, room, MODULE_NAME_SIZE))
    ),
    textEl(
      {
        x: node.x + MODULE_PAD_X,
        y: node.y + 31.5,
        size: MODULE_META_SIZE,
        fill: dimmed ? C.ink4 : C.ink3,
      },
      // `node.island`, matching the canvas: an exported map that counts a
      // module the screen said nothing depends on is a different picture.
      esc(truncate(moduleMetaLabel(module, node.island), room, MODULE_META_SIZE, SANS_ADVANCE))
    )
  );
  // The weight bar, same 3px inside the bottom edge as the canvas draws.
  if (node.weight > 0) {
    out.push(
      rect(node.x, node.y + node.height - 4, node.width * node.weight, 4, {
        fill: C.ink,
        fillOpacity: dimmed ? 0.1 : 0.3,
      })
    );
  }
  return out.join('');
}

/** A port along a box's edge: `x = left + width x (i+1)/(n+1)`. */
function portX(node: MapNodeLayout, handles: readonly string[], id: string): number {
  const index = handles.indexOf(id);
  const total = handles.length;
  if (index < 0 || total === 0) return node.x + node.width / 2;
  return node.x + (node.width * (index + 1)) / (total + 1);
}

function mapEdgeSvg(
  edge: MapEdgeLayout,
  from: MapNodeLayout,
  to: MapNodeLayout,
  hot: boolean
): string {
  const sx = portX(from, from.sourceHandles, edge.id);
  const sy = from.y + from.height;
  const tx = portX(to, to.targetHandles, edge.id);
  const ty = to.y;
  const midY = (sy + ty) / 2;
  const path = `M${round(sx)},${round(sy)} C${round(sx)},${round(midY)} ${round(tx)},${round(midY)} ${round(tx)},${round(ty)}`;
  if (edge.back) {
    return `<path d="${path}" fill="none" stroke="${C.accent}" stroke-opacity="0.6" stroke-dasharray="4 3" stroke-width="${round(edge.width)}" />`;
  }
  return `<path d="${path}" fill="none" stroke="${C.ink}" stroke-opacity="${hot ? 0.95 : 0.28}" stroke-width="${round(edge.width)}" />`;
}

/** The Map as a standalone SVG. */
export function mapSvg(layout: MapLayout, options: MapExportOptions = {}): string {
  return withPalette(options.theme, () => drawMapSvg(layout, options));
}

function drawMapSvg(layout: MapLayout, options: MapExportOptions): string {
  const selected = options.selected ?? null;
  const nodes = new Map(layout.nodes.map((n) => [n.id, n]));
  const neighbours =
    selected === null
      ? null
      : new Set<string>([
          selected,
          ...layout.edges.flatMap((e) =>
            e.source === selected ? [e.target] : e.target === selected ? [e.source] : []
          ),
        ]);

  // Bounds come from the boxes, and the layer rules are then drawn to fit THEM
  // — not to `layout.width`, which carries the canvas' own generous padding and
  // would run the hairlines past the edge of the image.
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const node of layout.nodes) {
    minX = Math.min(minX, node.x);
    minY = Math.min(minY, node.y);
    maxX = Math.max(maxX, node.x + node.width);
    maxY = Math.max(maxY, node.y + node.height);
  }
  if (!Number.isFinite(minX)) {
    minX = 0;
    minY = 0;
    maxX = 1;
    maxY = 1;
  }
  const ruleLeft = minX - LAYER_RULE_BLEED;
  const ruleRight = maxX + LAYER_RULE_BLEED;

  const body: string[] = [];

  // Layer rules first: they sit behind the boxes they explain, exactly as the
  // canvas' back viewport portal puts them.
  for (const row of layout.layers) {
    body.push(
      `<line x1="${round(ruleLeft)}" y1="${round(row.y)}" x2="${round(ruleRight)}" y2="${round(row.y)}" stroke="${C.ruleFaint}" stroke-width="1" />`
    );
    if (row.label !== null) {
      const y = row.index === 0 ? row.y + 40 : row.y - 36;
      body.push(
        textEl(
          { x: ruleLeft, y, size: LAYER_LABEL_SIZE, fill: C.ink3 },
          esc(row.label)
        )
      );
      minY = Math.min(minY, y - LAYER_LABEL_SIZE);
      maxY = Math.max(maxY, y + 4);
    }
  }

  for (const edge of layout.edges) {
    if (!isEdgeVisible(edge, selected)) continue;
    const from = nodes.get(edge.source);
    const to = nodes.get(edge.target);
    if (!from || !to) continue;
    body.push(mapEdgeSvg(edge, from, to, selected !== null && !edge.back));
  }
  for (const node of layout.nodes) {
    body.push(
      mapNodeSvg(node, selected === node.id, neighbours !== null && !neighbours.has(node.id))
    );
  }

  return document_(
    { minX: ruleLeft, minY, width: ruleRight - ruleLeft, height: maxY - minY },
    '',
    body.join('\n'),
    options
  );
}

/* ------------------------------------------------------------- filenames -- */

/**
 * A safe file stem — `codegraph-flow-execute-getfile`. No extension: the caller
 * adds one, because the same picture goes out as both `.svg` and `.png`.
 */
export function exportFilename(kind: 'flow' | 'map', label: string): string {
  const slug = label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 60)
    .replace(/-+$/g, '');
  return `codegraph-${kind}${slug ? `-${slug}` : ''}`;
}

export { CARD_WIDTH };
