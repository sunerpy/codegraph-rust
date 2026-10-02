// The two direction D token sets, read out of `src/lib/theme.css` itself, held
// to the WCAG 2.x pairs docs/design/viewer-d.md declares (§3.1 Nebula, §3.6
// Daylight): every declared pair at 4.5:1 or better. `fg-4` is decorative in
// both sets (§3.5) and is not a declared pair.
//
// Also pinned: the four copies of the sets inside theme.css agree with each
// other, and the SVG export's literal palettes are the same colours.
import { readFileSync } from 'node:fs';
import { describe, it, expect } from 'vitest';
import { EXPORT_PALETTES } from '../src/lib/export-svg';

const CSS = readFileSync(new URL('../src/lib/theme.css', import.meta.url), 'utf8');

/** The `--name: #hex;` declarations of the first rule whose selector matches. */
function block(selector: RegExp): Record<string, string> {
  const match = selector.exec(CSS);
  if (!match) throw new Error(`no rule matching ${selector}`);
  const open = CSS.indexOf('{', match.index + match[0].length - 1);
  let depth = 0;
  let end = open;
  for (; end < CSS.length; end++) {
    if (CSS[end] === '{') depth++;
    else if (CSS[end] === '}' && --depth === 0) break;
  }
  const body = CSS.slice(open + 1, end);
  const tokens: Record<string, string> = {};
  for (const m of body.matchAll(/--([a-z0-9-]+):\s*(#[0-9a-fA-F]{6})\s*;/g)) {
    tokens[m[1] as string] = (m[2] as string).toLowerCase();
  }
  return tokens;
}

// The bare `:root {` set is Daylight; the explicit dark rule is Nebula.
const DAYLIGHT = block(/\n:root \{/);
const NEBULA = block(/:root\[data-theme='dark'\],\s*\n\[data-theme='dark'\] \{/);
const NEBULA_BY_OS = block(/:root:not\(\[data-theme='light'\]\) \{/);
const DAYLIGHT_IN_DARK_PAGE = block(/\n\[data-theme='light'\] \{/);

/** WCAG 2.x relative luminance of `#rrggbb`. */
function luminance(hex: string): number {
  const channel = (i: number): number => {
    const c = parseInt(hex.slice(1 + i * 2, 3 + i * 2), 16) / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * channel(0) + 0.7152 * channel(1) + 0.0722 * channel(2);
}

function contrast(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x) as [number, number];
  return (hi + 0.05) / (lo + 0.05);
}

const SURFACES = ['bg', 'rail', 'panel', 'card', 'raised', 'overlay'];
/** Text and accents that carry meaning on any surface (§3.1, §3.6). */
const TEXT = ['fg', 'fg-2', 'fg-3', 'primary', 'primary-2', 'primary-ink', 'cyan', 'green', 'amber', 'red', 'violet'];
/** Code sits on the card, the hovered line and the hot line only. */
const SYNTAX = ['syn-kw', 'syn-str', 'syn-num', 'syn-com', 'syn-type', 'syn-punct'];
const CODE_SURFACES = ['card', 'raised', 'primary-soft'];
/** A tone pill: its text on its own soft fill. */
const PILLS: Array<[string, string]> = [
  ['primary-ink', 'primary-soft'],
  ['cyan', 'cyan-soft'],
  ['green', 'green-soft'],
  ['amber', 'amber-soft'],
  ['red', 'red-soft'],
  ['violet', 'violet-soft'],
];
/** `on-primary` is the only text set on a gradient: GRAD.button's two stops. */
const BUTTON: Array<[string, string]> = [
  ['on-primary', 'primary-deep'],
  ['on-primary', 'primary-2-deep'],
];

const AA = 4.5;

function declaredPairs(): Array<[string, string]> {
  const pairs: Array<[string, string]> = [];
  for (const fg of TEXT) for (const bg of SURFACES) pairs.push([fg, bg]);
  for (const fg of SYNTAX) for (const bg of CODE_SURFACES) pairs.push([fg, bg]);
  return [...pairs, ...PILLS, ...BUTTON];
}

describe.each([
  ['D · Nebula (dark)', NEBULA],
  ['D · Daylight (light)', DAYLIGHT],
])('%s', (_name, tokens) => {
  it('declares all 39 tokens', () => {
    expect(Object.keys(tokens).length).toBeGreaterThanOrEqual(39);
    for (const [fg, bg] of declaredPairs()) {
      expect(tokens[fg], fg).toMatch(/^#[0-9a-f]{6}$/);
      expect(tokens[bg], bg).toMatch(/^#[0-9a-f]{6}$/);
    }
  });

  it.each(declaredPairs())('%s on %s holds 4.5:1', (fg, bg) => {
    const ratio = contrast(tokens[fg] as string, tokens[bg] as string);
    expect(ratio, `${fg} ${tokens[fg]} on ${bg} ${tokens[bg]}: ${ratio.toFixed(2)}`).toBeGreaterThanOrEqual(AA);
  });
});

describe('the token sets inside theme.css', () => {
  it('the OS-dark block is the explicit dark block, value for value', () => {
    expect(NEBULA_BY_OS).toEqual(NEBULA);
  });

  it('the light-inside-a-dark-page block is the bare :root set, value for value', () => {
    expect(DAYLIGHT_IN_DARK_PAGE).toEqual(DAYLIGHT);
  });

  it('pins the spec values that most often get mistyped', () => {
    // §3.4's AA corrections and §3.6's surfaces.
    expect(NEBULA['fg-3']).toBe('#838da0');
    expect(NEBULA['syn-com']).toBe('#808aa2');
    expect(NEBULA['primary-deep']).toBe('#4465ff');
    expect(DAYLIGHT.bg).toBe('#ebeef4');
    expect(DAYLIGHT.panel).toBe('#ffffff');
    expect(DAYLIGHT['primary-ink']).toBe('#3340aa');
  });
});

describe('the SVG export palettes', () => {
  /** §3.2: upstream's names and the D tokens they alias. */
  const ALIAS: Record<string, string> = {
    paper: 'panel',
    paper2: 'raised',
    press: 'raised',
    ink: 'fg',
    ink2: 'fg-2',
    ink3: 'fg-3',
    ink4: 'fg-4',
    ruleSoft: 'line',
    ruleFaint: 'line-faint',
    accent: 'primary',
    accentSoft: 'primary-soft',
    accentLine: 'primary-line',
    codeComment: 'syn-com',
  };

  it.each([
    ['light', DAYLIGHT],
    ['dark', NEBULA],
  ] as const)('the %s export paints the theme tokens', (theme, tokens) => {
    const palette = EXPORT_PALETTES[theme] as unknown as Record<string, string>;
    for (const [name, token] of Object.entries(ALIAS)) {
      expect(palette[name], `${theme} ${name} = ${token}`).toBe(tokens[token]);
    }
  });
});
