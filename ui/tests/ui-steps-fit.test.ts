/**
 * Where the Steps picture is fitted (`src/lib/steps-fit.ts`), without a
 * browser.
 *
 * The canvas fits its picture once, when it mounts: `@xyflow/system`'s
 * `getViewportForBounds` takes the stage less the padding, works out the zoom
 * that fits the boxes into it, clamps that zoom to `[minZoom, maxZoom]` and
 * then only shifts the picture by the padding a side is short of. A zoom the
 * clamp raised is never checked against the far side, so a picture whose
 * zoom was raised runs off the stage. It stays inside exactly when the boxes
 * at `minZoom` fit the room the padding leaves, which is what these tests
 * assert: `bounds × minZoom ≤ room`.
 */
import { describe, it, expect } from 'vitest';
import { buildStepsModel, type StepsModel } from '../src/lib/steps-model';
import { stepsFitOptions, type StageSize } from '../src/lib/steps-fit';
import type { WireNodeRef, WireStep, WireStepLink, WireStepsPayload } from '../src/lib/wire';

/* ------------------------------------------------------------- fixtures -- */

function ref(name: string, file = 'src/server/users.routes.ts'): WireNodeRef {
  return { id: `function:${name}`, kind: 'function', name, qualifiedName: name, file, line: 6, endLine: 20, language: 'typescript', test: false };
}

const ENDPOINT: WireStep = {
  id: 'route:src/server/users.routes.ts:6:POST:/users',
  kind: 'screen',
  anchor: true,
  node: ref('createUser'),
  label: 'POST /users',
  sub: 'createUser · users.routes.ts',
  depth: 0,
  cut: null,
  screen: { path: 'POST /users', component: ref('createUser'), endpoint: true, inline: false },
};

function effect(label: string, sub: string, category: string): WireStep {
  return {
    id: `effect:function:createUser:${label}`,
    kind: 'effect',
    anchor: false,
    node: null,
    label,
    sub,
    depth: 1,
    cut: null,
    effect: { api: label, apis: [label], category, by: ref('createUser'), line: 9 },
  };
}

function link(from: WireStep, to: WireStep): WireStepLink {
  return { id: `${from.id} ${to.id}`, from: from.id, to: to.id, kind: 'effect', via: [], when: '', label: '', synthesized: false, uncertain: false, sites: [] };
}

function picture(effects: WireStep[]): StepsModel {
  const payload: WireStepsPayload = {
    anchor: ENDPOINT.node!,
    ambiguous: [],
    project: 'api',
    steps: [ENDPOINT, ...effects],
    links: effects.map((to) => link(ENDPOINT, to)),
    program: null,
    defaultView: 'tree',
    depth: 8,
    limit: 120,
    through: false,
    truncated: { steps: 0, hubs: 0, chrome: 0 },
    index: { lastIndexedAt: null, edges: 0, files: 1 },
    timing: { elapsedMs: 1 },
  };
  return buildStepsModel(payload);
}

/** The Express server's `POST /users` of W7-22's capture: one row of five effects under the endpoint. */
const EXPRESS = picture([
  effect('prisma.user.create({ data })', 'database · user · write', 'database'),
  effect("emailQueue.add('welcome', { userId })", 'queue', 'queue'),
  effect('transporter.sendMail({ to })', 'email · sendVerification', 'email'),
  effect('jwt.sign({ id }, process.env.JWT_SECRET, { expiresIn })', 'auth · signToken', 'auth'),
  effect('201', 'response · createUser', 'response'),
]);

/** Three boxes that fit beside the key at a zoom the 0.4 floor never touches. */
const SMALL = picture([effect('db.find()', 'database', 'database'), effect('200', 'response', 'response')]);

/** The anchor and 23 effects on one row: 24 boxes, too wide for the key's room even at 0.2. */
const WIDE_ROW = picture(
  Array.from({ length: 23 }, (_, i) => effect(`db.call${String(i).padStart(2, '0')}()x`, 'database', 'database'))
);

/** 25 boxes: past the few the key's room is for. */
const MANY = picture(Array.from({ length: 24 }, (_, i) => effect(`e${i}()`, 'database', 'database')));

const DESKTOP: StageSize = { width: 1032, height: 828 };
const PHONE: StageSize = { width: 374, height: 365 };

/* --------------------------------------------- xyflow's arithmetic, read -- */

/** The boxes' bounding rectangle — what the canvas is handed to fit. */
function bounds(model: StepsModel): { width: number; height: number } {
  const xs = model.layout.nodes.map((n) => n.x);
  const ys = model.layout.nodes.map((n) => n.y);
  const right = Math.max(...model.layout.nodes.map((n) => n.x + n.width));
  const bottom = Math.max(...model.layout.nodes.map((n) => n.y + n.height));
  return { width: right - Math.min(...xs), height: bottom - Math.min(...ys) };
}

/** One side's padding in px, as `@xyflow/system` 0.0.81 `parsePadding` reads it. */
function side(padding: number | string, size: number): number {
  if (typeof padding === 'number') return Math.floor((size - size / (1 + padding)) * 0.5);
  return Math.floor(parseFloat(padding));
}

type Fit = ReturnType<typeof stepsFitOptions>;

/** The room the padding leaves for the picture on this stage. */
function room(fit: Fit, stage: StageSize): { width: number; height: number } {
  const p = fit.padding;
  if (typeof p === 'number') return { width: stage.width - 2 * side(p, stage.width), height: stage.height - 2 * side(p, stage.height) };
  return {
    width: stage.width - side(p.left, stage.width) - side(p.right, stage.width),
    height: stage.height - side(p.top, stage.height) - side(p.bottom, stage.height),
  };
}

/** The zoom the canvas wants before it clamps: the boxes into the room. */
function wanted(model: StepsModel, fit: Fit, stage: StageSize): number {
  const b = bounds(model);
  const r = room(fit, stage);
  return Math.min(r.width / b.width, r.height / b.height);
}

/** True when the clamp cannot raise the zoom past what fits: the picture stays whole. */
function drawnWhole(model: StepsModel, fit: Fit, stage: StageSize): boolean {
  const b = bounds(model);
  const r = room(fit, stage);
  return b.width * fit.minZoom <= r.width && b.height * fit.minZoom <= r.height;
}

const KEY_SIDE = { left: '440px', top: '32px', right: '32px', bottom: '32px' };

/* ---------------------------------------------------------------- tests -- */

describe('the fixtures are the shapes the captures showed', () => {
  it('Express POST /users is one row about 1618 layout px wide, too wide for the key room at 0.4', () => {
    expect(EXPRESS.layout.nodes).toHaveLength(6);
    const b = bounds(EXPRESS);
    expect(b.width).toBeGreaterThan((DESKTOP.width - 472) / 0.4);
    expect(b.width).toBeCloseTo(1618.1, 0);
    expect(b.height).toBe(220);
  });

  it('the wide row is 24 boxes too wide for the key room even at 0.2, but not for the whole stage', () => {
    expect(WIDE_ROW.layout.nodes).toHaveLength(24);
    expect(bounds(WIDE_ROW).width).toBeGreaterThan((DESKTOP.width - 472) / 0.2);
    expect(MANY.layout.nodes).toHaveLength(25);
  });
});

describe('stepsFitOptions', () => {
  it('a few boxes beside the open key are drawn whole', () => {
    const fit = stepsFitOptions(EXPRESS, true, DESKTOP);
    expect(fit.padding).toEqual(KEY_SIDE);
    expect(fit.maxZoom).toBe(1);
    expect(drawnWhole(EXPRESS, fit, DESKTOP)).toBe(true);
  });

  it("the key's room is not reserved on a canvas narrower than it", () => {
    const fit = stepsFitOptions(EXPRESS, true, PHONE);
    expect(fit.padding).toBe(0.1);
    expect(drawnWhole(EXPRESS, fit, PHONE)).toBe(true);
  });

  it('a few boxes fill a narrow canvas whole when the key is closed', () => {
    const fit = stepsFitOptions(EXPRESS, false, PHONE);
    expect(fit.padding).toBe(0.1);
    expect(drawnWhole(EXPRESS, fit, PHONE)).toBe(true);
    // Express needs about 0.21 there.
    expect(fit.minZoom).toBeLessThanOrEqual(0.21);
  });

  it('a picture that fits today is fitted as today', () => {
    const fit = stepsFitOptions(SMALL, true, DESKTOP);
    expect(fit.padding).toEqual(KEY_SIDE);
    expect(fit.maxZoom).toBe(1);
    const zoom = wanted(SMALL, fit, DESKTOP);
    // Today's floor never touches it, so a lower floor changes nothing.
    expect(zoom).toBeGreaterThanOrEqual(0.4);
    expect(Math.min(Math.max(zoom, fit.minZoom), fit.maxZoom)).toBe(Math.min(Math.max(zoom, 0.4), 1));
  });

  it("more than 24 boxes keep upstream's fit", () => {
    for (const open of [true, false]) {
      expect(stepsFitOptions(MANY, open, DESKTOP)).toEqual({ padding: 0.1, maxZoom: 1, minZoom: 0.4 });
    }
    // A screen's picture laid out by region may fit far out (the regions are the overview).
    const regions: StepsModel = { ...MANY, regions: [{ id: 'r', label: 'R', x: 0, y: 0, width: 10, height: 10, entry: 'r' }] };
    expect(stepsFitOptions(regions, true, DESKTOP)).toEqual({ padding: 0.1, maxZoom: 1, minZoom: 0.2 });
  });

  it('a picture too wide for the key room even at 0.2 goes to the whole canvas', () => {
    const fit = stepsFitOptions(WIDE_ROW, true, DESKTOP);
    expect(fit).toEqual({ padding: 0.1, maxZoom: 1, minZoom: 0.2 });
    expect(drawnWhole(WIDE_ROW, fit, DESKTOP)).toBe(true);
  });

  it('without a stage to measure it falls back to the key-side fit of before', () => {
    expect(stepsFitOptions(EXPRESS, true, null)).toEqual({ padding: KEY_SIDE, maxZoom: 1, minZoom: 0.4 });
    expect(stepsFitOptions(EXPRESS, false, null)).toEqual({ padding: 0.1, maxZoom: 1, minZoom: 0.4 });
  });
});
