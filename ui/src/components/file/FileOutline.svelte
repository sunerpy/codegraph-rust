<!--
  The file's symbols in source order (design spec §3.4).

  Same row geometry as the Symbol view's members outline — `tile | name |
  signature | counts` — because they answer the same question at two scales, and a reader
  who has learned one should not have to learn the other. What differs is the
  right column: a file outline prints the LINE number as well as the edge
  counts, since source order is the only ordering here and the line is how a
  row is found in an editor.

  Long files are windowed rather than paged. `worker-configuration.d.ts` in
  this repo's own fixtures holds 1,681 symbols; rendering them all costs about
  a second of layout on every scroll, and paging would hide exactly the thing
  the outline exists to give — one uninterrupted read of the file's shape.
  Rows are a fixed height (pinned in the CSS below, and asserted by the
  `OUTLINE_ROW_HEIGHT` constant) so the window's arithmetic stays exact.
-->
<script lang="ts">
  import Icon from '../Icon.svelte';
  import KindGlyph from '../KindGlyph.svelte';
  import type { WireNodeRef } from '../../lib/api';
  import {
    OUTLINE_ROW_HEIGHT,
    OUTLINE_VIRTUAL_THRESHOLD,
    type OutlineEntryRow,
  } from '../../lib/file-model';

  interface Props {
    rows: OutlineEntryRow[];
    total: number;
    truncated: boolean;
    /** The scroll container the rows live inside — the view's centre column. */
    scroller: HTMLElement | null;
    /** Index of the keyboard's position, or -1. */
    selected?: number;
    onopen: (node: WireNodeRef) => void;
    onhover?: (index: number) => void;
  }

  let { rows, total, truncated, scroller, selected = -1, onopen, onhover }: Props = $props();

  /** The in-bar's scale: the most-referenced symbol in this file. */
  let maxIn = $derived(rows.reduce((max, row) => Math.max(max, row.entry.fanIn ?? 0), 1));

  let listEl = $state<HTMLDivElement | null>(null);
  let scrollTop = $state(0);
  let viewport = $state(0);

  let virtual = $derived(rows.length > OUTLINE_VIRTUAL_THRESHOLD);

  /**
   * The slice to draw, plus the spacer heights that keep the scrollbar honest.
   *
   * The offset is measured against the SCROLLER, not the list, because the
   * header above the outline scrolls with it: `listEl.offsetTop` is where the
   * first row starts inside that coordinate space. An overscan of eight rows
   * covers a fast flick between two measurements.
   */
  let window_ = $derived.by(() => {
    if (!virtual) return { start: 0, end: rows.length, before: 0, after: 0 };
    const top = listEl ? listEl.offsetTop : 0;
    const first = Math.floor((scrollTop - top) / OUTLINE_ROW_HEIGHT) - 8;
    const count = Math.ceil((viewport || 800) / OUTLINE_ROW_HEIGHT) + 16;
    const start = Math.max(0, Math.min(rows.length - 1, first));
    const end = Math.max(start, Math.min(rows.length, start + count));
    return {
      start,
      end,
      before: start * OUTLINE_ROW_HEIGHT,
      after: (rows.length - end) * OUTLINE_ROW_HEIGHT,
    };
  });

  // Keep the row the keyboard just moved to on screen. Rows are a fixed height
  // in both modes, so the arithmetic is the same — and it has to be arithmetic
  // rather than `scrollIntoView`, because a windowed row far outside the drawn
  // slice has no element to scroll to.
  $effect(() => {
    if (selected < 0 || !scroller || !listEl) return;
    const rowTop = listEl.offsetTop + selected * OUTLINE_ROW_HEIGHT;
    const rowBottom = rowTop + OUTLINE_ROW_HEIGHT;
    if (rowTop < scroller.scrollTop) scroller.scrollTop = rowTop - OUTLINE_ROW_HEIGHT;
    else if (rowBottom > scroller.scrollTop + scroller.clientHeight) {
      scroller.scrollTop = rowBottom - scroller.clientHeight + OUTLINE_ROW_HEIGHT;
    }
  });

  $effect(() => {
    const el = scroller;
    if (!el) return;
    const read = () => {
      scrollTop = el.scrollTop;
      viewport = el.clientHeight;
    };
    read();
    el.addEventListener('scroll', read, { passive: true });
    const observer = new ResizeObserver(read);
    observer.observe(el);
    return () => {
      el.removeEventListener('scroll', read);
      observer.disconnect();
    };
  });
</script>

<div class="ocard">
<div class="subh">
  <span class="lead"><Icon name="list-tree" /></span>
  <span>Outline</span>
  <span class="n">source order · nested by owner · {total}</span>
  <span class="cols" aria-hidden="true"><span>← in</span><span>→ out</span><span>line</span></span>
</div>

<div class="outline" bind:this={listEl}>
  {#if window_.before > 0}<div style:height={`${window_.before}px`}></div>{/if}
  {#each rows.slice(window_.start, window_.end) as row, offset (row.entry.id)}
    {@const index = window_.start + offset}
    <button
      type="button"
      class="orow"
      class:dimmed={row.dimmed}
      class:sel={index === selected}
      style:padding-left={`${8 + row.indent * 28}px`}
      onclick={() => onopen(row.entry)}
      onmouseenter={() => onhover?.(index)}
      title={`${row.entry.qualifiedName} — line ${row.entry.line}`}
    >
      <KindGlyph kind={row.entry.kind} size={20} />
      <span class="nm">{row.entry.name}</span>
      <span class="sig">{row.entry.signature ?? ''}</span>
      <span class="in" class:zero={!row.entry.fanIn}>
        <b>{row.entry.fanIn ?? 0}</b>
        <span class="bar"><i style:width={`${Math.max(row.entry.fanIn ? 8 : 0, Math.round((100 * (row.entry.fanIn ?? 0)) / maxIn))}%`}></i></span>
      </span>
      <span class="out" class:zero={!row.entry.fanOut}>{row.entry.fanOut ?? 0}</span>
      <span class="ln">{row.entry.line}</span>
    </button>
  {/each}
  {#if window_.after > 0}<div style:height={`${window_.after}px`}></div>{/if}
</div>

{#if rows.length === 0}
  <div class="note">
    Nothing was extracted from this file — it holds no symbols the graph
    recognises, only top-level code, or a language without an extractor.
  </div>
{/if}

{#if truncated}
  <div class="note">
    Showing {rows.length} of {total} symbols. The rest are in the index; this
    screen caps what it draws.
  </div>
{/if}
</div>

<style>
  /* §8 D-03 outline card: rows 28 high at a 30 pitch — name / signature
     `mono-sm` `fg-4` / ← in with a 3px GRAD.data bar / → out / line. */
  .ocard {
    margin-top: 18px;
    padding: 0 10px 8px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--card);
  }

  .subh {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 48px;
    padding: 0 8px;
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

  .cols {
    display: grid;
    grid-template-columns: 80px 48px 44px;
    margin-left: auto;
    color: var(--fg-3);
    font: var(--t-micro);
    letter-spacing: 0.6px;
    text-transform: uppercase;
  }

  .cols span:last-child {
    text-align: right;
  }

  /* The pitch here is load-bearing: the windowing arithmetic above assumes
     every row is exactly OUTLINE_ROW_HEIGHT tall. Any change must move both. */
  .orow {
    display: grid;
    height: 30px;
    box-sizing: border-box;
    grid-template-columns: 20px minmax(140px, auto) minmax(0, 1fr) 80px 48px 44px;
    width: 100%;
    align-items: center;
    gap: 10px;
    padding: 1px 8px;
    border-radius: 8px;
    background-clip: content-box;
    text-align: left;
  }

  .orow:hover {
    background-color: var(--raised);
  }

  .orow.sel {
    background-color: var(--primary-soft);
    box-shadow:
      inset 0 0 0 1px var(--primary-line),
      var(--glow-20);
  }

  .orow.sel .nm {
    color: var(--primary-ink);
  }

  .nm {
    overflow: hidden;
    color: var(--fg);
    font: var(--t-mono);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .orow.dimmed .nm {
    color: var(--fg-2);
  }

  .sig {
    overflow: hidden;
    color: var(--fg-4);
    font: var(--t-mono-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .in {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }

  .in b,
  .out {
    color: var(--fg);
    font: var(--t-mono-sm);
    font-variant-numeric: tabular-nums;
  }

  .in .bar {
    width: 36px;
    height: 3px;
  }

  .zero b,
  .out.zero {
    color: var(--fg-4);
  }

  .ln {
    color: var(--fg-4);
    font: var(--t-mono-sm);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }

  .note {
    padding: 10px 8px 4px;
    color: var(--fg-3);
    font: var(--t-caption);
    line-height: 1.5;
  }
</style>
