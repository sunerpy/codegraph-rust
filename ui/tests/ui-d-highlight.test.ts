// Direction D colours two token kinds upstream left in plain ink
// (docs/design/viewer-d.md §3: `syn-type`, `syn-punct`), without letting an
// unhighlighted line turn the punctuation colour.
import { describe, it, expect } from 'vitest';
import { tokenClass, tokenClassFor } from '../src/lib/highlight';

describe('tokenClassFor', () => {
  it('keeps every class tokenClass already gives', () => {
    for (const cls of ['comment', 'string', 'keyword', 'number', 'def'] as const) {
      expect(tokenClassFor({ cls, text: 'x' })).toBe(tokenClass(cls));
    }
  });

  it('colours a type reference', () => {
    expect(tokenClassFor({ cls: 'type', text: 'Path' })).toBe('t-t');
  });

  it('colours an other token only when it is all punctuation', () => {
    expect(tokenClassFor({ cls: 'other', text: '(' })).toBe('t-p');
    expect(tokenClassFor({ cls: 'other', text: ') -> ' })).toBe('t-p');
    expect(tokenClassFor({ cls: 'other', text: '::' })).toBe('t-p');
  });

  it('leaves whitespace and whole unclassified lines in plain ink', () => {
    expect(tokenClassFor({ cls: 'other', text: '    ' })).toBeNull();
    expect(tokenClassFor({ cls: 'other', text: 'let x = parse(input);' })).toBeNull();
    expect(tokenClassFor({ cls: 'ident', text: 'resolve' })).toBeNull();
  });
});
