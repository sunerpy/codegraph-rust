<script lang="ts">
  /**
   * The results panel under the search box (design spec §3.7).
   *
   * It renders whatever `palette.view` is: the entry points when the box is
   * empty, the ranked kind groups when it is not. The keyboard lives in
   * `SearchPalette` (the keys are pressed in the input, not here) and arrives as the
   * `selected` index; this component's only job beyond drawing is keeping that
   * row in view when the selection moves past the panel's edge.
   */
  import PaletteRows from './PaletteRows.svelte';
  import { palette } from '../lib/palette.svelte';
  import type { PaletteItem } from '../lib/search-model';

  interface Props {
    onpick: (item: PaletteItem) => void;
  }

  let { onpick }: Props = $props();

  let panel: HTMLDivElement | null = $state(null);
  let view = $derived(palette.view);

  $effect(() => {
    const index = palette.selected;
    if (!panel) return;
    const row = panel.querySelector(`[data-palette-row="${index}"]`);
    row?.scrollIntoView({ block: 'nearest' });
  });
</script>

<div class="panel" bind:this={panel} id="palette-panel" role="listbox" aria-label="Search results">
  {#if view.hint}
    <p class="hint">{view.hint}</p>
  {/if}

  <PaletteRows
    palette={view}
    selected={palette.selected}
    rowRole="option"
    {onpick}
    onhover={(index) => palette.select(index)}
  />

  {#if palette.failure}
    <p class="note">{palette.failure}</p>
  {:else if palette.pending && view.items.length === 0}
    <p class="note">Searching…</p>
  {:else if view.empty}
    <p class="note empty">{view.empty}</p>
  {/if}

  <div class="keys" aria-hidden="true">
    <span><span class="kbd">↑</span><span class="kbd">↓</span> move</span>
    <span><span class="kbd">⏎</span> open</span>
    <span><span class="kbd">esc</span> close</span>
  </div>
</div>

<style>
  /* §7 search palette: under the command input at its x and width, r 12,
     `overlay` + `line-strong` + SH.pop; key hints along the bottom. */
  .panel {
    position: absolute;
    z-index: 40;
    top: 42px;
    right: 0;
    left: 0;
    display: flex;
    max-height: min(480px, calc(100vh - 80px));
    flex-direction: column;
    gap: 2px;
    overflow: auto;
    padding: 8px;
    border: 1px solid var(--line-strong);
    border-radius: 12px;
    background: var(--overlay);
    box-shadow: var(--sh-pop);
  }

  .hint {
    margin: 0 0 4px;
    padding: 8px 10px;
    border-radius: 8px;
    background: var(--raised);
    color: var(--fg-2);
    font: var(--t-small);
  }

  .note {
    margin: 0;
    padding: 10px;
    color: var(--fg-3);
    font: var(--t-small);
  }

  .note.empty {
    color: var(--fg-2);
  }

  .keys {
    position: sticky;
    bottom: -8px;
    display: flex;
    gap: 14px;
    margin: 4px -8px -8px;
    padding: 8px 12px;
    border-top: 1px solid var(--line-faint);
    background: var(--overlay);
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .keys > span {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
</style>
