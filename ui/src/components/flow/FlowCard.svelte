<!--
  One hop of a flow: the symbol, where it lives, and the seven lines around the
  call that carries the reader to the next card (design spec §3.5).

  The card is a Svelte Flow node, but nothing about it is Svelte Flow's: the
  handles are hidden ports at the vertical middle of each side, the position
  came from `buildFlowLayout`, and the height is the one that layout computed —
  pinned here so the arrows land where the arithmetic said they would.

  The source window is the Symbol view's code block with the noise removed. It
  keeps the two things that make the code readable: the server's classified
  classification, and one accent link on the identifier the graph resolved. It
  drops gutter ports and multi-window folding, because a seven-line card has
  neither a gutter worth reading nor anything to fold.
-->
<script lang="ts">
  import { Handle, Position } from '@xyflow/svelte';
  import KindGlyph from '../KindGlyph.svelte';
  import { tokenClassFor, tokensByLine, type Token } from '../../lib/highlight';
  import { assignRefs, basename, type LineRef } from '../../lib/symbol-model';
  import type { FlowCardLayout } from '../../lib/flow-model';

  interface Props {
    data: {
      card: FlowCardLayout;
      current: boolean;
      dimmed: boolean;
      onOpen: (card: FlowCardLayout) => void;
      onFollow: (card: FlowCardLayout) => void;
    };
  }

  let { data }: Props = $props();
  let card = $derived(data.card);
  let hop = $derived(card.hop);
  let source = $derived(hop.source);

  /** The call site as the code block's overlay wants it: one ref on one line. */
  let refs = $derived.by<Map<number, LineRef[]>>(() => {
    const byLine = new Map<number, LineRef[]>();
    const ref = hop.callRef;
    if (!ref) return byLine;
    byLine.set(ref.line, [
      {
        ident: ref.name,
        col: ref.col,
        targetId: ref.targetId,
        uncertain: false,
        outside: false,
        title: ref.backwards
          ? `${hop.node.name} calls ${ref.name} here`
          : `calls ${ref.name}`,
      },
    ]);
    return byLine;
  });

  let tokens = $derived.by<Map<number, Token[]>>(() =>
    source?.lines ? tokensByLine(source.lines, source.from, source.highlight) : new Map()
  );

  interface Part {
    text: string;
    cls: string | null;
    ref: LineRef | null;
  }

  let rows = $derived.by(() => {
    if (!source?.lines) return [];
    return source.lines.map((text, offset) => {
      const n = source.from + offset;
      const lineTokens = tokens.get(n) ?? [{ cls: 'other' as const, text, col: 0 }];
      const claimed = assignRefs(lineTokens, refs.get(n) ?? []);
      return {
        n,
        call: n === hop.callRef?.line || n === card.stopLine,
        parts: lineTokens.map((token, index): Part => {
          const ref = claimed.get(index) ?? null;
          return { text: token.text, cls: ref ? null : tokenClassFor(token), ref };
        }),
      };
    });
  });
</script>

<div
  class="card"
  class:cur={data.current}
  class:dim={data.dimmed}
  style={`width:${card.width}px;height:${card.height}px`}
>
  <Handle type="target" position={Position.Left} id="in" isConnectable={false} />
  <Handle type="source" position={Position.Right} id="out" isConnectable={false} />

  <button type="button" class="head" onclick={() => data.onOpen(card)}>
    {#if card.step >= 0}<span class="badge">{card.step + 1}</span>{/if}
    <KindGlyph kind={hop.node.kind} size={20} />
    <span class="nm">{hop.node.name}</span>
    <span class="loc">{basename(hop.node.file)}:{hop.node.line}</span>
  </button>

  {#if rows.length > 0}
    <div class="code">
      {#each rows as row (row.n)}
        <div class="ln" class:call={row.call}>
          <span class="no">{row.n}</span>
          <span class="tx"
            >{#each row.parts as part, i (i)}{#if part.ref}<button
                  type="button"
                  class="ref"
                  title={part.ref.title}
                  onclick={() => data.onFollow(card)}>{part.text}</button
                >{:else if part.cls}<span class={part.cls}>{part.text}</span
                >{:else}{part.text}{/if}{/each}</span
          >
        </div>
      {/each}
    </div>
  {:else}
    <p class="nosource">
      {source?.drift
        ? 'Changed on disk after the last index sync — source is not shown.'
        : (source?.reason ?? 'Source outside this slice or this index.')}
    </p>
  {/if}
</div>

<style>
  /* §7 flow step card: `card` + `line` + SH.card, r 12; the step badge,
     the kind tile, the name and its location; then the window opened at the
     call, the call line lit as the Symbol view lights a hot line. */
  .card {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--card);
    box-shadow: var(--sh-card);
    text-align: left;
    transition:
      border-color 120ms,
      box-shadow 120ms;
  }

  .card:hover {
    border-color: var(--line-strong);
  }

  /* Selected: 1.5px GRAD.brand stroke and the 40 % glow (§7, D-04). */
  .card.cur {
    border: 1.5px solid transparent;
    background:
      linear-gradient(var(--card), var(--card)) padding-box,
      var(--grad-brand-d) border-box;
    box-shadow: var(--glow-40);
  }

  .card.dim {
    opacity: 0.45;
  }

  .head {
    display: grid;
    align-items: center;
    padding: 10px 12px 9px;
    border-bottom: 1px solid var(--line-faint);
    background: none;
    color: var(--fg);
    gap: 8px;
    grid-template-columns: auto 20px minmax(0, 1fr) auto;
    text-align: left;
  }

  .head:hover .nm {
    color: var(--primary-ink);
  }

  .badge {
    display: inline-flex;
    width: 22px;
    height: 22px;
    align-items: center;
    justify-content: center;
    border-radius: 11px;
    background: var(--raised);
    color: var(--fg-2);
    font: var(--t-mono-sm);
    font-weight: 600;
  }

  .card.cur .badge {
    background: var(--grad-button);
    box-shadow: var(--glow-50);
    color: var(--on-primary);
  }

  .nm {
    overflow: hidden;
    font: var(--t-mono-500);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .loc {
    color: var(--fg-3);
    font: var(--t-mono-sm);
    white-space: nowrap;
  }

  .code {
    padding: 6px 0;
    font: 400 12px / 19px var(--mono);
  }

  .ln {
    position: relative;
    display: grid;
    align-items: stretch;
    grid-template-columns: 40px 1fr 6px;
  }

  .ln.call {
    background: var(--primary-soft);
  }

  .ln.call::before {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 0;
    width: 2px;
    background: var(--grad-brand-v);
    content: '';
  }

  .no {
    padding-right: 10px;
    color: var(--fg-4);
    font: 400 11px / 19px var(--mono);
    text-align: right;
    user-select: none;
  }

  .tx {
    overflow: hidden;
    color: var(--fg);
    text-overflow: ellipsis;
    white-space: pre;
  }

  .nosource {
    margin: 0;
    padding: 8px 12px;
    color: var(--fg-3);
    font: var(--t-small);
    line-height: 19px;
  }

  /* §3 syntax — the same classes the Symbol view paints. */
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

  /* The call this card is opened at: a cyan capsule (§7). */
  .ref {
    margin: 0 -3px;
    padding: 1px 3px;
    border: 0;
    border-radius: 5px;
    background: var(--cyan-soft);
    box-shadow: inset 0 0 0 1px var(--cyan-line);
    color: var(--cyan);
    cursor: pointer;
    font: inherit;
  }

  .ref:hover {
    background: color-mix(in srgb, var(--cyan) 22%, var(--cyan-soft));
  }
</style>
