<!--
  A container's members in source order, with the two numbers that say which
  one to open (design spec §3.2).

  This replaces the body for anything over 80 lines, and the `← in  → out`
  columns are why it is a better view than the body rather than a poorer one:
  a class's own fan-out is nearly always zero because a class calls nothing —
  its methods do — so scrolling 700 lines of braces tells you less about where
  the weight sits than twenty rows with their edge counts.
-->
<script lang="ts">
  import Icon from '../Icon.svelte';
  import KindGlyph from '../KindGlyph.svelte';
  import type { WireNodeRef, WireOverride } from '../../lib/api';
  import type { OutlineRow } from '../../lib/symbol-model';

  interface Props {
    rows: OutlineRow[];
    total: number;
    truncated: boolean;
    onopen: (node: WireNodeRef) => void;
  }

  let { rows, total, truncated, onopen }: Props = $props();

  /**
   * The override mark is a NAME match inside a chain the graph links, not an
   * `overrides` edge — nothing in the engine emits one. The tooltip says so,
   * because "overrides Base" and "declares the same name as Base" are different
   * claims and only the second one was checked.
   */
  function overrideTitle(o: WireOverride): string {
    return o.relation === 'implements'
      ? `Declares a member ${o.baseTypeName} requires — matched by name.`
      : `Redeclares a member of ${o.baseTypeName} — matched by name.`;
  }
</script>

<div class="mcard">
<div class="subh">
  <span class="lead"><Icon name="braces" /></span>
  <span>Members</span>
  <span class="n">{total} · ← in → out</span>
</div>

<div class="outline">
  {#each rows as row (row.member.id)}
    <button
      type="button"
      class="orow"
      class:nested={row.nested}
      class:dimmed={row.dimmed}
      onclick={() => onopen(row.member)}
      title={`${row.member.qualifiedName} — ${row.member.file}:${row.member.line}`}
    >
      <KindGlyph kind={row.member.kind} size={20} />
      <span class="nm">{row.member.name}</span>
      <span class="sig">
        {#if row.member.overrides}
          <span class="ovr" title={overrideTitle(row.member.overrides)}>
            {row.member.overrides.relation === 'implements' ? 'satisfies' : 'overrides'}
            {row.member.overrides.baseTypeName}
          </span>
        {/if}{row.member.signature ?? ''}</span>
      <span class="cnt"><span class:zero={!row.member.fanIn}>← {row.member.fanIn}</span><span class:zero={!row.member.fanOut}>→ {row.member.fanOut}</span></span>
    </button>
  {/each}
</div>

{#if truncated}
  <div class="note">
    Showing {rows.length} of {total} members — open the file to see the rest.
  </div>
{/if}
</div>

<style>
  /* §8 D-06 members card: tile 20, the name, the signature in `mono-sm`
     `fg-4`, and the ← in / → out counts on the right. */
  .mcard {
    margin-top: 16px;
    padding: 0 12px 10px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--card);
  }

  .subh {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 48px;
    padding: 0 4px;
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

  .orow {
    display: grid;
    grid-template-columns: 20px minmax(120px, auto) minmax(0, 1fr) auto;
    gap: 10px;
    align-items: center;
    width: 100%;
    min-height: 34px;
    padding: 0 6px;
    border-radius: 8px;
    text-align: left;
  }

  .orow:hover {
    background: var(--raised);
  }

  .orow.nested {
    padding-left: 28px;
  }

  .nm {
    color: var(--fg);
    font: var(--t-mono);
  }

  .orow.dimmed .nm {
    color: var(--fg-3);
  }

  .ovr {
    margin-right: 8px;
    padding: 1px 7px;
    border-radius: 9px;
    background: var(--violet-soft);
    color: var(--violet);
    font: var(--t-mono-sm);
    white-space: nowrap;
  }

  .sig {
    overflow: hidden;
    color: var(--fg-4);
    font: var(--t-mono-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .cnt {
    display: inline-flex;
    gap: 12px;
    color: var(--fg-2);
    font: var(--t-mono-sm);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }

  .cnt .zero {
    color: var(--fg-4);
  }

  .note {
    padding: 8px 6px 2px;
    color: var(--fg-3);
    font: var(--t-caption);
  }
</style>
