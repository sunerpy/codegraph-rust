<!--
  One side of the File view: the files this one depends on, or the files that
  depend on it (design spec §3.4).

  Both rails are the same component because the row is the same thing in both
  directions — a file, whether it is reachable or reaching. The count in the
  header is the engine's own `getFileDependencies` / `getFileDependents`
  answer, so a rail and a blast radius can never disagree about how far a
  change here goes.

  Imports that resolved to nothing indexed — packages, runtime builtins — sit
  below the files, in `--ink-3` and not clickable. Leaving them out would make
  a file importing `react`, `fs` and one local module show a single row and
  read as broken.
-->
<script lang="ts">
  import Icon from '../Icon.svelte';
  import { fileHref } from '../../lib/navigation';
  import { plural } from '../../lib/symbol-model';
  import type { FileRailModel, FileRailRow } from '../../lib/file-model';

  interface Props {
    title: string;
    model: FileRailModel;
    /** "none in the graph" reads wrong for both directions; each says its own. */
    emptyNote: string;
    side: 'left' | 'right';
    /** Index of the keyboard's position in this rail, or -1. */
    selected?: number;
    onhover?: (index: number) => void;
  }

  let { title, model, emptyNote, side, selected = -1, onhover }: Props = $props();

  /**
   * A path is drawn as a shrinkable directory plus a basename that never
   * truncates: the last segment is what tells two `index.ts` apart, so it is
   * the one part of a 300px column that must survive.
   */
  function dirOf(path: string): string {
    const cut = path.lastIndexOf('/');
    return cut < 0 ? '' : path.slice(0, cut + 1);
  }

  function baseOf(path: string): string {
    return path.slice(path.lastIndexOf('/') + 1);
  }

  function rowTitle(row: FileRailRow): string {
    if (row.symbols.length === 0) return row.path;
    const names = row.symbols.map((s) => s.name).join(', ');
    const more = row.symbolCount > row.symbols.length ? ', …' : '';
    return `${row.path} — ${names}${more}`;
  }
</script>

<div class="rail" class:right={side === 'right'} aria-label={title}>
  <div class="rail-h">
    <span class="title"
      ><span class="lead"><Icon name={side === 'left' ? 'corner-down-right' : 'arrow-right'} /></span>{title}
      <span class="count">{model.total}</span></span
    >
  </div>

  {#if model.rows.length === 0}
    <div class="note">{emptyNote}</div>
  {/if}

  {#each model.rows as row, index (row.path)}
    <a
      class="filerow"
      class:test={row.test}
      class:sel={index === selected}
      href={fileHref(row.path)}
      title={rowTitle(row)}
      onmouseenter={() => onhover?.(index)}
    >
      <span class="fi"><Icon name="file-code-2" size={14} /></span>
      <span class="p"><span class="dir">{dirOf(row.path)}</span><span class="base"
          >{baseOf(row.path)}</span
        ></span>
      {#if row.symbolCount > 0}
        <span class="n2">{row.symbolCount}</span>
      {/if}
    </a>
  {/each}

  {#if model.testCount > 0 && model.testCount < model.rows.length}
    <div class="note dim">
      {plural(model.testCount, 'test file')} at the end of the list.
    </div>
  {/if}

  {#if model.outside.length > 0}
    <div class="sub micro">Outside the index · {model.outside.length}</div>
    <div class="outside">
      {#each model.outside as row (row.name)}
        <span class="pill bordered mono" title={`imported at line ${row.lines.join(', ')}`}
          >{row.name}{#if row.lines.length > 1}<span class="dim"> ×{row.lines.length}</span>{/if}</span
        >
      {/each}
    </div>
    <div class="note">
      Listed, not linked — packages and runtime modules the index does not contain.
    </div>
  {/if}
</div>

<style>
  /* §8 D-03 rails: header 52, file rows 28 high at a 30 pitch with the
     count right-aligned; what leaves the index as bordered pills. */
  .rail {
    height: 100%;
    overflow: auto;
    padding-bottom: 12px;
  }

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

  .filerow {
    display: grid;
    grid-template-columns: 14px minmax(0, 1fr) auto;
    gap: 8px;
    align-items: center;
    height: 28px;
    margin: 0 10px 2px;
    padding: 0 8px;
    border-radius: 8px;
    color: var(--fg-2);
    font: var(--t-mono);
    text-decoration: none;
  }

  a.filerow:hover,
  a.filerow.sel {
    background: var(--raised);
    color: var(--fg);
  }

  a.filerow.sel {
    box-shadow: inset 0 0 0 1px var(--fg-2);
  }

  .fi {
    display: inline-flex;
    color: var(--fg-4);
  }

  .p {
    display: flex;
    min-width: 0;
  }

  .dir {
    overflow: hidden;
    flex: 0 1 auto;
    color: var(--fg-3);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .base {
    flex: 0 0 auto;
    color: var(--fg);
    white-space: nowrap;
  }

  .filerow.test .base {
    color: var(--fg-3);
  }

  .n2 {
    min-width: 26px;
    padding: 1px 7px;
    border-radius: 9px;
    background: var(--raised);
    color: var(--fg-2);
    font: var(--t-mono-sm);
    font-variant-numeric: tabular-nums;
    text-align: center;
  }

  .sub {
    margin: 18px 16px 10px;
  }

  .outside {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 0 16px;
  }

  .note {
    padding: 10px 16px;
    color: var(--fg-3);
    font: var(--t-caption);
    line-height: 1.45;
  }

  .note.dim {
    color: var(--fg-4);
  }
</style>
