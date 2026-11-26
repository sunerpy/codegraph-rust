/**
 * Ways on that wait at the same place are one way on (#2264's `mergeTails`),
 * checked on a body small enough to finish without the merge. The eighty
 * checks of `ui-program-model.test.ts` pin the cost; without the merge they
 * exhaust the worker before any assertion reports, so this file pins what the
 * merge means on its own.
 */

import { describe, it, expect } from 'vitest';
import { lineWords, orderGraph } from '../src/lib/program-model';
import type { WireArm, WireBlock, WireProgram } from '../src/lib/wire';

const arm = (when: string, body: WireBlock): WireArm => ({ when, ends: null, body });

/** The graph as `from → to` lines, each with what has to hold. */
function shape(root: WireBlock): string[] {
  const g = orderGraph({ root, truncated: 0 } as WireProgram, 'anchor');
  return g.edges.map((e) => `${e.from} → ${e.to}${e.when ? ` · ${lineWords(e)}` : ''}`);
}

describe('ways on that wait at the same place', () => {
  it('meet as one: an if/else whose arms draw nothing claims neither side', () => {
    // Both arms go on to `b`, so `b` follows `a` whatever `ready` is. Kept as
    // two ways, the line would read `ready || !ready`.
    const root: WireBlock = [
      { kind: 'step', step: 'a' },
      { kind: 'fork', form: 'if', on: 'ready', arms: [arm('ready', []), arm('!ready', [])] },
      { kind: 'step', step: 'b' },
    ];
    expect(shape(root)).toEqual(['anchor → a', 'a → b']);
  });
});
