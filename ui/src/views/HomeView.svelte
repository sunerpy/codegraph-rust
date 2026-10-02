<script lang="ts">
  /**
   * Start — the answer to "where do I start" (docs/design/viewer-d.md §8 D-01).
   *
   * Nothing selected is the normal first state of a viewer opened on a project
   * nobody has read before. So the screen is a dashboard of what the index
   * already knows: the project and its index state with the three ways in
   * (the map, search, a flow), then six cards — what the graph holds, the
   * trails somebody kept, the symbols most code depends on, the files that
   * run something, the tests that reach furthest, and the languages. Every
   * number is read from `/api/stats`, `/api/entrypoints` or `/api/trails`;
   * nothing is guessed from a filename.
   *
   * The full-length entry-point lists, grouped by file and able to start a
   * flow, are `#/entry` (`EntryView`); the cards link to it.
   */
  import Icon from '../components/Icon.svelte';
  import KindGlyph from '../components/KindGlyph.svelte';
  import SavedTrails from '../components/SavedTrails.svelte';
  import { palette } from '../lib/palette.svelte';
  import { project } from '../lib/project.svelte';
  import { command } from '../lib/command.svelte';
  import { entryHref, fileHref, flowHref, mapHref, navigate } from '../lib/navigation';
  import { walkTo } from '../lib/walk';
  import type { WireEntryFile, WireEntryHub, WireEntryRoute, WireEntryTest } from '../lib/api';

  interface Props {
    project?: string | null;
  }
  let { project: projectName = null }: Props = $props();

  $effect(() => {
    void palette.ensureEntries();
    void project.ensure();
  });

  let stats = $derived(project.stats);
  let entries = $derived(palette.entries);

  const n = (value: number): string => value.toLocaleString();

  /** §3.3: the accents as a categorical palette — nodes, then edges. */
  const NODE_SERIES = ['var(--cyan)', 'var(--primary)', 'var(--violet)', 'var(--amber)', 'var(--green)'];
  const EDGE_SERIES = ['var(--cyan)', 'var(--line-strong)', 'var(--primary)', 'var(--violet)', 'var(--green)'];
  const OTHER = 'var(--fg-4)';

  interface Slice {
    label: string;
    count: number;
    colour: string;
    share: number;
  }

  /** The five biggest kinds and one "other" — a stacked bar and its legend. */
  function slices(byKind: Record<string, number> | undefined, palette_: string[]): Slice[] {
    if (!byKind) return [];
    const rows = Object.entries(byKind)
      .filter(([, count]) => count > 0)
      .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]));
    const total = rows.reduce((sum, [, count]) => sum + count, 0) || 1;
    const top = rows.slice(0, palette_.length);
    const rest = rows.slice(palette_.length).reduce((sum, [, count]) => sum + count, 0);
    const out = top.map(([kind, count], i) => ({
      label: kind.replace(/_/g, ' '),
      count,
      colour: palette_[i] ?? OTHER,
      share: (100 * count) / total,
    }));
    if (rest > 0) out.push({ label: 'other', count: rest, colour: OTHER, share: (100 * rest) / total });
    return out;
  }

  let nodeSlices = $derived(slices(stats?.graph.nodesByKind, NODE_SERIES));
  let edgeSlices = $derived(slices(stats?.graph.edgesByKind, EDGE_SERIES));

  let languages = $derived.by(() => {
    const rows = Object.entries(stats?.graph.filesByLanguage ?? {}).sort(
      (a, b) => b[1] - a[1] || a[0].localeCompare(b[0])
    );
    const top = rows.slice(0, 7);
    const rest = rows.slice(7);
    const max = Math.max(1, ...top.map(([, count]) => count));
    return {
      top: top.map(([language, count]) => ({ language, count, share: (100 * count) / max })),
      more: rest.length,
      moreFiles: rest.reduce((sum, [, count]) => sum + count, 0),
    };
  });

  let hubs = $derived((entries?.hubs.items ?? []).slice(0, 6));
  let hubMax = $derived(Math.max(1, ...hubs.map((h) => h.dependents)));
  let tests = $derived((entries?.tests.items ?? []).slice(0, 5));
  let testMax = $derived(Math.max(1, ...tests.map((t) => t.reaches)));

  /** Routes first when the project is a routed app; else files that run something. */
  let starts = $derived.by(() => {
    if (entries?.routes.routed) {
      return { kind: 'routes' as const, routes: entries.routes.items.items.slice(0, 4), files: [] as WireEntryFile[] };
    }
    return { kind: 'files' as const, routes: [] as WireEntryRoute[], files: (entries?.files.items ?? []).slice(0, 4) };
  });

  let indexedOn = $derived.by(() => {
    const at = stats?.index.lastIndexedAt;
    return at ? new Date(at).toISOString().slice(0, 10) : null;
  });

  let current = $derived(
    stats !== null && !stats.index.stale && (stats.index.state === null || stats.index.state === 'complete')
  );

  /** The directory a path sits in, as the cards' caption shows it. */
  function where(file: string): string {
    const parts = file.split('/');
    return parts.length > 2 ? parts[parts.length - 3] ?? parts[0] ?? '' : (parts[0] ?? '');
  }

  function openHub(hub: WireEntryHub): void {
    walkTo({ id: hub.id, name: hub.name, kind: hub.kind }, 'start');
  }

  function openTest(test: WireEntryTest): void {
    navigate(fileHref(test.file));
  }

  function openFile(file: WireEntryFile): void {
    navigate(fileHref(file.file));
  }

  function openRoute(route: WireEntryRoute): void {
    if (route.handlerId) walkTo({ id: route.handlerId, name: route.handler, kind: null }, 'start');
  }
</script>

<div class="start">
  <!-- The project, its index state, and the three ways in. -->
  <section class="island project">
    <span class="ptile"><Icon name="box" size={20} /></span>
    <div class="pt">
      <div class="pn">
        <h1>{projectName ?? stats?.project.name ?? '…'}</h1>
        {#if stats}
          {#if current}
            <span class="pill green"><span class="dot"></span>Current</span>
          {:else}
            <span class="pill amber"><span class="dot"></span>Stale</span>
          {/if}
        {/if}
      </div>
      {#if stats}
        <p class="meta">
          {[
            indexedOn ? `indexed ${indexedOn}` : null,
            `extraction version ${stats.index.extractionVersion ?? '?'}`,
            `${n(stats.graph.files)} files`,
            `${n(stats.graph.nodes)} nodes`,
            `${n(stats.graph.edges)} edges`,
          ]
            .filter(Boolean)
            .join(' · ')}
        </p>
      {/if}
    </div>
    <div class="actions">
      <a class="btn primary lg" href={mapHref()}><Icon name="network" />Open the map</a>
      <button type="button" class="btn secondary lg" onclick={() => command.requestSearch()}><Icon name="search" />Search</button>
      <a class="btn secondary lg" href={flowHref()}><Icon name="workflow" />Read a flow</a>
    </div>
  </section>

  <div class="cards">
    <section class="island card">
      <div class="ch"><Icon name="layers" /><h2>Index composition</h2></div>
      <p class="sub">what the graph holds, by node and edge kind</p>
      {#if stats}
        <div class="tiles">
          <div class="stat"><span class="label">Files</span><span class="value">{n(stats.graph.files)}</span></div>
          <div class="stat"><span class="label">Nodes</span><span class="value">{n(stats.graph.nodes)}</span></div>
          <div class="stat"><span class="label">Edges</span><span class="value">{n(stats.graph.edges)}</span></div>
        </div>
        {@render stacked('Nodes by kind', nodeSlices)}
        {@render stacked('Edges by kind', edgeSlices)}
      {:else}
        {@render skeleton(5)}
      {/if}
    </section>

    <section class="island card">
      <SavedTrails hideWhenEmpty={false} />
    </section>

    <section class="island card">
      <div class="ch"><Icon name="zap" /><h2>Most depended on</h2></div>
      <p class="sub">production symbols the most code reaches into</p>
      {#if entries}
        {#each hubs as hub (hub.id)}
          <button type="button" class="barrow" onclick={() => openHub(hub)} title={`${hub.qualifiedName} — ${hub.file}:${hub.line}`}>
            <span class="bl"><span class="mono nm">{hub.name}</span><span class="dim">{where(hub.file)}</span></span>
            <span class="bv mono">{n(hub.dependents)}</span>
            <span class="bar"><i style:width={`${Math.max(2, Math.round((100 * hub.dependents) / hubMax))}%`}></i></span>
          </button>
        {:else}
          <p class="dim">Nothing in this index is depended on yet.</p>
        {/each}
      {:else}
        {@render skeleton(6)}
      {/if}
    </section>

    <section class="island card">
      <div class="ch"><Icon name="command" /><h2>{starts.kind === 'routes' ? 'Routes' : 'Where it starts'}</h2></div>
      <p class="sub">
        {starts.kind === 'routes'
          ? `the URLs a request arrives on · ${entries?.frameworks.join(', ') ?? ''}`
          : 'files that run something at their top level'}
      </p>
      {#if entries}
        {#if starts.kind === 'routes'}
          {#each starts.routes as route (route.routeId)}
            <div class="erow">
              <KindGlyph kind="route" size={22} />
              <button type="button" class="et" onclick={() => openRoute(route)} disabled={!route.handlerId}>
                <span class="mono nm">{route.url}</span>
                <span class="mono dim">{route.handler} · {route.file.split('/').pop()}:{route.line}</span>
              </button>
              {#if route.method}<span class="pill amber mono">{route.method}</span>{/if}
            </div>
          {/each}
        {:else}
          {#each starts.files as file (file.id)}
            <div class="erow">
              <KindGlyph kind="file" size={22} />
              <button type="button" class="et" onclick={() => openFile(file)}>
                <span class="mono nm">{file.name}</span>
                <span class="mono dim">{file.file}</span>
              </button>
              <span class="pill cyan mono" title="Calls made at the top level">{file.calls} call{file.calls === 1 ? '' : 's'}</span>
            </div>
          {:else}
            <p class="dim">No file in this index runs anything at its top level.</p>
          {/each}
        {/if}
        <a class="more" href={entryHref()}>All entry points <Icon name="chevron-right" size={14} /></a>
      {:else}
        {@render skeleton(4)}
      {/if}
    </section>

    <section class="island card">
      <div class="ch"><Icon name="flask-conical" /><h2>Tests by reach</h2></div>
      <p class="sub">{entries ? `${n(entries.tests.total)} test files · ` : ''}how many other files each reaches</p>
      {#if entries}
        {#each tests as test (test.id)}
          <button type="button" class="barrow" onclick={() => openTest(test)} title={test.file}>
            <span class="bl"><span class="mono nm">{test.name}</span><span class="dim">{where(test.file)}</span></span>
            <span class="bv mono">{n(test.reaches)}</span>
            <span class="bar green"><i style:width={`${Math.max(2, Math.round((100 * test.reaches) / testMax))}%`}></i></span>
          </button>
        {:else}
          <p class="dim">No test file reaches into the rest of the index.</p>
        {/each}
      {:else}
        {@render skeleton(5)}
      {/if}
    </section>

    <section class="island card">
      <div class="ch"><Icon name="file-code-2" /><h2>Files by language</h2></div>
      <p class="sub">every indexed file, fixtures included</p>
      {#if stats}
        {#each languages.top as row (row.language)}
          <div class="barrow static">
            <span class="bl"><span class="nm">{row.language}</span></span>
            <span class="bv mono">{n(row.count)}</span>
            <span class="bar"><i style:width={`${Math.max(2, Math.round(row.share))}%`}></i></span>
          </div>
        {/each}
        {#if languages.more > 0}
          <p class="dim">+{languages.more} more language{languages.more === 1 ? '' : 's'} · {n(languages.moreFiles)} files</p>
        {/if}
      {:else}
        {@render skeleton(6)}
      {/if}
    </section>
  </div>
</div>

{#snippet stacked(title: string, parts: Slice[])}
  <div class="micro stk">{title}</div>
  <div class="stack" role="img" aria-label={`${title}: ${parts.map((p) => `${p.label} ${p.count}`).join(', ')}`}>
    {#each parts as part (part.label)}<i style:width={`${part.share}%`} style:background={part.colour}></i>{/each}
  </div>
  <div class="legend">
    {#each parts as part (part.label)}
      <span><i style:background={part.colour}></i>{part.label} {n(part.count)}</span>
    {/each}
  </div>
{/snippet}

{#snippet skeleton(count: number)}
  <div class="sk">
    {#each Array.from({ length: count }, (_, i) => i) as i (i)}
      <span class="skeleton" style:width={`${45 + ((i * 37) % 50)}%`}></span>
    {/each}
  </div>
{/snippet}

<style>
  /* §8 D-01: the project island across the top (84), then cards 3 x 2. */
  .start {
    display: flex;
    height: 100%;
    flex-direction: column;
    gap: var(--gap);
    overflow: auto;
  }

  .project {
    display: flex;
    flex: 0 0 auto;
    flex-wrap: wrap;
    align-items: center;
    gap: 14px 20px;
    min-height: 84px;
    padding: 18px 20px;
  }

  .ptile {
    display: inline-flex;
    width: 48px;
    height: 48px;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--raised);
    color: var(--primary-ink);
  }

  .pt {
    min-width: 0;
    flex: 1;
  }

  .pn {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .pn h1 {
    margin: 0;
    overflow: hidden;
    color: var(--fg);
    font: var(--t-display);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .meta {
    margin: 4px 0 0;
    color: var(--fg-3);
    font: var(--t-small);
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .cards {
    display: grid;
    flex: 1 0 auto;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    grid-auto-rows: minmax(352px, auto);
    gap: var(--gap);
  }

  .card {
    padding: 16px 18px 18px;
  }

  .ch {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--fg-3);
  }

  .ch h2 {
    margin: 0;
    color: var(--fg);
    font: var(--t-label);
  }

  .sub {
    margin: 6px 0 14px;
    color: var(--fg-4);
    font: var(--t-caption);
  }

  .card :global(.trails) {
    max-width: none;
  }

  .tiles {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
    margin-bottom: 16px;
  }

  .stk {
    margin: 12px 0 8px;
  }

  .stack {
    display: flex;
    height: 8px;
    overflow: hidden;
    border-radius: 4px;
    background: var(--raised);
  }

  .stack i {
    height: 100%;
  }

  .legend {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 14px;
    margin-top: 8px;
    color: var(--fg-2);
    font: var(--t-small);
  }

  .legend span {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .legend i {
    width: 8px;
    height: 8px;
    border-radius: 2px;
  }

  /* Bar rows: label + value right-aligned, a 4px track 20 below, pitch 36. */
  .barrow {
    display: grid;
    width: 100%;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 6px 10px;
    padding: 4px 0 10px;
    color: var(--fg);
    text-align: left;
  }

  button.barrow:hover .nm {
    color: var(--primary-ink);
  }

  .bl {
    display: flex;
    min-width: 0;
    align-items: baseline;
    gap: 8px;
  }

  .bl .nm {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .mono.nm {
    font: var(--t-mono-500);
  }

  .bl .dim {
    color: var(--fg-4);
    font: var(--t-caption);
  }

  .bv {
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .barrow .bar {
    grid-column: 1 / -1;
  }

  /* Entry rows: 52 high, `card` + `line-faint`, a ghost Flow button. */
  .erow {
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr) auto;
    align-items: center;
    gap: 12px;
    min-height: 52px;
    margin-bottom: 6px;
    padding: 6px 10px 6px 12px;
    border: 1px solid var(--line-faint);
    border-radius: 10px;
    background: var(--card);
  }

  .erow:hover {
    border-color: var(--line);
  }

  .et {
    display: flex;
    min-width: 0;
    flex-direction: column;
    text-align: left;
  }

  .et .nm,
  .et .dim {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .et .dim {
    color: var(--fg-3);
    font: var(--t-mono-sm);
  }

  .et:hover .nm {
    color: var(--primary-ink);
  }

  .more {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-top: 6px;
    color: var(--fg-2);
    font: var(--t-small-500);
  }

  .more:hover {
    color: var(--fg);
  }

  .dim {
    color: var(--fg-3);
  }

  .sk {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding-top: 6px;
  }

  @media (max-width: 1279px) {
    .cards {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  @media (max-width: 767px) {
    .cards {
      grid-template-columns: minmax(0, 1fr);
      grid-auto-rows: auto;
    }

    .actions {
      width: 100%;
    }

    .project .btn {
      flex: 1;
    }
  }
</style>
