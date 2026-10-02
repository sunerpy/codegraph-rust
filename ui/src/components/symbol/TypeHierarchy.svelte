<!--
  The type hierarchy: what this type is built on, and what is built on it
  (design spec §3.10).

  It sits above the members outline because it changes how the outline reads. A
  method on a class that implements a twelve-member interface is not the same
  object as a method on a class nothing extends: one is a contract you can break
  for eleven other files, the other is a private detail. The tree says which
  before the member list is on screen, and the outline's "overrides X" marks
  come from the same walk.

  Layout is arithmetic — fixed row height, fixed indent step — so the connectors
  are drawn from two numbers rather than measured. `implements` is dashed and
  `extends` solid; a synthesized edge (Go's implicit interface satisfaction) is
  dashed wider and says where it was wired, exactly as the Flow strip draws a
  synthesized hop.
-->
<script lang="ts">
  import Icon from '../Icon.svelte';
  import KindGlyph from '../KindGlyph.svelte';
  import type { WireHierarchy, WireNodeDetail, WireNodeRef } from '../../lib/api';
  import {
    buildHierarchyModel,
    connectorPath,
    visibleHierarchy,
    HIER_ROW_H,
    HIER_TILE,
  } from '../../lib/hierarchy-model';

  interface Props {
    hierarchy: WireHierarchy;
    focus: WireNodeDetail;
    onopen: (node: WireNodeRef) => void;
  }

  let { hierarchy, focus, onopen }: Props = $props();

  let expanded = $state(false);
  let model = $derived(buildHierarchyModel(hierarchy, focus));
  let view = $derived(visibleHierarchy(model, expanded));

  // Reset the fold when the reader navigates to another type — an expanded fan
  // left open across a navigation would silently apply to a different symbol.
  $effect(() => {
    focus.id;
    expanded = false;
  });

  let counts = $derived(
    [
      hierarchy.ancestors.total > 0
        ? `${hierarchy.ancestors.total} above`
        : '',
      hierarchy.direct > 0 ? `${hierarchy.descendants.total} below` : '',
    ]
      .filter(Boolean)
      .join(' · ')
  );

  function title(row: (typeof view.rows)[number]): string {
    const where = `${row.node.file}:${row.node.line}`;
    if (!row.entry) return `${row.node.qualifiedName} — ${where}`;
    const wiring = row.entry.synthesized
      ? ` — matched by ${row.entry.via ?? 'the resolver'}${row.entry.registeredAt ? ` at ${row.entry.registeredAt}` : ''}`
      : '';
    return `${row.node.qualifiedName} — ${where}${wiring}`;
  }
</script>

{#if model.headline}
  <!-- §8 D-06: the one claim the rows cannot make for themselves. -->
  <p class="callout violet headline"><span class="zap"><Icon name="zap" /></span>{model.headline}</p>
{/if}

<div class="hcard">
  <div class="subh">
    <span class="lead"><Icon name="list-tree" /></span>
    <span>Type hierarchy</span>
    {#if counts}<span class="n">{counts}</span>{/if}
    <span class="hint">supertypes above · subtypes below</span>
  </div>

  <div class="tree">
   <div class="canvas" style:height={`${view.height}px`}>
    <svg class="wires" width="100%" height={view.height} aria-hidden="true">
      {#each view.connectors as c, i (i)}
        <path
          d={connectorPath(c)}
          class:dashed={c.relation === 'implements'}
          class:synth={c.synthesized}
        />
      {/each}
    </svg>

    {#each view.rows as row (row.node.id + row.side)}
      {#if row.side === 'focus'}
        <div
          class="row focus"
          style:top={`${row.index * HIER_ROW_H}px`}
          style:padding-left={`${row.indent + 18}px`}
        >
          <span class="focustile"><KindGlyph kind={row.node.kind} size={HIER_TILE} /></span>
          <span class="nm">{row.node.name}</span>
        </div>
      {:else}
        <button
          type="button"
          class="row"
          style:top={`${row.index * HIER_ROW_H}px`}
          style:padding-left={`${row.indent + 18}px`}
          onclick={() => onopen(row.node)}
          title={title(row)}
        >
          <KindGlyph kind={row.node.kind} size={HIER_TILE} />
          <span class="nm">{row.node.name}</span>
          <span class="word">{row.word} · {row.node.file === focus.file ? 'same file' : row.node.file.slice(row.node.file.lastIndexOf('/') + 1)}</span>
          {#if row.entry?.synthesized}
            <span class="pill mono" title={row.entry.registeredAt ?? ''}>
              via {row.entry.via ?? 'resolver'}
            </span>
          {/if}
          {#if row.entry && row.entry.hiddenSubtypes > 0}
            <span class="pill mono">+{row.entry.hiddenSubtypes} below</span>
          {/if}
        </button>
      {/if}
    {/each}
   </div>
  </div>

  {#if model.foldFrom !== null}
    <button type="button" class="btn secondary fold" onclick={() => (expanded = !expanded)}>
      <Icon name={expanded ? 'minus' : 'plus'} />{expanded ? 'Fold' : `+${model.foldCount} more ${model.foldNoun}`}
    </button>
  {/if}

  {#if model.note}
    <div class="note">{model.note}</div>
  {/if}
</div>

<style>
  .headline {
    margin: 20px 0 0;
    font: var(--t-body);
  }

  .zap {
    display: inline-flex;
    color: var(--violet);
  }

  /* §8 D-06 hierarchy card: rows 26 high, 28 per level, tiles 20; guides
     `line-strong` 1.2 — solid for extends, dashed 4 3 for implements. */
  .hcard {
    margin-top: 16px;
    padding: 0 16px 14px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--card);
  }

  .subh {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 48px;
    font: var(--t-label);
  }

  .subh .lead {
    display: inline-flex;
    color: var(--fg-3);
  }

  .subh .n {
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .subh .hint {
    margin-left: auto;
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .tree {
    padding-top: 2px;
  }

  /* The one positioned box: rows and wires share its origin, so a row's y and
     the y its connector lands on are the same arithmetic. */
  .canvas {
    position: relative;
  }

  .wires {
    position: absolute;
    top: 0;
    left: 0;
    overflow: visible;
    pointer-events: none;
  }

  .wires path {
    fill: none;
    stroke: var(--line-strong);
    stroke-width: 1.2;
  }

  .wires path.dashed {
    stroke-dasharray: 4 3;
  }

  .wires path.synth {
    stroke: var(--fg-3);
    stroke-dasharray: 6 3;
  }

  .row {
    position: absolute;
    top: 0;
    right: 0;
    left: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    height: 26px;
    padding-right: 6px;
    border-radius: 6px;
    text-align: left;
  }

  button.row:hover {
    background: var(--raised);
  }

  .nm {
    color: var(--fg);
    font: var(--t-mono);
    white-space: nowrap;
  }

  .row.focus .nm {
    color: var(--primary-ink);
    font: var(--t-mono-500);
  }

  .focustile {
    display: inline-flex;
    border-radius: 6px;
    box-shadow: var(--glow-50);
  }

  .word {
    overflow: hidden;
    color: var(--fg-3);
    font: var(--t-caption);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .row .pill {
    height: 18px;
    padding: 0 7px;
  }

  .fold {
    height: 28px;
    margin: 10px 0 0 34px;
  }

  .note {
    padding: 10px 0 0;
    color: var(--fg-3);
    font: var(--t-caption);
  }
</style>
