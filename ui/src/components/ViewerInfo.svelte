<!--
  The rail's settings panel: what this viewer is reading and how it behaves.
  The viewer has nothing to configure from the page — the port, read-only and
  live watching are command-line choices — so the panel says what they are,
  each line read from the same payloads the screens use.
-->
<script lang="ts">
  import { project } from '../lib/project.svelte';
  import { live } from '../lib/live.svelte';
  import { trails } from '../lib/trails.svelte';

  $effect(() => {
    void trails.ensure();
  });

  let stats = $derived(project.stats);

  let indexed = $derived.by(() => {
    const at = stats?.index.lastIndexedAt;
    if (!at) return null;
    return new Date(at).toISOString().slice(0, 16).replace('T', ' ');
  });

  let indexLine = $derived(
    stats
      ? [
          stats.index.state ?? 'unknown',
          stats.index.extractionVersion !== null ? `extraction ${stats.index.extractionVersion}` : null,
          indexed ? `${indexed} UTC` : null,
        ]
          .filter(Boolean)
          .join(' · ')
      : '…'
  );

  let liveLine = $derived.by(() => {
    if (live.degraded !== null) return `off — ${live.degraded}`;
    if (live.stopped) return 'not connected — focus this tab to try again';
    return 'on — screens refresh when the index or a file changes';
  });

  let trailsLine = $derived(
    trails.canSave
      ? `saved to ${trails.directory ?? '.codegraph/ui/trails'}`
      : (trails.readOnlyReason ?? 'not saved')
  );
</script>

<div class="info">
  <div class="micro">This viewer</div>
  <dl>
    <dt>Project</dt>
    <dd class="mono">{stats?.project.root ?? '…'}</dd>
    <dt>Index</dt>
    <dd>{indexLine}</dd>
    <dt>Live</dt>
    <dd>{liveLine}</dd>
    <dt>Trails</dt>
    <dd>{trailsLine}</dd>
  </dl>
  <p class="dim">Port, read-only and watching are set on the command line: <code>codegraph ui --help</code>.</p>
</div>

<style>
  .info {
    color: var(--fg-2);
    font: var(--t-small);
  }

  dl {
    display: grid;
    grid-template-columns: 56px minmax(0, 1fr);
    gap: 6px 10px;
    margin: 8px 0 10px;
  }

  dt {
    color: var(--fg-3);
  }

  dd {
    margin: 0;
    overflow-wrap: anywhere;
    color: var(--fg);
  }

  dd.mono {
    font: var(--t-mono-sm);
  }

  p {
    margin: 0;
    font: var(--t-caption);
  }

  code {
    font: var(--t-mono-sm);
  }
</style>
