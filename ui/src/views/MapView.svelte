<!--
  The Map (`#/map`, design spec §3.6): the repository at module granularity,
  layered so dependencies point down.

  Svelte Flow draws it — custom node, custom edge, hidden handles as ports —
  but none of Svelte Flow's editing machinery is in play: positions come from
  `buildMapLayout`, selection is a local string, and nothing here is draggable.
  What the library provides is pan, zoom and fit; what it must not provide is
  a layout, because a map you cannot recognise between two visits is not a map.

  Root and depth ride in the hash, so a link to "src/vs at depth 2" reopens the
  same picture. Selection does not: it is a question you ask of the map, not a
  place you were.
-->
<script lang="ts">
  import { effectiveTheme, theme } from '../lib/theme-choice.svelte';
  import { SvelteFlow, Controls, ViewportPortal, type Node, type Edge, MiniMap } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';
  import ModuleNode from '../components/map/ModuleNode.svelte';
  import ModuleEdge from '../components/map/ModuleEdge.svelte';
  import MapSidePanel from '../components/map/MapSidePanel.svelte';
  import MapKey from '../components/map/MapKey.svelte';
  import { exportFilename, mapSvg } from '../lib/export-svg';
  import { fetchMap, type WireMapPayload } from '../lib/api';
  import { live } from '../lib/live.svelte';
  import { mapHref, navigate } from '../lib/navigation';
  import {
    buildMapLayout,
    isEdgeVisible,
    type MapEdgeLayout,
    type MapLayout,
  } from '../lib/map-model';

  interface Props {
    root: string | null;
    /** `null` = nobody has chosen; the answer picks a grouping for this repo. */
    depth: number | null;
    tests: boolean;
  }

  let { root, depth, tests }: Props = $props();

  let payload = $state<WireMapPayload | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let selected = $state<string | null>(null);
  let hovered = $state<{ edge: MapEdgeLayout; x: number; y: number } | null>(null);
  let stage = $state<HTMLDivElement | null>(null);

  /**
   * Fit, but never past readable.
   *
   * The prototype refused to scale a label below ~0.9 and scrolled instead;
   * a pannable canvas can be more generous, but not unboundedly so — a
   * seventy-module repository fitted to a laptop screen is a picture of grey
   * hair, not a map. Below this floor the view opens part-way and the reader
   * pans, which is the honest trade.
   */
  const FIT = { fitViewOptions: { padding: 0.12, maxZoom: 1, minZoom: 0.45 } };

  // The key rests as D-05's one-line legend and opens to the full key on
  // request; the choice survives a reload but is per browser — a preference,
  // not a fact about the project. Same storage shape as the Screens and Steps
  // keys. (Upstream opened the full key by default, over the map.)
  const LEGEND_KEY = 'codegraph-ui:map-legend';
  let legendOpen = $state(readLegendOpen());
  function readLegendOpen(): boolean {
    try {
      return localStorage.getItem(LEGEND_KEY) === 'open';
    } catch {
      return false;
    }
  }
  $effect(() => {
    try {
      localStorage.setItem(LEGEND_KEY, legendOpen ? 'open' : 'closed');
    } catch {
      // Storage refused (private mode): the key simply reopens next time.
    }
  });

  const nodeTypes = { module: ModuleNode };
  const edgeTypes = { module: ModuleEdge };

  // One fetch per (root, depth). The tests toggle is deliberately NOT in here:
  // the payload already carries every module, so including them is a filter,
  // not a question for the server.
  $effect(() => {
    const wantRoot = root;
    const wantDepth = depth;
    // Read so the effect re-runs when the index moves: the map IS the graph,
    // and the layering changes with it. The canvas stays on screen while the
    // new aggregation lands (the server answers a cached one in milliseconds
    // when nothing actually changed).
    void live.indexTick;
    const controller = new AbortController();
    loading = true;
    error = null;
    fetchMap({ root: wantRoot, depth: wantDepth ?? undefined }, controller.signal)
      .then((next) => {
        payload = next;
        loading = false;
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) return;
        error = err instanceof Error ? err.message : String(err);
        loading = false;
      });
    return () => controller.abort();
  });

  const layout = $derived<MapLayout | null>(
    payload === null ? null : buildMapLayout(payload, { includeTests: tests })
  );

  /** Modules one hop from the selection — everything else is dimmed, not hidden. */
  const neighbours = $derived.by(() => {
    if (layout === null || selected === null) return null;
    const set = new Set<string>([selected]);
    for (const edge of layout.edges) {
      if (edge.source === selected) set.add(edge.target);
      if (edge.target === selected) set.add(edge.source);
    }
    return set;
  });

  const nodes = $derived.by<Node[]>(() => {
    if (layout === null) return [];
    return layout.nodes.map((node) => ({
      id: node.id,
      type: 'module',
      position: { x: node.x, y: node.y },
      draggable: false,
      selectable: false,
      connectable: false,
      data: {
        layout: node,
        selected: selected === node.id,
        dimmed: neighbours !== null && !neighbours.has(node.id),
        onSelect: (id: string) => {
          selected = selected === id ? null : id;
          hovered = null;
        },
      },
    }));
  });

  const edges = $derived.by<Edge[]>(() => {
    if (layout === null) return [];
    return layout.edges
      .filter((edge) => isEdgeVisible(edge, selected))
      .map((edge) => ({
        id: edge.id,
        source: edge.source,
        target: edge.target,
        sourceHandle: edge.sourceHandle,
        targetHandle: edge.targetHandle,
        type: 'module',
        selectable: false,
        deletable: false,
        data: {
          edge,
          hot: hovered?.edge.id === edge.id || (selected !== null && !edge.back),
          dimmed: false,
          onHover: onEdgeHover,
        },
      }));
  });

  const selectedFiles = $derived(
    selected === null || payload === null
      ? []
      : (payload.modules.find((m) => m.id === selected)?.fileList.items ?? [])
  );

  function onEdgeHover(edge: MapEdgeLayout | null, event: MouseEvent | null): void {
    if (edge === null || event === null || stage === null) {
      hovered = null;
      return;
    }
    const box = stage.getBoundingClientRect();
    hovered = {
      edge,
      // Clamped so the card never runs off the right-hand side of the canvas.
      x: Math.min(event.clientX - box.left + 14, box.width - 330),
      y: event.clientY - box.top + 14,
    };
  }

  function setRoot(next: string): void {
    selected = null;
    // Deliberately dropping the depth: how finely to cut `ios` is a different
    // question from how finely to cut the whole project, and carrying the old
    // answer over is how a reader lands on a one-box map.
    navigate(mapHref({ root: next, tests }));
  }

  /** `null` hands the grouping back to the answering side. */
  function setDepth(next: number | null): void {
    selected = null;
    navigate(mapHref({ root, depth: next, tests }));
  }

  /**
   * The map as it stands, for a README.
   *
   * Serialised from `layout` — the object the canvas is drawing — so the file
   * carries the same layering, the same hidden thin links and the same
   * selection the reader is looking at. SVG rather than PNG is the point here:
   * a forty-module map is a wide, mostly-empty drawing that scales, and GitHub
   * renders SVG in a README.
   */
  function buildSvg(scale: number): string {
    if (layout === null) throw new Error('There is no map to export yet.');
    const root = payload?.root ?? '';
    return mapSvg(layout, {
      scale,
      // The theme on screen, System resolved (§3.6).
      theme: effectiveTheme(theme.choice),
      selected,
      caption: `${root || 'the project'} · ${layout.nodes.length} modules${selected ? ` · ${selected} selected` : ''}`,
    });
  }

  function setTests(next: boolean): void {
    selected = null;
    navigate(mapHref({ root, depth, tests: next }));
  }

</script>

<div class="mapview">
  <div class="mapstage island" bind:this={stage}>
    {#if error !== null}
      <div class="state">
        <h2>The map could not be built</h2>
        <p>{error}</p>
      </div>
    {:else if loading && payload === null}
      <div class="state"><span class="pill">Aggregating the graph by module…</span></div>
    {:else if layout !== null && layout.nodes.length === 0}
      <div class="state">
        <h2>Nothing to draw here</h2>
        <p>
          No indexed files sit under this root{tests
            ? ''
            : ', or every module under it is test code'}. Pick another root on the right.
        </p>
      </div>
    {:else if layout !== null}
      <SvelteFlow
        {nodes}
        {edges}
        {nodeTypes}
        {edgeTypes}
        fitView
        {...FIT}
        minZoom={0.2}
        maxZoom={1.6}
        nodesDraggable={false}
        nodesConnectable={false}
        elementsSelectable={false}
        panOnDrag
        proOptions={{ hideAttribution: true }}
        onpaneclick={() => {
          selected = null;
          hovered = null;
        }}
      >
        <!-- The layer rules ride INSIDE the viewport, so they pan and zoom
             with the boxes they explain. A layer line drawn on the frame
             would sit next to the wrong row the moment anyone scrolled. -->
        <ViewportPortal target="back">
          {#each layout.layers as row (row.index)}
            <div
              class="layerline"
              style={`transform:translate(0px,${row.y}px);width:${layout.width}px`}
            ></div>
            {#if row.label !== null}
              <!-- Above the top row, below the bottom one: both sit in the
                   clear band outside the drawing rather than under the edge
                   bundles, which is where a label stops being readable. -->
              <div
                class="layerlbl"
                style={`transform:translate(8px,${row.index === 0 ? row.y + 40 : row.y - 36}px)`}
              >
                {row.label}
              </div>
            {/if}
          {/each}
        </ViewportPortal>
        <!-- §7: the floating toolbar at the top right, the minimap at the
             bottom right, the key at the bottom left. -->
        <Controls position="top-right" orientation="horizontal" showLock={false} />
        <MiniMap position="bottom-right" width={176} height={116} pannable zoomable />
      </SvelteFlow>

      <!-- The key, on the picture it explains. -->
      <MapKey
        minWeight={layout.minWeight}
        thinCount={layout.edges.filter((e) => e.thin && !e.back).length}
        declaredBasis={layout.basis.kind === 'declared'}
        open={legendOpen}
        onToggle={(next) => (legendOpen = next)}
      />

      {#if hovered !== null}
        <div class="tip" style={`left:${hovered.x}px;top:${hovered.y}px`}>
          <div class="mono"><b>{hovered.edge.source}</b> → {hovered.edge.target}</div>
          <div class="row2">
            <span>{hovered.edge.link.count} edges</span>
            <span
              >{hovered.edge.link.byKind.map((k) => `${k.kind} ${k.count}`).join(' · ')}</span
            >
          </div>
          {#if hovered.edge.link.declared !== hovered.edge.link.count}
            <div class="row2 dim">
              <span>{hovered.edge.link.declared} through an import or a declared type</span>
            </div>
          {/if}
          {#each hovered.edge.link.topPairs as pair (pair.from + pair.to)}
            <div class="row2 mono">
              <span>{pair.from} → {pair.to}</span><span>{pair.count}</span>
            </div>
          {/each}
        </div>
      {/if}
    {/if}
  </div>

  {#if payload !== null && layout !== null}
    <MapSidePanel
      {payload}
      {layout}
      {selected}
      includeTests={tests}
      files={selectedFiles}
      buildSvg={buildSvg}
      exportName={exportFilename('map', payload.root ?? '')}
      onToggleTests={setTests}
      onSelectRoot={setRoot}
      chosenDepth={depth}
      onSelectDepth={setDepth}
      onSelect={(id) => (selected = id)}
    />
  {/if}
</div>

<style>
  /* §8 D-05: the canvas island (dots and ambient glows) | the inspector 320. */
  .mapview {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 320px;
    gap: var(--gap);
    height: 100%;
    min-height: 0;
  }
  .mapstage {
    position: relative;
    background: var(--canvas);
  }
  .mapstage :global(.svelte-flow) {
    background: transparent;
  }
  .mapstage :global(.svelte-flow__handle) {
    opacity: 0;
    width: 1px;
    height: 1px;
    min-width: 0;
    min-height: 0;
    border: 0;
    pointer-events: none;
  }
  .mapstage :global(.svelte-flow__controls) {
    overflow: hidden;
    border: 1px solid var(--line);
    border-radius: 10px;
    box-shadow: var(--sh-pop);
  }
  .mapstage :global(.svelte-flow__controls-button) {
    border: 0;
    border-bottom: 1px solid var(--line);
    background: var(--overlay);
    box-shadow: none;
    fill: var(--fg-2);
  }
  .mapstage :global(.svelte-flow__controls-button:hover) {
    background: var(--raised);
    fill: var(--fg);
  }
  .mapstage :global(.svelte-flow__node) {
    cursor: default;
  }
  .mapstage :global(.svelte-flow__controls.horizontal) {
    margin: 14px 16px;
  }
  .mapstage :global(.svelte-flow__controls.horizontal .svelte-flow__controls-button) {
    border-right: 1px solid var(--line);
    border-bottom: 0;
  }
  .mapstage :global(.svelte-flow__minimap) {
    overflow: hidden;
    margin: 16px;
    border: 1px solid var(--line);
    border-radius: 10px;
    box-shadow: var(--sh-pop);
  }
  .mapstage :global(.svelte-flow__minimap-node) {
    fill: var(--line-strong);
  }

  .layerline {
    position: absolute;
    top: 0;
    left: 0;
    height: 1px;
    background: var(--line-faint);
    pointer-events: none;
  }
  /* §8 D-05 bands: `ENTRY` and `FOUNDATIONS · DEPEND ON NOTHING BELOW` in
     `fg-4` micro. */
  .layerlbl {
    position: absolute;
    top: 0;
    left: 0;
    color: var(--fg-4);
    font: var(--t-micro);
    letter-spacing: 0.6px;
    text-transform: uppercase;
    white-space: nowrap;
    pointer-events: none;
  }

  .state {
    max-width: 52ch;
    padding: 32px;
  }
  .state h2 {
    margin: 0 0 8px;
    color: var(--fg);
    font: var(--t-h2);
  }
  .state p {
    margin: 0;
    color: var(--fg-2);
    font: var(--t-body);
  }
  .dim {
    color: var(--fg-3);
  }

  /* The link tooltip: `overlay` + `line-strong` + SH.pop (§7). */
  .tip {
    position: absolute;
    z-index: 6;
    max-width: 340px;
    padding: 10px 12px;
    border: 1px solid var(--line-strong);
    border-radius: 10px;
    background: var(--overlay);
    box-shadow: var(--sh-pop);
    color: var(--fg-2);
    font: var(--t-small);
    pointer-events: none;
  }
  .tip .mono {
    margin-bottom: 6px;
    color: var(--fg-2);
    font: var(--t-mono);
  }
  .tip .mono b {
    color: var(--fg);
    font-weight: 600;
  }
  .tip .row2 {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    padding: 1px 0;
  }
  .tip .row2.mono {
    margin: 0;
    font: var(--t-mono-sm);
  }
  .tip .row2.dim {
    color: var(--fg-3);
  }

  @media (max-width: 1023px) {
    .mapview {
      grid-template-columns: minmax(0, 1fr) 260px;
    }
  }

  @media (max-width: 599px) {
    .mapview {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: minmax(0, 1fr) auto;
    }
  }
</style>
