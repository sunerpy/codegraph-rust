<!--
  Called by — the left rail (design spec §3.2).

  Grouped by file, the symbol's own file first as "same file", because the
  first question about a caller is "is this local, or does it come from
  somewhere else in the repo". The call-site chips (`:4657`) are the useful
  part: clicking one opens the caller already scrolled to the line that makes
  the call, which is the step a reader would otherwise do by hand.

  This rail draws no connectors. It scrolls independently of the code, so a
  line drawn to a caller row would point at the wrong place the moment either
  side moved.
-->
<script lang="ts">
  import Icon from '../Icon.svelte';
  import KindGlyph from '../KindGlyph.svelte';
  import { fileHref } from '../../lib/navigation';
  import { hot, railFocus } from '../../lib/focus.svelte';
  import { basename, plural, type CallerRailModel, type CallerRow } from '../../lib/symbol-model';
  import type { WireNodeRef } from '../../lib/api';

  interface Props {
    model: CallerRailModel;
    /** What the "Filter callers" box holds; the view owns the filtered model. */
    query?: string;
    onquery?: (query: string) => void;
    /** The symbol this one was reached from, when it is a caller. */
    originId: string | null;
    exported: boolean;
    /** Follow a caller, optionally landing on one of its call sites. */
    onstepUp: (node: WireNodeRef, line?: number) => void;
  }

  let { model, query = '', onquery, originId, exported, onstepUp }: Props = $props();

  let filtering = $derived(query.trim() !== '');
  let listed = $derived(
    model.groups.reduce((n, g) => n + g.rows.length, 0) + model.tests.rows.length + model.uncertain.length
  );

  /** A file node's own "symbol" is the file's top level; say so. */
  function rowName(node: WireNodeRef): string {
    return node.kind === 'file' ? `${basename(node.file)} (top level)` : node.name;
  }

  function rowTitle(row: CallerRow): string {
    return `${row.relation.node.qualifiedName} — ${row.relation.node.file}:${row.relation.node.line}`;
  }

  /**
   * A row's place in the flat order the keyboard walks (file groups in order,
   * folds excluded — arrowing into collapsed content would move a selection
   * nobody can see). Computed from the group offsets so the rail can stay a
   * nested render while the keyboard sees one list.
   */
  function indexOf(groupIndex: number, rowIndex: number): number {
    let base = 0;
    for (let i = 0; i < groupIndex; i++) base += model.groups[i]?.rows.length ?? 0;
    return base + rowIndex;
  }
</script>

<div class="rail-h">
  <span class="title"><span class="lead"><Icon name="corner-down-right" /></span>Called by <span class="count">{model.total}</span></span>
</div>

{#if model.total > 0}
  <label class="filter">
    <span class="lead"><Icon name="filter" /></span>
    <input
      type="search"
      placeholder="Filter callers"
      autocomplete="off"
      spellcheck="false"
      value={query}
      oninput={(e) => onquery?.(e.currentTarget.value)}
      aria-label="Filter callers by name or file"
    />
  </label>
{/if}

{#if model.total === 0}
  <div class="note">
    Nothing in the graph calls or references this symbol{exported
      ? ' — it is exported, so callers may live outside the index (or it is an entry point).'
      : '.'}
  </div>
{:else if filtering && listed === 0}
  <div class="note">No caller matches “{query.trim()}”.</div>
{/if}

{#each model.groups as group, groupIndex (group.file)}
  <div class="filegroup">
    <div class="fpath">
      <Icon name="folder" size={14} />
      <a href={fileHref(group.file)} title={group.file}>{group.same ? 'same file' : group.file}</a>
      <b>{group.rows.length}</b>
    </div>
    {#each group.rows as row, rowIndex (row.relation.node.id)}
      {@const node = row.relation.node}
      {@const isOrigin = node.id === originId}
      <div
        class="row"
        class:origin={isOrigin}
        class:sel={railFocus.at('left', indexOf(groupIndex, rowIndex))}
        data-target={node.id}
        role="button"
        tabindex="0"
        title={rowTitle(row)}
        onclick={() => onstepUp(node)}
        onkeydown={(e) => {
          if (e.key === 'Enter' || e.key === ' ') {
            e.preventDefault();
            onstepUp(node);
          }
        }}
        onmouseenter={() => hot.set(node.id)}
        onmouseleave={() => hot.clear(node.id)}
      >
        <KindGlyph kind={node.kind} size={22} />
        <div class="txt">
          <div class="nm">{rowName(node)}</div>
          <div class="meta">
            {#if row.words.length > 0}<span class="kindlbl">{row.words.join(', ')}</span>{/if}
            {#each row.when as w (w)}<span class="tag when" title="The call runs only under this condition — read from the source as it is now"
                >when {w}</span
              >{/each}
            {#each row.lines as line (line)}
              <button
                type="button"
                class="chip"
                title={`Open ${node.name} at line ${line}`}
                onclick={(e) => {
                  e.stopPropagation();
                  onstepUp(node, line);
                }}>:{line}</button
              >
            {/each}
            {#if row.via}<span class="kindlbl">via {row.via}</span>{/if}
          </div>
          {#if isOrigin}<div class="came"><Icon name="corner-up-left" size={12} />you came from here</div>{/if}
        </div>
      </div>
    {/each}
  </div>
{/each}

{#if model.tests.rows.length > 0}
  <details class="fold tests" open={filtering || undefined}>
    <summary>
      <span class="flask"><Icon name="flask-conical" /></span>
      <span class="ft">Tests</span>
      <span class="dim">{plural(model.tests.calls, 'call')} · {plural(model.tests.files.length, 'file')}</span>
      <span class="chev"><Icon name="chevron-right" size={14} /></span>
    </summary>
    <div class="body">
      {#each model.tests.rows as row (row.relation.node.id)}
        {@const node = row.relation.node}
        <div
          class="trow"
          role="button"
          tabindex="0"
          title={rowTitle(row)}
          onclick={() => onstepUp(node)}
          onkeydown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              onstepUp(node);
            }
          }}
          onmouseenter={() => hot.set(node.id)}
          onmouseleave={() => hot.clear(node.id)}
        >
          <KindGlyph kind={node.kind} />
          <span class="nm">{rowName(node)}</span>
          <span class="tf">{basename(node.file)}</span>
        </div>
      {/each}
    </div>
  </details>
{/if}

{#if model.uncertain.length > 0}
  <details class="fold" open={filtering || undefined}>
    <summary>
      <span class="flask dim"><Icon name="circle-dashed" /></span>
      <span class="ft">Uncertain</span>
      <span class="dim"
        >{model.uncertain.length} name-only match{model.uncertain.length === 1 ? '' : 'es'} · &lt; 0.6</span
      >
      <span class="chev"><Icon name="chevron-right" size={14} /></span>
    </summary>
    <div class="body">
      {#each model.uncertain as row (row.relation.node.id)}
        {@const node = row.relation.node}
        <div
          class="trow uncertain"
          role="button"
          tabindex="0"
          title={rowTitle(row)}
          onclick={() => onstepUp(node)}
          onkeydown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              onstepUp(node);
            }
          }}
          onmouseenter={() => hot.set(node.id)}
          onmouseleave={() => hot.clear(node.id)}
        >
          <KindGlyph kind={node.kind} />
          <span class="nm">{rowName(node)}</span>
          <span class="tf">{basename(node.file)}{#if row.relation.confidence !== null} · {row.relation.confidence}{/if}</span>
        </div>
      {/each}
    </div>
  </details>
{/if}

{#if model.hiddenGroups > 0}
  <div class="note">
    +{model.hiddenGroups} more caller{model.hiddenGroups === 1 ? '' : 's'} not shown — this symbol
    has more than the rail lists.
  </div>
{/if}

<style>
  /* §8 D-02 Called by: header 52, the filter box, file groups under a
     folder-and-path line, rows 46 high (the origin 64) at r 10. */
  .rail-h {
    position: sticky;
    top: 0;
    z-index: 2;
    display: flex;
    align-items: center;
    height: 52px;
    padding: 0 16px;
    background: var(--panel);
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

  .filter {
    position: relative;
    display: block;
    margin: 0 12px 6px;
  }

  .filter .lead {
    position: absolute;
    top: 8px;
    left: 12px;
    display: inline-flex;
    color: var(--fg-3);
    pointer-events: none;
  }

  .filter input {
    width: 100%;
    height: 32px;
    padding: 0 10px 0 36px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--raised);
    color: var(--fg);
    font: var(--t-small);
  }

  .filter input:hover {
    border-color: var(--line-strong);
  }

  .filter input:focus {
    border-color: var(--primary-line);
    outline: none;
    box-shadow: var(--glow-25);
  }

  .filter input::placeholder {
    color: var(--fg-3);
  }

  .filegroup {
    padding: 10px 10px 2px;
  }

  .fpath {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 6px 8px;
    color: var(--fg-3);
    font: var(--t-mono-sm);
  }

  .fpath a {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fpath a:hover {
    color: var(--fg);
    text-decoration: underline;
  }

  .fpath b {
    color: var(--fg-2);
    font-weight: 500;
  }

  .row {
    position: relative;
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr);
    gap: 10px;
    align-items: start;
    min-height: 46px;
    margin-bottom: 6px;
    padding: 11px 12px 8px;
    border: 1px solid var(--line-faint);
    border-radius: 10px;
    background: var(--card);
    cursor: pointer;
    transition:
      background-color 120ms,
      border-color 120ms;
  }

  .row:hover {
    border-color: var(--line);
    background: var(--raised);
  }

  .row.sel {
    border-color: var(--fg-2);
  }

  .row:focus-visible {
    outline-offset: 4px;
  }

  /* The origin: `primary-soft` + `primary-line`, the 22 % glow and a 2px
     GRAD.brand bar against the left edge. */
  .row.origin {
    border-color: var(--primary-line);
    background: var(--primary-soft);
    box-shadow: var(--glow-22);
  }

  .row.origin::before {
    position: absolute;
    top: 10px;
    bottom: 10px;
    left: -1px;
    width: 2px;
    border-radius: 2px;
    background: var(--grad-brand-v);
    content: '';
  }

  .txt {
    min-width: 0;
  }

  .nm {
    overflow: hidden;
    color: var(--fg);
    font: var(--t-mono-500);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 8px;
    align-items: center;
    margin-top: 3px;
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .tag {
    flex: 0 0 auto;
    height: 18px;
    padding: 0 7px;
    border-radius: 9px;
    background: var(--amber-soft);
    color: var(--amber);
    font: var(--t-mono-sm);
    line-height: 18px;
  }

  .kindlbl {
    color: var(--fg-3);
  }

  .chip {
    height: 18px;
    padding: 0 7px;
    border-radius: 9px;
    background: var(--raised);
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .row:hover .chip {
    background: var(--overlay);
  }

  .chip:hover {
    color: var(--fg);
    box-shadow: inset 0 0 0 1px var(--line-strong);
  }

  .came {
    display: flex;
    align-items: center;
    gap: 5px;
    margin-top: 6px;
    color: var(--primary-ink);
    font: var(--t-caption);
  }

  /* Tests and uncertain folds: 44 high closed, `raised` + `line`, r 10. */
  .fold {
    margin: 8px 10px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--raised);
  }

  .fold > summary {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 44px;
    padding: 0 12px;
    color: var(--fg-2);
    cursor: pointer;
    font: var(--t-caption);
    list-style: none;
  }

  .fold > summary::-webkit-details-marker {
    display: none;
  }

  .flask {
    display: inline-flex;
    color: var(--green);
  }

  .flask.dim {
    color: var(--fg-3);
  }

  .ft {
    color: var(--fg);
    font: var(--t-body-500);
  }

  .chev {
    display: inline-flex;
    margin-left: auto;
    color: var(--fg-3);
    transition: transform 120ms;
  }

  .fold[open] .chev {
    transform: rotate(90deg);
  }

  .fold .body {
    display: flex;
    max-height: 260px;
    flex-direction: column;
    overflow: auto;
    padding: 0 8px 8px;
  }

  .trow {
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr) auto;
    align-items: center;
    gap: 8px;
    min-height: 26px;
    padding: 0 6px;
    border-radius: 6px;
    cursor: pointer;
  }

  .trow:hover {
    background: var(--overlay);
  }

  .trow .nm {
    font: var(--t-mono);
  }

  .trow.uncertain .nm {
    color: var(--fg-2);
    text-decoration: underline dotted var(--fg-4);
    text-underline-offset: 3px;
  }

  .tf {
    overflow: hidden;
    color: var(--fg-3);
    font: var(--t-caption);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .note {
    padding: 8px 16px;
    color: var(--fg-3);
    font: var(--t-caption);
    line-height: 1.45;
  }
</style>
