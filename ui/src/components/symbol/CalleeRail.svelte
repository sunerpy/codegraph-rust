<!--
  Calls — the right rail (design spec §3.2).

  Every row is absolutely positioned beside the line that makes the call, which
  is the whole idea of the screen: the callee list is not a list, it is an
  annotation of the body. Rows keep source order and are pushed down when two
  call sites are closer together than a row is tall, so the sequence still reads
  top to bottom even where the geometry cannot be exact.

  The tops are computed by the view, which is the only thing that can measure
  where a line ended up. This component draws what it is told.
-->
<script lang="ts">
  import Icon from '../Icon.svelte';
  import KindGlyph from '../KindGlyph.svelte';
  import { hot, railFocus } from '../../lib/focus.svelte';
  import { plural, type CalleeRailModel, type CalleeRow } from '../../lib/symbol-model';
  import type { WireNodeRef, WireOutsideIndex } from '../../lib/api';

  interface Props {
    model: CalleeRailModel;
    /** Top offset in px for each row in `model.rows`, same order. */
    tops: number[];
    foldTop: number;
    noteTop: number;
    /** False until the view has measured the rail — see SymbolView. */
    placed: boolean;
    /** The focal symbol's file — a callee in it reads "same file", not a path. */
    focalFile: string;
    /** The symbol this one was reached from, when it is a callee. */
    originId: string | null;
    /** Empty-rail wording depends on why it is empty. */
    emptyReason: string;
    /** Calls into symbols the index does not hold — the "Leaves the index" card. */
    outside?: WireOutsideIndex | null;
    onstepDown: (node: WireNodeRef) => void;
  }

  let {
    model,
    tops,
    foldTop,
    noteTop,
    placed,
    focalFile,
    originId,
    emptyReason,
    outside = null,
    onstepDown,
  }: Props = $props();

  /** The call names that leave the index, each once, in source order. */
  let leaving = $derived.by(() => {
    if (!outside) return [];
    const seen = new Set<string>();
    const names: string[] = [];
    for (const sample of outside.samples) {
      if (sample.kind !== 'calls' || seen.has(sample.name)) continue;
      seen.add(sample.name);
      names.push(sample.name);
    }
    return names.slice(0, 8);
  });

  function baseName(file: string): string {
    return file.slice(file.lastIndexOf('/') + 1);
  }

  function rowTitle(row: CalleeRow): string {
    return `${row.relation.node.qualifiedName} — ${row.relation.node.file}:${row.relation.node.line}`;
  }
</script>

<!-- Everything above the anchored rows is one measured block: the rows start
     under it, wherever the call lines are. -->
<div class="rail-top" data-rail-header>
  <div class="rail-h">
    <span class="title"><span class="lead"><Icon name="arrow-right" /></span>Calls <span class="count">{model.rows.length}</span></span>
    {#if model.outsideCalls > 0}<span class="hint">+{model.outsideCalls} leave the index</span>{/if}
  </div>
  {#if model.rows.length === 0 && model.uncertain.length === 0}
    <div class="emptycard">
      <span class="ic"><Icon name="info" /></span>
      <span>{emptyReason}</span>
    </div>
  {/if}
  {#if leaving.length > 0}
    <div class="leaves">
      <div class="lh"><span class="micro">Leaves the index</span><span class="dim">not followed</span></div>
      <div class="pills">
        {#each leaving as name (name)}<span class="pill bordered mono">{name}</span>{/each}
      </div>
    </div>
  {/if}
</div>

{#each model.rows as row, i (row.relation.node.id)}
  {@const node = row.relation.node}
  <div
    class="rrow"
    class:origin={node.id === originId}
    class:hot={hot.is(node.id)}
    class:sel={railFocus.at('right', i)}
    class:unplaced={!placed}
    style:top={`${tops[i] ?? 0}px`}
    data-target={node.id}
    role="button"
    tabindex="0"
    title={rowTitle(row)}
    onclick={() => onstepDown(node)}
    onkeydown={(e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        onstepDown(node);
      }
    }}
    onmouseenter={() => hot.set(node.id)}
    onmouseleave={() => hot.clear(node.id)}
  >
    <KindGlyph kind={node.kind} size={22} />
    <div class="body">
      <div class="nm">
        {node.name}{#if row.lines.length > 1}<span class="dim"> ×{row.lines.length}</span>{/if}
      </div>
      <div class="meta">
        <span>{`${node.file === focalFile ? 'same file' : baseName(node.file)}${row.anchor !== null ? ` · :${row.anchor}` : ''}`}</span>
        {#if row.words.length > 0}<span>{row.words.join(', ')}</span>{/if}
        {#if row.relation.hub}<span class="tag">hub · {row.relation.fanIn}</span>{/if}
        {#if row.via}<span class="tag" title="A synthesized edge — dynamic dispatch the parser cannot see"
            >via {row.via}</span
          >{/if}
        {#each row.when as w (w)}<span class="tag when" title="The call runs only under this condition — read from the source as it is now"
            >when {w}</span
          >{/each}
      </div>
    </div>
  </div>
{/each}

{#if model.uncertain.length > 0}
  <details class="rfold" class:unplaced={!placed} data-rail-fold style:top={`${foldTop}px`}>
    <summary>
      Uncertain <span class="dim"
        >· {model.uncertain.length} name-only match{model.uncertain.length === 1 ? '' : 'es'},
        confidence &lt; 0.6</span
      >
    </summary>
    <div class="fold-body">
      {#each model.uncertain as row (row.relation.node.id)}
        {@const node = row.relation.node}
        <div
          class="rrow static uncertain"
          class:hot={hot.is(node.id)}
          data-target={node.id}
          role="button"
          tabindex="0"
          title={rowTitle(row)}
          onclick={() => onstepDown(node)}
          onkeydown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              onstepDown(node);
            }
          }}
          onmouseenter={() => hot.set(node.id)}
          onmouseleave={() => hot.clear(node.id)}
        >
          <KindGlyph kind={node.kind} />
          <div class="body">
            <div class="nm">{node.name}</div>
            <div class="meta">
              <span>{node.file}</span>
              {#if row.relation.confidence !== null}<span>{row.relation.confidence}</span>{/if}
            </div>
          </div>
        </div>
      {/each}
    </div>
  </details>
{/if}

{#if model.rows.length > 0 || model.uncertain.length > 0}{#if model.outsideCalls > 0 || model.outsideTypeRefs > 0 || model.hiddenGroups > 0}
  <div class="rnote" class:unplaced={!placed} style:top={`${noteTop}px`}>
    {#if model.outsideCalls > 0}
      +{plural(model.outsideCalls, 'more call')} into symbols outside the index{#if model.outsideTypeRefs > 0}{' '}·
        {plural(model.outsideTypeRefs, 'type reference')}{/if}.
    {:else if model.outsideTypeRefs > 0}
      {plural(model.outsideTypeRefs, 'type reference')} into symbols outside the index.
    {/if}
    {#if model.hiddenGroups > 0}
      <br />+{model.hiddenGroups} more callee{model.hiddenGroups === 1 ? '' : 's'} not shown.
    {/if}
  </div>
{/if}{/if}

<style>
  /* §8 D-02 Calls: header 52, the "Leaves the index" card, then the rows,
     each anchored beside the line that makes the call. */
  .rail-top {
    position: relative;
    z-index: 2;
  }

  .rail-h {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    height: 52px;
    padding: 0 16px;
  }

  .title {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    font: var(--t-label);
  }

  .title .lead {
    display: inline-flex;
    color: var(--cyan);
  }

  .count {
    display: inline-flex;
    min-width: 22px;
    height: 20px;
    align-items: center;
    justify-content: center;
    padding: 0 7px;
    border-radius: 10px;
    background: var(--raised);
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .hint {
    color: var(--fg-4);
    font: var(--t-caption);
  }

  .emptycard {
    display: grid;
    grid-template-columns: 16px minmax(0, 1fr);
    gap: 10px;
    margin: 0 12px 8px;
    padding: 14px;
    border: 1px solid var(--line-faint);
    border-radius: 10px;
    background: var(--card);
    color: var(--fg-2);
    font: var(--t-small);
  }

  .emptycard .ic {
    display: inline-flex;
    padding-top: 1px;
    color: var(--fg-3);
  }

  .leaves {
    margin: 0 12px 4px;
    padding: 12px;
    border: 1px solid var(--line-faint);
    border-radius: 10px;
    background: var(--card);
  }

  .lh {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: 10px;
    font: var(--t-caption);
  }

  .pills {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  /* Positioned by measurement, so it must not paint before it is measured. */
  .unplaced {
    visibility: hidden;
  }

  /* §7 callee row: 44 high, r 10, inset 12, `card` + `line-faint`; tile 22
     at 11,11, the name `mono-500` at 42,6, the meta `caption` at 42,24. */
  .rrow {
    position: absolute;
    right: 12px;
    left: 12px;
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr);
    gap: 9px;
    align-items: center;
    height: 44px;
    padding: 0 10px;
    border: 1px solid var(--line-faint);
    border-radius: 10px;
    background: var(--card);
    cursor: pointer;
    transition:
      background-color 120ms,
      border-color 120ms,
      box-shadow 120ms;
  }

  /* Inside the uncertain fold the rows are a list again — nothing to line up
     with, because an unresolved edge has no trustworthy call site. */
  .rrow.static {
    position: static;
    height: auto;
    min-height: 36px;
    margin-bottom: 6px;
    padding: 6px 10px;
  }

  .rrow:hover {
    border-color: var(--line);
    background: var(--raised);
  }

  .rrow.sel {
    border-color: var(--fg-2);
  }

  .rrow:focus-visible {
    outline-offset: 4px;
  }

  .rrow.hot {
    border-color: var(--primary-line);
    background: var(--primary-soft);
    box-shadow: var(--glow-30);
  }

  .rrow.hot .nm {
    color: var(--primary-ink);
  }

  .rrow.origin {
    border-color: var(--primary-line);
    background: var(--primary-soft);
    box-shadow: var(--glow-22);
  }

  .body {
    min-width: 0;
  }

  .nm {
    overflow: hidden;
    color: var(--fg);
    font: var(--t-mono-500);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .rrow.uncertain .nm {
    color: var(--fg-2);
    text-decoration: underline dotted var(--fg-4);
    text-underline-offset: 3px;
  }

  .meta {
    display: flex;
    gap: 8px;
    overflow: hidden;
    margin-top: 2px;
    color: var(--fg-3);
    font: var(--t-caption);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tag {
    flex: 0 0 auto;
    padding: 0 6px;
    border-radius: 7px;
    background: var(--raised);
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .tag.when {
    background: var(--amber-soft);
    color: var(--amber);
  }

  .rfold {
    position: absolute;
    right: 12px;
    left: 12px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--raised);
  }

  .rfold summary {
    padding: 10px 12px;
    color: var(--fg-2);
    cursor: pointer;
    font: var(--t-small);
    list-style: none;
  }

  .rfold summary::-webkit-details-marker {
    display: none;
  }

  .rfold summary::before {
    color: var(--fg-3);
    content: '+ ';
    font-family: var(--mono);
  }

  .rfold[open] summary::before {
    content: '− ';
  }

  .fold-body {
    padding: 0 8px 2px;
  }

  .rnote {
    position: absolute;
    right: 12px;
    left: 16px;
    color: var(--fg-3);
    font: var(--t-caption);
    line-height: 1.45;
  }
</style>
