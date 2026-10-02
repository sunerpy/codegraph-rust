/**
 * Graph links land on the right token — the `decodeLine` + `assignRefs` half of
 * upstream `__tests__/ui-highlight.test.ts` (v1.6.1). Upstream runs these over
 * its own highlighter's output; here each case reads the literal tokens the
 * codegraph-rs server sends for the same line (captured from
 * `codegraph_ui::highlight::highlight_lines`), so the frontend's claiming rule
 * is pinned against the classes the Rust classifier actually produces. The
 * classification itself is tested in `crates/codegraph-ui/tests/highlight.rs`.
 */

import { describe, it, expect } from 'vitest';
import { decodeLine, type Token } from '../src/lib/highlight';
import { assignRefs, type LineRef } from '../src/lib/symbol-model';

interface Highlighted {
  engine: string;
  grammar: string | null;
  classes: string[];
  lines: Array<Array<[number, string]>>;
}

const TEMPLATE: Highlighted = {"engine": "tree-sitter", "grammar": "typescript", "classes": ["other", "ident", "comment", "string", "keyword", "number", "type", "def"], "lines": [[[4, "const"], [0, " "], [1, "s"], [0, " = "], [3, "`n="], [1, "$"], [0, "{"], [1, "store"], [0, "."], [1, "size"], [0, "()}"], [3, " done`"], [0, ";"]]]};
const SVELTE: Highlighted = {"engine": "tree-sitter", "grammar": "typescript", "classes": ["other", "ident", "comment", "string", "keyword", "number", "type", "def"], "lines": [[[0, "<"], [1, "script"], [0, " "], [1, "lang"], [0, "=\""], [1, "ts"], [0, "\">"]], [[0, "  "], [4, "let"], [0, " "], [1, "count"], [0, " = "], [5, "0"], [0, ";"]], [[0, "</"], [1, "script"], [0, ">"]], [], [[0, "<"], [1, "button"], [0, " "], [1, "onclick"], [0, "={"], [1, "bump"], [0, "}>{"], [1, "count"], [0, "}</"], [1, "button"], [0, ">"]]]};
const PLAIN: Highlighted = {"engine": "plain", "grammar": null, "classes": ["other", "ident", "comment", "string", "keyword", "number", "type", "def"], "lines": [[[0, "  "], [1, "return"], [0, " "], [1, "this"], [0, "."], [1, "mutex"], [0, "."], [1, "withLock"], [0, "();"]]]};
const RECEIVER: Highlighted = {"engine": "tree-sitter", "grammar": "typescript", "classes": ["other", "ident", "comment", "string", "keyword", "number", "type", "def"], "lines": [[[0, "    "], [4, "return"], [0, " "], [1, "this"], [0, "."], [1, "indexMutex"], [0, "."], [1, "withLock"], [0, "("], [4, "async"], [0, " () => {"]]]};
const GO: Highlighted = {"engine": "tree-sitter", "grammar": "go", "classes": ["other", "ident", "comment", "string", "keyword", "number", "type", "def"], "lines": [[[0, "\t"], [1, "result"], [0, " := "], [1, "s"], [0, "."], [1, "repo"], [0, "."], [1, "FindByID"], [0, "("], [1, "ctx"], [0, ", "], [1, "id"], [0, ")"]]]};
const PYTHON: Highlighted = {"engine": "tree-sitter", "grammar": "python", "classes": ["other", "ident", "comment", "string", "keyword", "number", "type", "def"], "lines": [[[0, "    "], [4, "return"], [0, " "], [1, "self"], [0, "."], [1, "store"], [0, "."], [1, "join"], [0, "("], [1, "self"], [0, "."], [1, "store"], [0, "."], [1, "path"], [0, ")"]]]};
const COMMENT: Highlighted = {"engine": "tree-sitter", "grammar": "typescript", "classes": ["other", "ident", "comment", "string", "keyword", "number", "type", "def"], "lines": [[[0, "  "], [2, "// call render here"]], [[0, "  "], [4, "const"], [0, " "], [1, "s"], [0, " = "], [3, "\"render\""], [0, ";"]]]};
const TWICE: Highlighted = {"engine": "tree-sitter", "grammar": "typescript", "classes": ["other", "ident", "comment", "string", "keyword", "number", "type", "def"], "lines": [[[1, "render"], [0, "(); "], [1, "render"], [0, "();"]]]};
const TYPE: Highlighted = {"engine": "tree-sitter", "grammar": "typescript", "classes": ["other", "ident", "comment", "string", "keyword", "number", "type", "def"], "lines": [[[4, "let"], [0, " "], [1, "store"], [0, ": "], [6, "Store"], [0, " = "], [1, "make"], [0, "();"]]]};
const EXACT: Highlighted = {"engine": "tree-sitter", "grammar": "typescript", "classes": ["other", "ident", "comment", "string", "keyword", "number", "type", "def"], "lines": [[[0, "  "], [4, "const"], [0, " "], [1, "s"], [0, " = "], [3, "`a "], [1, "$"], [0, "{"], [1, "b"], [0, "."], [1, "c"], [0, "()}"], [3, " d`"], [0, "; "], [2, "// 1 + 2"]]]};

function tokensOf(result: Highlighted, line: number): Token[] {
  return decodeLine(result.lines[line] ?? [], result.classes);
}

function lineRef(over: Partial<LineRef>): LineRef {
  return {
    ident: 'x',
    col: null,
    targetId: 'method:x',
    uncertain: false,
    outside: false,
    title: '',
    ...over,
  };
}

/** Which token an overlay ref claims — the whole point of the atomisation. */
function claimedText(result: Highlighted, line: number, ref: LineRef): string | undefined {
  const tokens = tokensOf(result, line);
  const claimed = assignRefs(tokens, [ref]);
  const [index] = [...claimed.keys()];
  return index === undefined ? undefined : tokens[index]?.text;
}

describe('graph links land on the right token', () => {
  it('keeps a template literal’s interpolated call as code, so it can link', () => {
    expect(claimedText(TEMPLATE, 0, lineRef({ ident: 'size' }))).toBe('size');
  });

  it('splits a component’s markup into identifiers, so a call site in it links', () => {
    expect(claimedText(SVELTE, 4, lineRef({ ident: 'bump' }))).toBe('bump');
  });

  it('still splits identifiers when it cannot highlight, so the links land', () => {
    expect(PLAIN.engine).toBe('plain');
    expect(claimedText(PLAIN, 0, lineRef({ ident: 'withLock', col: 9 }))).toBe('withLock');
  });

  it('marks the callee, not the receiver the recorded column points at', () => {
    const line = '    return this.indexMutex.withLock(async () => {';
    expect(claimedText(RECEIVER, 0, lineRef({ ident: 'withLock', col: line.indexOf('this') }))).toBe(
      'withLock'
    );
  });

  it('lands on a Go method call', () => {
    const line = '\tresult := s.repo.FindByID(ctx, id)';
    expect(claimedText(GO, 0, lineRef({ ident: 'FindByID', col: line.indexOf('s.repo') }))).toBe(
      'FindByID'
    );
  });

  it('lands on a Python method call, not on the receiver of the same name', () => {
    const line = '    return self.store.join(self.store.path)';
    expect(claimedText(PYTHON, 0, lineRef({ ident: 'join', col: line.indexOf('self') }))).toBe('join');
  });

  it('leaves a word inside a comment or a string alone', () => {
    expect(claimedText(COMMENT, 0, lineRef({ ident: 'render' }))).toBeUndefined();
    expect(claimedText(COMMENT, 1, lineRef({ ident: 'render' }))).toBeUndefined();
  });

  it('keeps every identifier separately claimable', () => {
    const tokens = tokensOf(TWICE, 0);
    const claimed = assignRefs(tokens, [
      lineRef({ ident: 'render', targetId: 'a' }),
      lineRef({ ident: 'render', targetId: 'b' }),
    ]);
    expect(claimed.size).toBe(2);
  });

  it('keeps a type name claimable — it is a distinct class, not an excluded one', () => {
    expect(tokensOf(TYPE, 0).some((t) => t.cls === 'type' && t.text === 'Store')).toBe(true);
    expect(claimedText(TYPE, 0, lineRef({ ident: 'Store' }))).toBe('Store');
  });

  it('reproduces the line exactly — the code block renders these tokens', () => {
    const line = '  const s = `a ${b.c()} d`; // 1 + 2';
    expect(
      tokensOf(EXACT, 0)
        .map((t) => t.text)
        .join('')
    ).toBe(line);
  });
});
