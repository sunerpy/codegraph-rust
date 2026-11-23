/**
 * Where the Steps picture is fitted: beside the open key, or on the whole
 * canvas. Pure — the view hands it the model, whether the key is open and the
 * stage's size.
 *
 * The canvas fits its picture once, when it mounts (`@xyflow/system`'s
 * `getViewportForBounds`): the zoom that fits the boxes into the stage less the
 * padding, clamped to `[minZoom, maxZoom]`, and then a shift by whatever a side
 * is short of its padding. A zoom the clamp RAISED is never checked against the
 * far side, so a picture whose fitting zoom is under the floor runs off the
 * stage. A floor of 0.2 — the canvas's own `minZoom` — keeps a picture of a
 * few boxes whole wherever it can be drawn whole at all; a picture that fits at
 * 0.4 or more is fitted exactly as before, because the floor only acts below
 * the zoom it would have had.
 */
import type { StepsModel } from './steps-model';

/** The stage the canvas fills: its client size, in px. */
export interface StageSize {
  width: number;
  height: number;
}

/** Up to this many boxes the key's room is kept free; more are fitted as the Screens view's. */
const FEW_BOXES = 24;
/** The room the open key takes: 440 on the left, 32 on the other sides. */
const KEY_SIDE = { left: '440px', top: '32px', right: '32px', bottom: '32px' } as const;
const KEY_SIDE_X = 440 + 32;
const KEY_SIDE_Y = 32 + 32;
/** The canvas's own `minZoom`: the lowest zoom a few boxes are drawn at. */
const FEW_BOXES_FLOOR = 0.2;

/**
 * The fit. A picture of a few boxes is centred — and the key, bottom left,
 * would sit on its second row; while the key is open it is fitted to the right
 * of the key instead, when the boxes fit there at the floor, and to the whole
 * stage when they do not (a canvas narrower than the key's room, a row too
 * wide for it). A picture of many boxes is fitted to the whole stage, as the
 * Screens view's — and a picture laid out by region may fit far out: the
 * regions and their captions are the overview, and the reader zooms into one,
 * where a 0.4 floor on a big screen's picture opened on a window torn out of
 * its middle.
 *
 * `stage` null — nothing measured yet — keeps the rule of before: the key's
 * room whenever the key is open, at a 0.4 floor.
 *
 * The per-side padding keeps its literal types (`as const`), because the
 * canvas types a side as `` `${number}px` `` — widened to `string` it silently
 * fails to typecheck against the very option it is written for.
 */
export function stepsFitOptions(model: StepsModel | null, keyOpen: boolean, stage: StageSize | null) {
  if (model === null || model.layout.nodes.length > FEW_BOXES) {
    return { padding: 0.1, maxZoom: 1, minZoom: model !== null && model.regions !== null ? 0.2 : 0.4 };
  }
  if (stage === null) {
    return keyOpen
      ? { padding: KEY_SIDE, maxZoom: 1, minZoom: 0.4 }
      : { padding: 0.1, maxZoom: 1, minZoom: model.regions !== null ? 0.2 : 0.4 };
  }
  if (keyOpen) {
    const box = bounds(model);
    const fits =
      box.width * FEW_BOXES_FLOOR <= stage.width - KEY_SIDE_X &&
      box.height * FEW_BOXES_FLOOR <= stage.height - KEY_SIDE_Y;
    if (fits) return { padding: KEY_SIDE, maxZoom: 1, minZoom: FEW_BOXES_FLOOR };
  }
  return { padding: 0.1, maxZoom: 1, minZoom: FEW_BOXES_FLOOR };
}

/** The boxes' bounding rectangle — what the canvas is handed to fit. */
function bounds(model: StepsModel): { width: number; height: number } {
  let left = Infinity;
  let top = Infinity;
  let right = -Infinity;
  let bottom = -Infinity;
  for (const node of model.layout.nodes) {
    left = Math.min(left, node.x);
    top = Math.min(top, node.y);
    right = Math.max(right, node.x + node.width);
    bottom = Math.max(bottom, node.y + node.height);
  }
  return left === Infinity ? { width: 0, height: 0 } : { width: right - left, height: bottom - top };
}
