<!--
  The whole file's source, virtualised, with a gutter port on every line the
  graph has an edge from (design spec §3.4, task CG-52).

  The same line grid as the Symbol view — `44px | 1fr | 18px`, the same 6x6
  port, the same accent call-site links — with one difference that changes
  everything about how it is built: lines are ABSOLUTELY POSITIONED at
  `lineTop(n)` rather than stacked. That is what lets a 6 820-line file hold
  ninety-odd elements instead of six thousand, and what lets an arc drawn beside
  it know where line 4 271 is without asking the browser.

  A page of source that has not arrived yet still draws: its line number, its
  port and its place in the document are all facts from the graph, not from the
  text. Only the characters are missing, and they fill in behind the reader.
-->
<script lang="ts">
  import { tokenClassFor, type Token } from '../../lib/highlight';
  import { assignRefs, type LineRef } from '../../lib/symbol-model';
  import { lineTop } from '../../lib/filecode-model';
  import { hot } from '../../lib/focus.svelte';

  interface Props {
    /** Inclusive 1-based range to render. */
    first: number;
    last: number;
    /** Classified source for a line, or null while its page is in flight. */
    tokensFor: (line: number) => Token[] | null;
    refs: Map<number, LineRef[]>;
    /** Lines a definition starts on → its name, set in bold there. */
    defNames: Map<number, string>;
    /** The line `?hl=` or an arc click landed on. */
    highlight: number | null;
    onfollow: (ref: LineRef) => void;
    onhoverline: (line: number | null) => void;
  }

  let { first, last, tokensFor, refs, defNames, highlight, onfollow, onhoverline }: Props =
    $props();

  interface Part {
    text: string;
    cls: string | null;
    ref: LineRef | null;
    def: boolean;
  }

  interface RenderedLine {
    n: number;
    top: number;
    parts: Part[] | null;
    port: 'sure' | 'unsure' | null;
    targets: string[];
  }

  let lines = $derived.by<RenderedLine[]>(() => {
    const out: RenderedLine[] = [];
    for (let n = first; n <= last; n++) {
      const lineRefs = refs.get(n) ?? [];
      const tokens = tokensFor(n);
      out.push({
        n,
        top: lineTop(n),
        parts: tokens ? toParts(tokens, assignRefs(tokens, lineRefs), defNames.get(n) ?? null) : null,
        port: portFor(lineRefs),
        targets: [...new Set(lineRefs.map((r) => r.targetId).filter((id): id is string => !!id))],
      });
    }
    return out;
  });

  function toParts(line: Token[], claimed: Map<number, LineRef>, definition: string | null): Part[] {
    return line.map((token, index) => {
      const ref = claimed.get(index) ?? null;
      return {
        text: token.text,
        cls: ref ? null : tokenClassFor(token),
        ref,
        def:
          !ref &&
          definition !== null &&
          token.text === definition &&
          token.cls !== 'comment' &&
          token.cls !== 'string',
      };
    });
  }

  /**
   * Filled = the graph resolved something here; hollow = it only guessed, or
   * the reference left the index. No edge at all means no port: absence is the
   * signal, so an empty gutter must stay empty.
   */
  function portFor(lineRefs: readonly LineRef[]): 'sure' | 'unsure' | null {
    if (lineRefs.length === 0) return null;
    return lineRefs.some((r) => !r.uncertain && !r.outside) ? 'sure' : 'unsure';
  }

  function isHot(line: RenderedLine): boolean {
    return line.n === highlight || line.targets.some((id) => hot.is(id));
  }
</script>

<div class="code">
  {#each lines as line (line.n)}
    <div
      class="ln"
      class:hot={isHot(line)}
      style:top={`${line.top}px`}
      data-line={line.n}
      onmouseenter={() => onhoverline(line.n)}
      role="presentation"
    >
      <span class="no">{line.n}</span>
      <span class="tx"
        >{#if line.parts}{#each line.parts as part, i (i)}{#if part.ref && !part.ref.outside}{@const ref =
              part.ref}<span
              class="ref"
              class:uncertain={ref.uncertain}
              class:hot={hot.is(ref.targetId)}
              role="link"
              tabindex="0"
              title={ref.title}
              onclick={() => onfollow(ref)}
              onkeydown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') {
                  e.preventDefault();
                  onfollow(ref);
                }
              }}
              onmouseenter={() => hot.set(ref.targetId)}
              onmouseleave={() => hot.clear(ref.targetId)}>{part.text}</span
            >{:else if part.ref}<span class="ref stub" title={part.ref.title}>{part.text}</span
            >{:else if part.def}<span class="t-def">{part.text}</span
            >{:else if part.cls}<span class={part.cls}>{part.text}</span
            >{:else}{part.text}{/if}{/each}{:else}<span class="pending"></span>{/if}</span
      >
      <span class="port">
        {#if line.port}<i class:sure={line.port === 'sure'}></i>{/if}
      </span>
    </div>
  {/each}
</div>

<style>
  .code {
    position: absolute;
    inset: 0;
    font: var(--t-code);
  }

  /* 44px gutter | source | 18px port cell — the Symbol view's grid.

     The HEIGHT here is load-bearing: every arc, connector and rail row on this
     screen is placed at `CODE_TOP_PAD + (line - 1) * CODE_LINE_HEIGHT`, and a
     line that grew past it would detach all three from the source at once.
     `filecode-model.ts` holds the constant; this is the other half of it. */
  .ln {
    position: absolute;
    right: 0;
    left: 0;
    display: grid;
    height: 20px;
    box-sizing: border-box;
    grid-template-columns: 44px 1fr 18px;
    align-items: stretch;
  }

  .ln:hover {
    background: var(--raised);
  }

  /* Hot: `primary-soft` with the 2px GRAD.brand bar (§7 hot line). */
  .ln.hot {
    background: var(--primary-soft);
  }

  .ln.hot::before {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 0;
    width: 2px;
    background: var(--grad-brand-v);
    content: '';
  }

  .no {
    padding-right: 12px;
    color: var(--fg-4);
    font: var(--t-lineno);
    text-align: right;
    user-select: none;
  }

  .tx {
    overflow: hidden;
    color: var(--fg);
    white-space: pre;
  }

  /* A line whose page is still in flight. It keeps its number, its port and its
     place; only the characters are missing. */
  .pending {
    display: inline-block;
    width: 34%;
    height: 8px;
    border-radius: 4px;
    background: var(--raised);
    vertical-align: middle;
  }

  .port {
    position: relative;
  }

  /* §7 port: a 7px `cyan` dot; hollow for a line of guesses; glowing hot. */
  .port i {
    position: absolute;
    top: 6.5px;
    right: 5px;
    width: 7px;
    height: 7px;
    border: 1px solid var(--cyan-line);
    border-radius: 50%;
    background: transparent;
  }

  .port i.sure {
    border-color: var(--cyan);
    background: var(--cyan);
  }

  .ln.hot .port i {
    border-color: var(--cyan);
    background: var(--cyan);
    box-shadow: var(--glow-cyan-90);
  }

  /* ---- §3 syntax ---- */
  .t-c {
    color: var(--syn-com);
  }

  .t-s {
    color: var(--syn-str);
  }

  .t-k {
    color: var(--syn-kw);
    font-weight: 500;
  }

  .t-n {
    color: var(--syn-num);
  }

  .t-t {
    color: var(--syn-type);
  }

  .t-p {
    color: var(--syn-punct);
  }

  .t-def {
    color: var(--fg);
    font-weight: 600;
  }

  /* §7 call capsule behind each resolved call name. */
  .ref {
    margin: 0 -3px;
    padding: 1px 3px;
    border-radius: 5px;
    background: color-mix(in srgb, var(--cyan-soft) 85%, transparent);
    color: var(--cyan);
    cursor: pointer;
  }

  .ref:hover,
  .ref.hot,
  .ln.hot .ref {
    background: var(--cyan-soft);
    box-shadow: inset 0 0 0 1px var(--cyan-line);
  }

  .ref.uncertain {
    background: transparent;
    color: var(--fg-2);
    text-decoration: underline dotted var(--fg-4);
    text-underline-offset: 3px;
  }

  .ref.stub {
    background: transparent;
    color: var(--fg-2);
    cursor: default;
    text-decoration: underline dotted var(--line-strong);
    text-underline-offset: 3px;
  }

  .ref.stub:hover {
    box-shadow: none;
  }
</style>
