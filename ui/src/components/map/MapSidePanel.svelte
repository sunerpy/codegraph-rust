<!--
  The Map's 320px side panel (design spec §3.6).

  Three jobs, in the order a reader needs them: say what the picture IS and how
  it was derived, account for everything the picture leaves out, and — once a
  module is selected — become that module's dependency sheet.

  The accounting is not decoration. A map that hides thin links, drops
  name-only edges and layers on declared ones is a map with three deliberate
  omissions in it; each of them gets a sentence here, because a diagram nobody
  can audit is a diagram that gets believed too much.
-->
<script lang="ts">
  import ExportButtons from '../ExportButtons.svelte';
  import Icon from '../Icon.svelte';
  import KindGlyph from '../KindGlyph.svelte';
  import { fileHref } from '../../lib/navigation';
  import { plural } from '../../lib/symbol-model';
  import type { WireMapLink, WireMapPayload } from '../../lib/api';
  import type { MapLayout } from '../../lib/map-model';

  interface Props {
    payload: WireMapPayload;
    layout: MapLayout;
    selected: string | null;
    includeTests: boolean;
    files: string[];
    onToggleTests: (value: boolean) => void;
    onSelectRoot: (root: string) => void;
    /** What the reader asked for, or `null` when the depth in `payload` was chosen for them. */
    chosenDepth: number | null;
    onSelectDepth: (depth: number | null) => void;
    onSelect: (id: string | null) => void;
    /** Builds the map as an SVG at a given device-pixel scale. */
    buildSvg: (scale: number) => string;
    /** File stem for a downloaded map, without an extension. */
    exportName: string;
  }

  let {
    payload,
    layout,
    selected,
    includeTests,
    files,
    onToggleTests,
    onSelectRoot,
    chosenDepth,
    onSelectDepth,
    onSelect,
    buildSvg,
    exportName,
  }: Props = $props();

  /**
   * The grouping options.
   *
   * The first one is the default and is not a number: the answering side reads
   * the repository and picks the shallowest grouping that is not one box
   * holding the whole program. The numbers below it are there for when its
   * choice is wrong for what the reader is looking at — an escape hatch, not
   * the thing anybody should have to reach for.
   */
  const DEPTHS = [1, 2, 3, 4] as const;

  function depthLabel(depth: number): string {
    return depth === 1 ? 'top-level folders' : `${depth} folders deep`;
  }

  /** An em dash the mono face has; the select is narrow enough to notice a tofu. */
  const DASH = '\u2014';

  const selectedNode = $derived(
    selected === null ? null : (layout.nodes.find((n) => n.id === selected) ?? null)
  );
  const selectedModule = $derived(selectedNode?.module ?? null);
  /** Which of the listed files are tool-generated — the rows drawn in ink-4. */
  const generatedFiles = $derived(new Set(selectedModule?.generatedFiles ?? []));

  const dependencies = $derived(
    selected === null
      ? []
      : layout.edges
          .filter((e) => e.source === selected)
          .map((e) => e.link)
          .sort((a, b) => b.count - a.count || a.target.localeCompare(b.target))
  );
  const dependents = $derived(
    selected === null
      ? []
      : layout.edges
          .filter((e) => e.target === selected)
          .map((e) => e.link)
          .sort((a, b) => b.count - a.count || a.source.localeCompare(b.source))
  );

  const thinCount = $derived(layout.edges.filter((e) => e.thin && !e.back).length);

  /** §8 D-05 inspector tabs. Selecting a module brings Overview forward. */
  type Tab = 'overview' | 'dependencies' | 'files';
  let tab = $state<Tab>('overview');
  $effect(() => {
    void selected;
    tab = 'overview';
  });

  const maxDependency = $derived(Math.max(1, ...dependencies.map((l) => l.count)));
  const maxDependent = $derived(Math.max(1, ...dependents.map((l) => l.count)));
</script>

<aside class="mapside island">
  <div class="tabs" role="tablist" aria-label="Inspector">
    <button type="button" role="tab" aria-selected={tab === 'overview'} class:on={tab === 'overview'} onclick={() => (tab = 'overview')}>Overview</button>
    <button type="button" role="tab" aria-selected={tab === 'dependencies'} class:on={tab === 'dependencies'} onclick={() => (tab = 'dependencies')}>Dependencies</button>
    <button type="button" role="tab" aria-selected={tab === 'files'} class:on={tab === 'files'} onclick={() => (tab = 'files')}>Files</button>
  </div>

  <div class="body">
  {#if tab === 'overview'}
    {#if selectedModule}
      <div class="mhead">
        <KindGlyph kind="module" size={32} />
        <div class="mt">
          <b class="mono">{selectedModule.id}</b>
          <span class="dim">{plural(selectedModule.files, 'file')}{#if selectedModule.languages.length > 0} · {selectedModule.languages.map((l) => l.language).join(', ')}{/if}</span>
        </div>
        <button type="button" class="iconbtn" aria-label="Clear the selection" title="Clear the selection" onclick={() => onSelect(null)}><Icon name="x" /></button>
      </div>

      <div class="tiles">
        <div class="stat"><span class="label">Symbols</span><span class="value">{selectedModule.symbols.toLocaleString()}</span></div>
        <div class="stat"><span class="label">Files</span><span class="value">{selectedModule.files.toLocaleString()}</span></div>
        <div class="stat"><span class="label">Dependents</span><span class="value">{(selectedModule.dependents?.files ?? 0).toLocaleString()}<small>files</small></span></div>
      </div>

      {#if selectedModule.generated > 0}
        <p class="dim">
          {selectedModule.generated === selectedModule.files
            ? 'Every file in it is tool-generated.'
            : `${selectedModule.generated} of its files are tool-generated.`}
        </p>
      {/if}

      {#if (selectedModule.dependents?.files ?? 0) > 0}
        <p class="reach">
          <b>{plural(selectedModule.dependents.files, 'file')}</b> outside it, across
          {plural(selectedModule.dependents.modules, 'module')}, reference straight into it — the
          floor on what a change here has to be checked against, and the bar along the bottom of
          the box.
        </p>
      {/if}

      {#if selectedNode?.island}
        <p class="callout amber">
          Nothing in the index depends on this module — no import, call or reference crosses into
          it. It may be an entry point, or reached in a way the graph cannot see.
        </p>
      {/if}

      {@render linkList('Depends on', dependencies, 'target', maxDependency)}
      {@render linkList('Depended on by', dependents, 'source', maxDependent)}
    {:else}
      <h2>Architecture map</h2>
      <p>
        Derived from the graph, not drawn by hand: each module sits one layer above the modules it
        depends on, so reading top to bottom follows the dependency direction. Line weight is how many
        calls, imports and type references cross the link.
      </p>
      <p class="dim">
        Hover a link to see what crosses it — the counts by kind and the symbol pairs behind the
        weight. Click a module to isolate its links and list its files.
      </p>
    {/if}

    <div class="section micro">View</div>
    <label class="field">
      <span>Showing</span>
      <select
        value={payload.root}
        onchange={(event) => onSelectRoot((event.currentTarget as HTMLSelectElement).value)}
      >
        {#each payload.roots as option (option.root)}
          <option value={option.root}>{option.label} · {option.files} files</option>
        {/each}
      </select>
    </label>

    <!-- The grouping. A repository whose whole program sits under one directory
         draws as one box at the shallowest setting, which is why the default is
         chosen from the repository rather than fixed at 1. -->
    <label class="field">
      <span>Grouping</span>
      <select
        value={chosenDepth === null ? 'auto' : String(chosenDepth)}
        onchange={(event) => {
          const value = (event.currentTarget as HTMLSelectElement).value;
          onSelectDepth(value === 'auto' ? null : Number(value));
        }}
      >
        <option value="auto">automatic {DASH} {depthLabel(payload.depth)}</option>
        {#each DEPTHS as option (option)}
          <option value={String(option)}>{depthLabel(option)}</option>
        {/each}
      </select>
    </label>

    <label class="toggle">
      <input
        type="checkbox"
        checked={includeTests}
        onchange={(event) => onToggleTests((event.currentTarget as HTMLInputElement).checked)}
      />
      <span class="switch" aria-hidden="true"></span>
      Include test modules
    </label>
  {:else if tab === 'dependencies'}
    <div class="notes">
      {#if thinCount > 0}
        <p class="dim">
          {plural(thinCount, 'link')} carrying fewer than {layout.minWeight} references
          {thinCount === 1 ? 'is' : 'are'} hidden until you select a module {thinCount === 1
            ? 'it'
            : 'they'} touch.
        </p>
      {/if}
      {#if layout.basis.kind === 'declared'}
        <p class="dim">
          The layering uses the {layout.basis.declaredLinks} of {layout.basis.totalLinks} links with an
          import, a qualified name, an inheritance clause or a typed receiver behind them. Bare
          name matches still count toward line weight, but they do not decide what sits above what.
        </p>
      {:else}
        <p class="dim">
          Too few links here carry an import or a declared type, so the layering uses raw reference
          counts. A name shared by two unrelated modules can move a box.
        </p>
      {/if}
      {#if payload.excluded.uncertainEdges > 0}
        <p class="dim">
          {plural(payload.excluded.uncertainEdges, 'cross-module reference')} below confidence {payload
            .excluded.confidenceBelow}
          {payload.excluded.uncertainEdges === 1 ? 'is' : 'are'} excluded from every count on this
          screen — they are name-only guesses.
        </p>
      {/if}
    </div>

    {#if layout.mutual.length > 0}
      <details>
        <summary>
          Mutual dependencies
          <span class="dim">
            · {plural(layout.mutual.length, 'pair')} — the lighter direction, dashed when
            selected
          </span>
        </summary>
        {#each layout.mutual.slice(0, 8) as pair (pair.back.source + pair.back.target)}
          <div class="cyc">
            <b>{pair.back.source}</b> ⇄ {pair.back.target}
            <span class="dim">({pair.back.count} back-references)</span>
          </div>
        {/each}
        {#if layout.mutual.length > 8}
          <div class="cyc dim">+{layout.mutual.length - 8} more</div>
        {/if}
      </details>
    {/if}

    {#if layout.moduleCycles.length > 0}
      <details>
        <summary>
          Dependency cycles
          <span class="dim">
            · {plural(layout.moduleCycles.length, 'loop')} of three or more modules
          </span>
        </summary>
        {#each layout.moduleCycles.slice(0, 6) as cycle, i (i)}
          <div class="cyc">{cycle.join(' → ')} → {cycle[0]}</div>
        {/each}
      </details>
    {/if}

    {#if payload.cycles.total > 0}
      <details>
        <summary>
          Circular imports between files
          <span class="dim">
            · {plural(payload.cycles.total, 'group')}
          </span>
        </summary>
        {#each payload.cycles.items.slice(0, 6) as cycle, i (i)}
          <div class="cyc">
            <span class="dim">{cycle.size} files ·</span>
            {cycle.modules.join(', ')}
          </div>
          {#each cycle.files as file (file)}
            <a class="filerow" href={fileHref(file)}>{file}</a>
          {/each}
          {#if cycle.size > cycle.files.length}
            <div class="cyc dim">+{cycle.size - cycle.files.length} more files in this group</div>
          {/if}
        {/each}
        {#if payload.cycles.truncated}
          <div class="cyc dim">+{payload.cycles.total - payload.cycles.shown} more groups</div>
        {/if}
      </details>
    {/if}
  {:else}
    {#if selectedModule}
      <div class="section micro"><Icon name="folder" size={14} />Files · {selectedModule.fileList?.total ?? files.length}</div>
      {#if files.length > 0}
        {#each files as file (file)}
          <a
            class="filerow"
            class:gen={generatedFiles.has(file)}
            href={fileHref(file)}
            title={generatedFiles.has(file) ? `${file} — tool-generated` : file}
            ><Icon name="file-code-2" size={14} />{file}</a
          >
        {/each}
      {:else}
        <div class="pair dim">no files in the index for this module</div>
      {/if}
    {:else}
      <p class="dim">Click a module on the map to list its files.</p>
    {/if}
  {/if}
  </div>

  <!-- The map is the thing people paste into a README, so the way out sits
       at the foot of the inspector, on every tab. -->
  <div class="foot">
    {#if selectedModule && files.length > 0}
      <a class="btn primary" href={fileHref(files[0] ?? '')}><Icon name="external-link" />Open first file</a>
    {/if}
    <ExportButtons build={buildSvg} filename={exportName} />
  </div>
</aside>

{#snippet linkList(label: string, links: WireMapLink[], side: 'source' | 'target', max: number)}
  <div class="section micro"><Icon name={side === 'target' ? 'arrow-right' : 'corner-down-right'} size={14} />{label} · {links.length}</div>
  {#if links.length > 0}
    {#each links.slice(0, 8) as link (link.source + link.target)}
      <button type="button" class="rank" onclick={() => onSelect(side === 'target' ? link.target : link.source)}>
        <span class="rn mono">{side === 'target' ? link.target : link.source}</span>
        <span class="rc mono">{link.count}</span>
        <span class="bar"><i style:width={`${Math.max(4, Math.round((100 * link.count) / max))}%`}></i></span>
      </button>
    {/each}
    {#if links.length > 8}<div class="pair dim">+{links.length - 8} more</div>{/if}
  {:else}
    <div class="pair dim">nothing</div>
  {/if}
{/snippet}

<style>
  /* §8 D-05 inspector: tabs with a 2px GRAD.brand underline, the module tile
     32, three stat tiles, ranked lists with 3px GRAD.data bars; the actions
     at the foot. */
  .mapside {
    display: flex;
    min-height: 0;
    flex-direction: column;
    color: var(--fg-2);
    font: var(--t-small);
  }

  .tabs {
    display: flex;
    flex: 0 0 auto;
    gap: 18px;
    padding: 0 16px;
    border-bottom: 1px solid var(--line-faint);
  }

  .tabs button {
    position: relative;
    height: 46px;
    color: var(--fg-3);
    font: var(--t-body-500);
  }

  .tabs button:hover {
    color: var(--fg);
  }

  .tabs button.on {
    color: var(--fg);
  }

  .tabs button.on::after {
    position: absolute;
    right: 0;
    bottom: -1px;
    left: 0;
    height: 2px;
    border-radius: 1px;
    background: var(--grad-brand);
    content: '';
  }

  .body {
    min-height: 0;
    flex: 1;
    overflow: auto;
    padding: 14px 16px 16px;
  }

  h2 {
    margin: 0 0 8px;
    color: var(--fg);
    font: var(--t-h2);
  }

  p {
    margin: 0 0 10px;
    line-height: 1.5;
  }

  .dim {
    color: var(--fg-3);
  }

  .mono {
    font-family: var(--mono);
  }

  .mhead {
    display: grid;
    grid-template-columns: 32px minmax(0, 1fr) auto;
    align-items: center;
    gap: 12px;
    margin-bottom: 14px;
  }

  .mt {
    display: flex;
    min-width: 0;
    flex-direction: column;
  }

  .mt b {
    overflow: hidden;
    color: var(--fg);
    font: var(--t-mono-500);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .mt .dim {
    font: var(--t-caption);
  }

  .tiles {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 8px;
    margin-bottom: 14px;
  }

  .tiles .value {
    font-size: 20px;
  }

  .reach b {
    color: var(--fg);
  }

  .section {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 16px 0 8px;
  }

  .rank {
    display: grid;
    width: 100%;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 2px 8px;
    padding: 6px 0;
    color: var(--fg);
    text-align: left;
  }

  .rank:hover .rn {
    color: var(--primary-ink);
  }

  .rn {
    overflow: hidden;
    font: var(--t-mono);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .rc {
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .rank .bar {
    grid-column: 1 / -1;
    height: 3px;
  }

  .field {
    display: grid;
    grid-template-columns: 72px minmax(0, 1fr);
    align-items: center;
    gap: 10px;
    margin-bottom: 8px;
    color: var(--fg-3);
  }

  .field select {
    height: 32px;
    min-width: 0;
    padding: 0 10px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--raised);
    color: var(--fg);
    font: var(--t-mono-sm);
  }

  .toggle {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 6px;
    color: var(--fg);
    cursor: pointer;
  }

  .toggle input {
    position: absolute;
    opacity: 0;
  }

  .switch {
    position: relative;
    width: 30px;
    height: 18px;
    flex: 0 0 auto;
    border-radius: 9px;
    background: var(--raised);
    box-shadow: inset 0 0 0 1px var(--line-strong);
    transition: background-color 120ms;
  }

  .switch::after {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    background: var(--fg-3);
    content: '';
    transition: transform 120ms;
  }

  .toggle input:checked + .switch {
    background: var(--grad-button);
    box-shadow: none;
  }

  .toggle input:checked + .switch::after {
    background: var(--on-primary);
    transform: translateX(12px);
  }

  .toggle input:focus-visible + .switch {
    outline: 2px solid var(--primary);
    outline-offset: 3px;
  }

  .notes p {
    font: var(--t-caption);
  }

  details {
    margin-top: 10px;
    padding: 10px 12px;
    border: 1px solid var(--line-faint);
    border-radius: 10px;
    background: var(--card);
  }

  summary {
    color: var(--fg);
    cursor: pointer;
    font: var(--t-small-500);
  }

  summary .dim {
    font-weight: 400;
  }

  .cyc {
    padding: 4px 0;
    font: var(--t-mono-sm);
  }

  .cyc b {
    color: var(--fg);
  }

  .filerow {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 4px;
    overflow: hidden;
    border-radius: 6px;
    color: var(--fg-2);
    font: var(--t-mono-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .filerow:hover {
    background: var(--raised);
    color: var(--fg);
  }

  .filerow.gen {
    color: var(--fg-4);
  }

  .pair {
    padding: 4px 0;
    font: var(--t-mono-sm);
  }

  .foot {
    display: flex;
    flex: 0 0 auto;
    flex-wrap: wrap;
    gap: 8px;
    padding: 12px 16px;
    border-top: 1px solid var(--line-faint);
  }

  .foot :global(.exp) {
    margin-left: 0;
  }
</style>
