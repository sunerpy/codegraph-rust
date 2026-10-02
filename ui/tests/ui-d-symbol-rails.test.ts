// The direction D additions to the Symbol view's rails (docs/design/viewer-d.md
// §8 D-02): the "Filter callers" box over the Called by rail, and the row
// pitch the Calls rail is anchored with.
import { describe, it, expect } from 'vitest';
import {
  buildCallerRail,
  calleeRowTops,
  filterCallerRail,
  CALLEE_ROW_HEIGHT,
  CALLEE_ROW_GAP,
} from '../src/lib/symbol-model';
import type { WireRelation, WireSymbolPayload } from '../src/lib/api';

function nodeRef(over: Record<string, unknown> = {}): any {
  return {
    id: 'method:a',
    kind: 'method',
    name: 'load',
    qualifiedName: 'Service::load',
    file: 'src/service.ts',
    line: 10,
    endLine: 20,
    language: 'typescript',
    test: false,
    ...over,
  };
}

function caller(id: string, name: string, file: string, nodeOver: Record<string, unknown> = {}): WireRelation {
  return {
    edgeKinds: ['calls'],
    edges: [{ kind: 'calls', line: 4, col: 2 }],
    edgeCount: 1,
    lines: [4],
    confidence: 0.9,
    uncertain: false,
    synthesized: false,
    node: nodeRef({ id, name, qualifiedName: `${file}::${name}`, file, ...nodeOver }),
  } as WireRelation;
}

function payload(incoming: WireRelation[]): WireSymbolPayload {
  return {
    node: { ...nodeRef(), startColumn: 2, endColumn: 3, lines: 11 },
    ancestors: [],
    members: { total: 0, shown: 0, truncated: false, items: [] },
    incoming: { total: incoming.length, shown: incoming.length, truncated: false, items: incoming },
    outgoing: { total: 0, shown: 0, truncated: false, items: [] },
    typesUsed: [],
    counts: { callers: incoming.length, callees: 0, typesUsed: 0, fanIn: 0, fanOut: 0, members: 0, hub: false },
    tests: { reached: false, hops: null, fileCount: 0, files: [], exhaustive: true, hopsSearched: 3 },
    outsideIndex: { total: 0, byKind: {}, samples: [] },
    blast: null,
    drift: false,
  } as unknown as WireSymbolPayload;
}

const RAIL = buildCallerRail(
  payload([
    caller('f:1', 'index_paths', 'crates/daemon/src/paths.rs'),
    caller('f:2', 'watch_options_for_project', 'crates/watch/src/watcher.rs'),
    caller('m:3', 'start', 'crates/watch/src/watcher.rs'),
    caller('f:4', 'resolve_test', 'crates/core/tests/paths.rs', { test: true }),
  ])
);

describe('filterCallerRail', () => {
  it('returns the rail untouched for an empty or blank query', () => {
    expect(filterCallerRail(RAIL, '')).toBe(RAIL);
    expect(filterCallerRail(RAIL, '   ')).toBe(RAIL);
  });

  it('keeps the rows whose name matches, case-insensitively, and drops emptied groups', () => {
    const shown = filterCallerRail(RAIL, 'WATCH');
    expect(shown.groups.map((g) => g.file)).toEqual(['crates/watch/src/watcher.rs']);
    expect(shown.groups[0]?.rows.map((r) => r.relation.node.name)).toEqual([
      'watch_options_for_project',
      'start',
    ]);
  });

  it("matches a row by its file too, so a group can be found by its path", () => {
    const shown = filterCallerRail(RAIL, 'daemon/src');
    expect(shown.groups.map((g) => g.file)).toEqual(['crates/daemon/src/paths.rs']);
  });

  it('keeps the headline count: the filter narrows the rows, not the claim', () => {
    expect(filterCallerRail(RAIL, 'start').total).toBe(RAIL.total);
  });

  it('filters the tests fold by the same rule', () => {
    expect(RAIL.tests.rows.map((r) => r.relation.node.name)).toEqual(['resolve_test']);
    expect(filterCallerRail(RAIL, 'resolve').tests.rows.map((r) => r.relation.node.name)).toEqual([
      'resolve_test',
    ]);
    expect(filterCallerRail(RAIL, 'watch').tests.rows).toEqual([]);
  });

  it('leaves nothing behind when nothing matches', () => {
    const shown = filterCallerRail(RAIL, 'nothing-has-this-name');
    expect(shown.groups).toEqual([]);
    expect(shown.tests.rows).toEqual([]);
    expect(shown.uncertain).toEqual([]);
  });
});

describe('calleeRowTops', () => {
  it('pins the D-02 recipe: 44px rows, 52 apart at the least', () => {
    expect(CALLEE_ROW_HEIGHT).toBe(44);
    expect(CALLEE_ROW_HEIGHT + CALLEE_ROW_GAP).toBe(52);
  });

  it('centres each row on its line, y = max(lineY - 22, previous + 52)', () => {
    // Lines at 118, 120 and 121 of a body starting at 109: centres 20px apart.
    const centres = [200, 240, 260];
    expect(calleeRowTops(centres, 60)).toEqual([178, 230, 282]);
  });

  it('stacks an unanchored row under the one before it', () => {
    expect(calleeRowTops([null, 400, null], 60)).toEqual([60, 378, 430]);
  });
});
