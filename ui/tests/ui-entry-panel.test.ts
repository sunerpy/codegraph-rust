/**
 * The Entry points panel — the `buildEntryPanel` / `frameworkPhrase` half of
 * upstream `__tests__/ui-entrypoints-api.test.ts` (v1.6.1). Upstream feeds the
 * panel from a live server over two fixtures; here the same two payloads are
 * literals with the shapes those fixtures produce (a routed Go service and a
 * library with no routes), so the panel's rules are pinned without an index.
 * The server half runs in `crates/codegraph-ui/tests/entrypoints.rs`.
 */

import { describe, it, expect } from 'vitest';
import { buildEntryPanel, frameworkPhrase } from '../src/lib/entry-model';
import type { WireEntryPoints } from '../src/lib/api';

const list = <T>(items: T[], total = items.length) => ({
  total,
  shown: items.length,
  truncated: items.length < total,
  items,
});

const ROUTER = 'internal/transport/httpapi/router.go';

function route(method: string | null, path: string, handler: string, file: string, line: number, at: number) {
  return {
    url: method ? `${method} ${path}` : path,
    method,
    path,
    handler,
    handlerKind: 'method',
    file,
    line,
    handlerId: `method:${handler.toLowerCase()}`,
    routeFile: ROUTER,
    routeLine: at,
    routeId: `route:${ROUTER}:${at}`,
  };
}

const ROUTED: WireEntryPoints = {
  frameworks: ['go'],
  routes: {
    routed: true,
    routeCount: 4,
    items: list([
      route('POST', '/v1/payroll/cycles/{cycleID}/run', 'RunCycle', 'internal/transport/httpapi/payroll_handler.go', 34, 21),
      route('GET', '/v1/payroll/cycles/{cycleID}', 'GetCycle', 'internal/transport/httpapi/payroll_handler.go', 52, 22),
      route('GET', '/v1/payroll/cycles/{cycleID}/payslips', 'ListPayslips', 'internal/transport/httpapi/payroll_handler.go', 70, 23),
      route('GET', '/healthz', 'Healthz', ROUTER, 40, 24),
    ]),
  },
  files: list([]),
  tests: list([]),
  hubs: list([]),
  index: { lastIndexedAt: 1, files: 12 },
  timing: { elapsedMs: 3, cached: false },
} as unknown as WireEntryPoints;

const fileRef = (file: string, test = false) => ({
  id: `file:${file}`,
  kind: 'file',
  name: file.split('/').pop() as string,
  qualifiedName: file,
  file,
  line: 1,
  endLine: 8,
  language: 'typescript',
  test,
});

const LIBRARY: WireEntryPoints = {
  frameworks: [],
  routes: { routed: false, routeCount: 0, items: list([]) },
  files: list([{ ...fileRef('src/main.ts'), calls: 2, reaches: 1, dependents: 0 }], 1),
  tests: list([{ ...fileRef('__tests__/store.test.ts', true), reaches: 1, refs: 1 }]),
  hubs: list(
    [
      {
        id: 'function:insertNode',
        kind: 'function',
        name: 'insertNode',
        qualifiedName: 'insertNode',
        file: 'src/store.ts',
        line: 1,
        endLine: 3,
        language: 'typescript',
        test: false,
        dependents: 3,
      },
    ],
    2
  ),
  index: { lastIndexedAt: 1, files: 4 },
  timing: { elapsedMs: 2, cached: false },
} as unknown as WireEntryPoints;

describe('entry points on a routed service', () => {
  it('names the framework the route list came from', () => {
    expect(frameworkPhrase(ROUTED.frameworks)).toContain('go');
    expect(frameworkPhrase(['go', 'react'])).toBe('go and react');
    expect(frameworkPhrase(['a', 'b', 'c'])).toBe('a, b and c');
    expect(frameworkPhrase([])).toBe('');
  });

  it('groups the panel by the router file, with the handler in the meta line', () => {
    const panel = buildEntryPanel(ROUTED);
    const routes = panel.sections.find((s) => s.id === 'routes');
    expect(routes).toBeDefined();
    expect(routes?.groups).toHaveLength(1);
    expect(routes?.groups[0]?.path).toBe(ROUTER);
    expect(routes?.groups[0]?.rows).toHaveLength(4);
    expect(routes?.meta).toContain('go');

    const run = routes?.groups[0]?.rows.find((r) => r.method === 'POST');
    expect(run?.name).toBe('/v1/payroll/cycles/{cycleID}/run');
    expect(run?.meta).toBe('RunCycle · payroll_handler.go:34');
    expect(run?.target).toEqual({
      type: 'symbol',
      id: expect.any(String),
      name: 'RunCycle',
      kind: 'method',
    });
    // A route names a callable symbol, so it can start a flow.
    expect(run?.flowFrom).toBe('RunCycle');
  });
});

describe('entry points on a project with no routes', () => {
  it('says it is not a routed app instead of drawing an empty list', () => {
    const panel = buildEntryPanel(LIBRARY);
    expect(panel.sections.map((s) => s.id)).not.toContain('routes');
    expect(panel.empty).toBeNull();
    expect(panel.sections.length).toBeGreaterThan(0);
  });

  it('falls back to the file that runs something at module level', () => {
    const panel = buildEntryPanel(LIBRARY);
    const section = panel.sections.find((s) => s.id === 'files');
    expect(section?.title).toBe('Top-level files with calls');
    expect(section?.groups[0]?.path).toBe('src');
    expect(section?.groups[0]?.rows.every((r) => r.flowFrom === null)).toBe(true);
    expect(section?.groups[0]?.rows[0]?.target).toEqual({ type: 'file', path: 'src/main.ts' });
  });

  it('lists the tests by what they exercise', () => {
    const panel = buildEntryPanel(LIBRARY);
    const section = panel.sections.find((s) => s.id === 'tests');
    expect(section?.title).toBe('Tests');
    expect(section?.groups[0]?.rows[0]?.meta).toMatch(/^exercises \d+ files? · \d+ references?$/);
  });

  it('counts the tests exactly, and the derived lists as a floor', () => {
    const panel = buildEntryPanel(LIBRARY);
    expect(panel.sections.find((s) => s.id === 'tests')?.floor).toBe(false);
    expect(panel.sections.find((s) => s.id === 'files')?.floor).toBe(true);
  });
});
