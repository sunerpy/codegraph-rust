<!--
  What would need re-checking if this symbol changed (design spec §3.2).

  The bar exists because the numbers alone do not answer the question a reader
  actually has, which is comparative: is 19 dependents a lot? So both fills are
  drawn against the widest radius in the index (`/api/stats` → `blastScale`),
  and the legend says so rather than letting a full-width bar imply "everything".

  The scale is sampled, not exhaustive — measuring every symbol's radius means a
  traversal per symbol. When the symbol on screen is wider than the sample found,
  it becomes the scale instead of overflowing it: a bar that runs past its track
  is a drawing bug, and clamping silently would be a lie about the comparison.
-->
<script lang="ts">
  import { fileHref } from '../../lib/navigation';
  import { plural } from '../../lib/symbol-model';
  import type { WireBlastScale, WireBlastSummary } from '../../lib/api';

  interface Props {
    blast: WireBlastSummary;
    scale: WireBlastScale | null;
    /** Calls from test files — the tests that would catch a regression. */
    testCalls: number;
    testFiles: number;
  }

  let { blast, scale, testCalls, testFiles }: Props = $props();

  let maxDirect = $derived(Math.max(1, scale?.maxDirect ?? 0, blast.direct));
  let maxWithin = $derived(Math.max(1, scale?.maxWithinHops ?? 0, blast.withinHops));

  const share = (value: number, max: number): number =>
    value <= 0 ? 0 : Math.max(0.5, Math.min(100, (100 * value) / max));
</script>

<div class="blast">
  <div class="bh">
    <span class="micro">Blast radius</span>
    <span
      class="scale"
      title={scale?.estimated
        ? `Scaled to the widest radius in the index, measured across its ${scale.sampled} most-depended-on symbols.`
        : 'Scaled to the widest radius in the index.'}>vs widest {maxWithin.toLocaleString()}</span
    >
  </div>

  <div class="tiles">
    <div class="stat" title="Symbols that depend on this one directly">
      <span class="label">Direct</span>
      <span class="value">{blast.direct.toLocaleString()}</span>
      <span class="bar"><i style:width={`${share(blast.direct, maxDirect)}%`}></i></span>
    </div>
    <div class="stat" title={`Symbols within ${blast.hops} hops`}>
      <span class="label">≤ {blast.hops} hops</span>
      <span class="value">{blast.withinHops.toLocaleString()}</span>
      <span class="bar"><i style:width={`${share(blast.withinHops, maxWithin)}%`}></i></span>
    </div>
    <div class="stat" title="Production files that would need re-checking">
      <span class="label">Prod files</span>
      <span class="value">{Math.max(0, blast.files - blast.testFiles).toLocaleString()}</span>
    </div>
    <div class="stat" title="Test files that would catch a regression">
      <span class="label">Test files</span>
      <span class="value">{blast.testFiles.toLocaleString()}</span>
      <span class="bar green"><i style:width={`${share(blast.testFiles, Math.max(1, blast.files))}%`}></i></span>
    </div>
  </div>
  {#if blast.routes > 0}
    <div class="routes">{plural(blast.routes, 'route')} within reach</div>
  {/if}

  {#if blast.topFiles.length > 0}
    <details>
      <summary>What would need re-checking</summary>
      <div class="body">
        {#each blast.topFiles as entry (entry.file)}
          <div class="fp">
            <a href={fileHref(entry.file)} class:test={entry.test}>{entry.file}</a>
            <b>{entry.symbols}</b>
          </div>
        {/each}
        {#if blast.files > blast.topFiles.length}
          <div class="fp dim">+{blast.files - blast.topFiles.length} more files</div>
        {/if}
        {#if testCalls > 0}
          <div class="note">
            plus {plural(testCalls, 'call')} from {plural(testFiles, 'test file')} — the tests that
            would catch a regression.
          </div>
        {/if}
      </div>
    </details>
  {/if}
</div>

<style>
  .bh {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 10px;
  }

  .scale {
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .tiles {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
  }

  .routes {
    margin-top: 8px;
    color: var(--fg-3);
    font: var(--t-caption);
  }

  details {
    margin-top: 10px;
  }

  summary {
    display: flex;
    align-items: center;
    gap: 6px;
    color: var(--fg-2);
    cursor: pointer;
    font: var(--t-small);
    list-style: none;
  }

  summary::-webkit-details-marker {
    display: none;
  }

  summary::before {
    width: 10px;
    color: var(--fg-3);
    content: '+';
    font-family: var(--mono);
  }

  details[open] summary::before {
    content: '−';
  }

  .body {
    padding-top: 6px;
  }

  .fp {
    display: flex;
    justify-content: space-between;
    gap: 10px;
    padding: 2px 0;
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .fp a {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fp a:hover {
    color: var(--fg);
    text-decoration: underline;
  }

  .fp a.test {
    color: var(--fg-3);
  }

  .fp b {
    color: var(--fg);
    font-weight: 500;
    font-variant-numeric: tabular-nums;
  }

  .note {
    padding-top: 6px;
    color: var(--fg-3);
    font: var(--t-caption);
  }
</style>
