/**
 * The type hierarchy tree the viewer draws — the layout half of upstream
 * `__tests__/type-hierarchy.test.ts` (v1.6.1), verbatim. The walk half (which
 * needs a real index) is ported to Rust beside the engine it covers:
 * `crates/codegraph-ui/tests/hierarchy.rs` and
 * `crates/codegraph-graph/tests/hierarchy_bounds.rs`.
 */

import { describe, it, expect } from 'vitest';
import {
  buildHierarchyModel,
  connectorPath,
  visibleHierarchy,
  HIER_FOLD_AT,
  HIER_GLYPH_X,
  HIER_INDENT,
  HIER_PORT_X,
  HIER_ROW_H,
} from '../src/lib/hierarchy-model';
import type {
  WireHierarchy,
  WireHierarchyNode,
  WireNodeDetail,
} from '../src/lib/wire';

// =============================================================================
// The tree the viewer draws
// =============================================================================

const FOCUS: WireNodeDetail = {
  id: 'focus',
  kind: 'interface',
  name: 'Clock',
  qualifiedName: 'Clock',
  file: 'src/clock.ts',
  line: 1,
  endLine: 3,
  language: 'typescript' as WireNodeDetail['language'],
  test: false,
  startColumn: 0,
  endColumn: 0,
  lines: 3,
};

function entry(
  name: string,
  depth: number,
  parentId: string,
  relation: 'extends' | 'implements' = 'implements'
): WireHierarchyNode {
  return {
    id: name,
    kind: 'class',
    name,
    qualifiedName: name,
    file: `src/${name}.ts`,
    line: 1,
    endLine: 2,
    language: 'typescript' as WireNodeDetail['language'],
    test: false,
    depth,
    parentId,
    relation,
    synthesized: false,
    hiddenSubtypes: 0,
  };
}

function hierarchyOf(
  ancestors: WireHierarchyNode[],
  descendants: WireHierarchyNode[],
  extra: Partial<WireHierarchy> = {}
): WireHierarchy {
  return {
    ancestors: {
      total: ancestors.length,
      shown: ancestors.length,
      truncated: false,
      items: ancestors,
    },
    descendants: {
      total: descendants.length,
      shown: descendants.length,
      truncated: false,
      items: descendants,
    },
    direct: descendants.filter((d) => d.depth === 1).length,
    implementers: descendants.filter((d) => d.depth === 1 && d.relation === 'implements').length,
    bounded: false,
    polymorphic: false,
    ...extra,
  };
}

describe('buildHierarchyModel', () => {
  it('puts the focus between the two halves, farthest ancestor at the top', () => {
    const model = buildHierarchyModel(
      hierarchyOf(
        [entry('Base', 2, 'Mid', 'extends'), entry('Mid', 1, 'focus', 'extends')],
        [entry('Sub', 1, 'focus', 'extends')]
      ),
      FOCUS
    );
    expect(model.rows.map((r) => r.node.name)).toEqual(['Base', 'Mid', 'Clock', 'Sub']);
    expect(model.focusIndex).toBe(2);
    expect(model.rows[2]!.side).toBe('focus');
  });

  it('indents each descendant level and leaves ancestors at zero', () => {
    const model = buildHierarchyModel(
      hierarchyOf([entry('Base', 1, 'focus', 'extends')], [
        entry('Sub', 1, 'focus', 'extends'),
        entry('SubSub', 2, 'Sub', 'extends'),
      ]),
      FOCUS
    );
    const indents = Object.fromEntries(model.rows.map((r) => [r.node.name, r.indent]));
    expect(indents.Base).toBe(0);
    expect(indents.Clock).toBe(0);
    expect(indents.Sub).toBe(HIER_INDENT);
    expect(indents.SubSub).toBe(HIER_INDENT * 2);
  });

  it('draws a descendant connector from its own parent row, not from the focus', () => {
    const model = buildHierarchyModel(
      hierarchyOf([], [entry('Sub', 1, 'focus', 'extends'), entry('SubSub', 2, 'Sub', 'extends')]),
      FOCUS
    );
    const rowOf = (name: string) => model.rows.findIndex((r) => r.node.name === name);
    const deep = model.connectors.find((c) => c.toIndex === rowOf('SubSub'))!;
    expect(deep.fromIndex).toBe(rowOf('Sub'));
    // Leaves the parent's glyph centre, meets the child's glyph.
    expect(deep.x).toBe(HIER_INDENT + HIER_PORT_X);
    expect(deep.toX).toBe(HIER_INDENT * 2 + HIER_GLYPH_X - 2);
  });

  it('never hangs a descendant off an ancestor row that shares its name', () => {
    // A cycle in generated code: `Loop` is both above and below the focus.
    const model = buildHierarchyModel(
      hierarchyOf([entry('Loop', 1, 'focus', 'extends')], [entry('Loop', 1, 'focus', 'extends')]),
      FOCUS
    );
    const descendantRow = model.rows.findIndex((r) => r.side === 'descendant');
    const connector = model.connectors.find((c) => c.toIndex === descendantRow)!;
    expect(connector.fromIndex).toBe(model.focusIndex);
  });

  it('carries the relation into the connector so implements can be dashed', () => {
    const model = buildHierarchyModel(
      hierarchyOf([], [entry('Impl', 1, 'focus', 'implements')]),
      FOCUS
    );
    expect(model.connectors[0]!.relation).toBe('implements');
  });

  it('claims a dispatch only when the payload says the type is polymorphic', () => {
    const plain = buildHierarchyModel(hierarchyOf([], [entry('A', 1, 'focus')]), FOCUS);
    expect(plain.headline).toBe('');

    const fan = buildHierarchyModel(
      hierarchyOf([], [entry('A', 1, 'focus')], { polymorphic: true, implementers: 9 }),
      FOCUS
    );
    expect(fan.headline).toContain('9 implementations');
    expect(fan.headline).toContain('Clock');
  });
});

describe('the fold', () => {
  const fan = (n: number, relation: 'extends' | 'implements' = 'implements') =>
    hierarchyOf(
      [],
      Array.from({ length: n }, (_, i) => entry(`Impl${i}`, 1, 'focus', relation))
    );

  it('does not fold a fan of exactly the threshold — a "+0 more" is not a fold', () => {
    const model = buildHierarchyModel(fan(HIER_FOLD_AT), FOCUS);
    expect(model.foldFrom).toBeNull();
    expect(model.foldCount).toBe(0);
  });

  it('folds the tail past the threshold and counts what it hid', () => {
    const model = buildHierarchyModel(fan(HIER_FOLD_AT + 5), FOCUS);
    expect(model.foldCount).toBe(5);
    expect(model.foldNoun).toBe('implementations');
    const folded = visibleHierarchy(model, false);
    expect(folded.rows.length).toBe(model.focusIndex + 1 + HIER_FOLD_AT);
    expect(visibleHierarchy(model, true).rows.length).toBe(model.rows.length);
  });

  it('never leaves a connector running into the fold', () => {
    const model = buildHierarchyModel(fan(HIER_FOLD_AT + 5), FOCUS);
    const folded = visibleHierarchy(model, false);
    for (const connector of folded.connectors) {
      expect(connector.toIndex).toBeLessThan(folded.rows.length);
      expect(connector.fromIndex).toBeLessThan(folded.rows.length);
    }
  });

  it('calls a family of subclasses subclasses, not implementations', () => {
    const model = buildHierarchyModel(fan(HIER_FOLD_AT + 2, 'extends'), FOCUS);
    expect(model.foldNoun).toBe('subclasses');
  });

  it('heights are the row count times the row height, with nothing measured', () => {
    const model = buildHierarchyModel(fan(HIER_FOLD_AT + 5), FOCUS);
    expect(visibleHierarchy(model, false).height).toBe(
      (model.focusIndex + 1 + HIER_FOLD_AT) * HIER_ROW_H
    );
    expect(visibleHierarchy(model, true).height).toBe(model.rows.length * HIER_ROW_H);
  });
});

describe('connectorPath', () => {
  it('is two straight runs and a corner, never a curve', () => {
    const path = connectorPath({
      fromIndex: 0,
      toIndex: 1,
      x: 26,
      toX: 38,
      relation: 'extends',
      synthesized: false,
    });
    expect(path).toBe(`M 26 ${HIER_ROW_H / 2} L 26 ${HIER_ROW_H + HIER_ROW_H / 2} L 38 ${HIER_ROW_H + HIER_ROW_H / 2}`);
    expect(path).not.toContain('C');
  });

  it('drops the horizontal run when the two rows share an indent', () => {
    const path = connectorPath({
      fromIndex: 0,
      toIndex: 1,
      x: 26,
      toX: 26,
      relation: 'implements',
      synthesized: false,
    });
    expect(path.match(/L/g)).toHaveLength(1);
  });
});

describe('the note under the tree', () => {
  it('says how much of the fan is on screen when it was capped', () => {
    const payload = hierarchyOf([], [entry('A', 1, 'focus')]);
    payload.descendants.total = 900;
    payload.descendants.truncated = true;
    const model = buildHierarchyModel(payload, FOCUS);
    expect(model.note).toContain('900');
  });

  it('says deeper subtypes exist when the walk stopped rather than the list', () => {
    const model = buildHierarchyModel(
      hierarchyOf([], [entry('A', 1, 'focus')], { bounded: true }),
      FOCUS
    );
    expect(model.note).toContain('Deeper subtypes');
  });

  it('is empty when the payload is the whole truth', () => {
    expect(buildHierarchyModel(hierarchyOf([], [entry('A', 1, 'focus')]), FOCUS).note).toBe('');
  });
});
