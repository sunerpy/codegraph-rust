<!--
  One section of the entry-points panel — routes, executable files, tests, hubs.

  The file-group + row shapes are the Symbol view's caller rail (design spec
  §3.2, `.filegroup` / `.row`), reused rather than re-invented: they are the
  repo's established "a list of code, grouped by where it lives", and a second
  visual language for the same idea is how a small app starts looking like two.

  Every row does two things. Clicking it opens the code — a handler, a file, a
  hub. The `Flow ›` chip beside it arms a flow FROM that symbol, which the panel
  then completes with a second name. Rows that name no callable symbol carry no
  chip: `/api/flow` searches by name, and a file has none the path finder can
  look up.
-->
<script lang="ts">
  import Icon from '../Icon.svelte';
  import KindGlyph from '../KindGlyph.svelte';
  import { fileHref } from '../../lib/navigation';
  import type { EntryRow, EntrySection } from '../../lib/entry-model';

  interface Props {
    section: EntrySection;
    /** The row currently armed as a flow's start, by row id. */
    armed: string | null;
    onopen: (row: EntryRow) => void;
    onflow: (row: EntryRow) => void;
  }

  let { section, armed, onopen, onflow }: Props = $props();
</script>

<section class="sec" aria-labelledby={`entry-${section.id}`}>
  <div class="sec-h">
    <h3 id={`entry-${section.id}`}>{section.title}</h3>
    <span class="meta">{section.meta}</span>
  </div>
  <p class="note">{section.note}</p>

  {#each section.groups as group (group.path)}
    <div class="filegroup">
      <div class="fpath">
        <Icon name="folder" size={14} />
        {#if group.file}
          <a href={fileHref(group.file)} title={group.file}>{group.path}</a>
        {:else}
          <span title={group.path}>{group.path}</span>
        {/if}
        <b>{group.rows.length}</b>
      </div>
      {#each group.rows as row (row.id)}
        <div class="row" class:armed={armed === row.id} class:stub={!row.target}>
          <KindGlyph kind={row.kind} size={22} />
          <div class="body">
            <div class="line">
              {#if row.target}
                <button
                  type="button"
                  class="nm"
                  title={row.title}
                  data-entry-row={row.id}
                  onclick={() => onopen(row)}
                >
                  {#if row.method}<span class="verb">{row.method}</span>{/if}{row.name}
                </button>
              {:else}
                <span class="nm plain" title={row.title}>
                  {#if row.method}<span class="verb">{row.method}</span>{/if}{row.name}
                </span>
              {/if}
              {#if row.flowFrom}
                {@const label =
                  armed === null ? 'Flow ›' : armed === row.id ? 'Cancel' : '→ here'}
                <button
                  type="button"
                  class="btn chip"
                  title={armed === null
                    ? `Start a flow from ${row.flowFrom}`
                    : armed === row.id
                      ? 'Stop drawing a flow from here'
                      : `Draw the path that ends at ${row.flowFrom}`}
                  data-entry-flow={row.id}
                  onclick={() => onflow(row)}>{#if armed === null}<Icon name="workflow" />{/if}{label}</button
                >
              {/if}
            </div>
            <div class="meta">{row.meta}</div>
          </div>
        </div>
      {/each}
    </div>
  {/each}

  {#if section.shown < section.total}
    <p class="note dim">
      Showing {section.shown} of {section.floor ? 'at least ' : ''}{section.total} — the rest are in
      the index, not on this list.
    </p>
  {/if}
</section>

<style>
  /* One entry-point list as a D card: title `label`, a caption under it,
     rows as `card` + `line-faint` r 10 with a ghost Flow button (§8 D-01). */
  .sec {
    padding: 4px 18px 16px;
    border: 1px solid var(--line-faint);
    border-radius: 12px;
    background: var(--panel);
  }

  .sec-h {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 12px;
    padding: 14px 0 2px;
  }

  .sec-h h3 {
    margin: 0;
    color: var(--fg);
    font: var(--t-label);
  }

  .sec-h .meta {
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .note {
    margin: 0;
    padding: 2px 0 6px;
    color: var(--fg-3);
    font: var(--t-caption);
    line-height: 1.45;
  }

  .note.dim {
    color: var(--fg-4);
  }

  .filegroup {
    padding-top: 10px;
  }

  .fpath {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 2px 8px;
    color: var(--fg-3);
    font: var(--t-mono-sm);
  }

  .fpath a,
  .fpath span {
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
    flex: 0 0 auto;
    color: var(--fg-2);
    font-weight: 500;
  }

  .row {
    position: relative;
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr);
    gap: 12px;
    align-items: center;
    margin-bottom: 6px;
    padding: 9px 12px;
    border: 1px solid var(--line-faint);
    border-radius: 10px;
    background: var(--card);
  }

  .row:hover {
    border-color: var(--line);
    background: var(--raised);
  }

  .row.armed {
    border-color: var(--primary-line);
    background: var(--primary-soft);
    box-shadow: var(--glow-25);
  }

  .body {
    min-width: 0;
  }

  .line {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .nm {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    color: var(--fg);
    font: var(--t-mono-500);
    text-align: left;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .nm:not(.plain) {
    cursor: pointer;
  }

  .nm:not(.plain):hover {
    color: var(--primary-ink);
  }

  .row.stub .nm {
    color: var(--fg-2);
  }

  .verb {
    margin-right: 8px;
    color: var(--amber);
    font-weight: 600;
  }

  .meta {
    margin-top: 2px;
    overflow: hidden;
    color: var(--fg-3);
    font: var(--t-mono-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .chip {
    flex: none;
    height: 28px;
    padding: 0 10px;
  }
</style>
