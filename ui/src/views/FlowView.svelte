<!--
  The Flow strip (`#/flow`, design spec §3.5): how one symbol reaches another,
  as one card per hop, each opened at the line that makes the next call.

  The path is not computed here and is not computed by the server either — it
  comes from `resolveNamedSymbolFlow`, the search `codegraph_explore` leads its
  answers with. That is deliberate: a viewer that drew a different path from the
  one the MCP tool describes would get the two quoted against each other in a
  review, and one of them would be wrong.

  Svelte Flow draws it, for pan, zoom and fit and nothing else: positions come
  from `buildFlowLayout`, the flow picker is local state, and nothing is
  draggable. Clicking a card opens the Symbol view with the trail set to the
  path so far, so the strip hands the reader off to the view that goes deep.
-->
<script lang="ts">
  import { effectiveTheme, theme } from '../lib/theme-choice.svelte';
  import { SvelteFlow, Controls, type Node, type Edge } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';
  import FlowCard from '../components/flow/FlowCard.svelte';
  import FlowLink from '../components/flow/FlowLink.svelte';
  import FlowEndCap from '../components/flow/FlowEndCap.svelte';
  import ExportButtons from '../components/ExportButtons.svelte';
  import Icon from '../components/Icon.svelte';
  import KindGlyph from '../components/KindGlyph.svelte';
  import { exportFilename, flowSvg } from '../lib/export-svg';
  import { fetchFlow, type WireFlow, type WireFlowPayload } from '../lib/api';
  import { live } from '../lib/live.svelte';
  import { navigate, symbolHref } from '../lib/navigation';
  import { trail, encodeTrail, type TrailHop } from '../lib/trail.svelte';
  import { decodeTrail } from '../lib/trail-codec';
  import { buildFlowLayout, type FlowCardLayout, type FlowLayout } from '../lib/flow-model';
  import { basename } from '../lib/symbol-model';

  interface Props {
    from: string | null;
    to: string | null;
    symbols: string | null;
    /** An encoded trail, when the flow is the reader's own walk. */
    trailParam: string | null;
  }

  let { from, to, symbols, trailParam }: Props = $props();

  let payload = $state<WireFlowPayload | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  let picked = $state<string | null>(null);
  /** True when the picker is on "All paths" — the union is drawn as a DAG. */
  let showAll = $state(false);

  const ALL = 'all-paths';

  /**
   * The strip opens at 1:1, top left — it never fits itself to the window.
   *
   * Fitting an eight-hop flow into a laptop's width lands at about 0.38 zoom,
   * which is a picture of eight grey rectangles: the source inside them is the
   * answer, and source you cannot read is not an answer. So the reader arrives
   * at the first card, full size, and pans. The Controls' fit button is still
   * there for anyone who wants the shape rather than the code.
   */
  const START_VIEWPORT = { x: 0, y: 0, zoom: 1 };
  const nodeTypes = { flow: FlowCard, cap: FlowEndCap };
  const edgeTypes = { flow: FlowLink };

  /** The hops the trail form asks for, as `<dir><id>` — the wire's own spelling. */
  const trailHops = $derived<TrailHop[]>(trailParam ? decodeTrail(trailParam) : []);

  $effect(() => {
    const spec = trailParam
      ? { trail: trailHops.map((h) => `${h.dir === 'start' ? 's' : h.dir === 'up' ? 'u' : 'd'}${h.id}`) }
      : symbols
        ? { symbols }
        : { from: from ?? '', to: to ?? '' };
    if (!trailParam && !symbols && !(from && to)) {
      payload = null;
      loading = false;
      error = null;
      return;
    }
    // Re-run when the index moves: a path is a walk over edges that a sync can
    // add, remove or re-route, and a strip drawn from the previous graph would
    // disagree with `codegraph_explore` about the same question.
    void live.indexTick;
    const controller = new AbortController();
    loading = true;
    error = null;
    const keep = picked;
    fetchFlow(spec, controller.signal)
      .then((next) => {
        payload = next;
        // A refresh keeps the reader's chosen path when it survived the sync.
        picked = next.flows.some((f) => f.id === keep) ? keep : (next.flows[0]?.id ?? null);
        loading = false;
      })
      .catch((err: unknown) => {
        if (controller.signal.aborted) return;
        error = err instanceof Error ? err.message : String(err);
        loading = false;
      });
    return () => controller.abort();
  });

  const flows = $derived<WireFlow[]>(payload?.flows ?? []);
  const shown = $derived<WireFlow[]>(
    showAll ? flows : flows.filter((f) => f.id === picked).slice(0, 1)
  );
  const layout = $derived<FlowLayout | null>(
    shown.length === 0 ? null : buildFlowLayout(showAll ? flows : shown, picked)
  );
  const activeFlow = $derived(flows.find((f) => f.id === picked) ?? flows[0] ?? null);

  const nodes = $derived.by<Node[]>(() => {
    if (layout === null) return [];
    const caps: Node[] = layout.endCaps.map((cap) => ({
      id: cap.id,
      type: 'cap',
      position: { x: cap.x, y: cap.y },
      draggable: false,
      selectable: false,
      connectable: false,
      data: {
        cap,
        dimmed: showAll && picked !== null && !cap.flows.includes(picked),
        onOpen: openNode,
      },
    }));
    // Caps first, so a card that overlaps one paints on top of it.
    return [
      ...caps,
      ...layout.cards.map((card) => ({
        id: card.id,
        type: 'flow',
        position: { x: card.x, y: card.y },
        draggable: false,
        selectable: false,
        connectable: false,
        data: {
          // The accent border marks the picked path, and only means something
          // when there is more than one on screen. A single flow whose every
          // card is accented has said nothing.
          card,
          current: showAll && card.step >= 0,
          dimmed: showAll && card.step < 0,
          onOpen: openCard,
          onFollow: followCard,
        },
      })),
    ];
  });

  const edges = $derived.by<Edge[]>(() => {
    if (layout === null) return [];
    return layout.links.map((link) => ({
      id: link.id,
      source: link.source,
      target: link.target,
      sourceHandle: 'out',
      targetHandle: 'in',
      type: 'flow',
      selectable: false,
      deletable: false,
      data: { link, dimmed: showAll && picked !== null && !link.flows.includes(picked) },
    }));
  });

  /**
   * Open a card in the Symbol view with the trail set to the path so far.
   *
   * The prefix, not the whole flow: the reader is standing at that hop, and a
   * trail that ran on past them would claim a walk they had not taken.
   */
  function openCard(card: FlowCardLayout): void {
    const hops = activeFlow?.hops ?? [];
    const at = hops.findIndex((hop) => hop.node.id === card.id);
    const prefix = at >= 0 ? hops.slice(0, at + 1) : [];
    trail.clear();
    prefix.forEach((hop, index) =>
      trail.push({
        id: hop.node.id,
        name: hop.node.name,
        kind: hop.node.kind,
        dir: index === 0 ? 'start' : hop.edge?.upward ? 'up' : 'down',
      })
    );
    if (prefix.length === 0) {
      trail.push({ id: card.id, name: card.hop.node.name, kind: card.hop.node.kind, dir: 'start' });
    }
    navigate(
      symbolHref(card.id, {
        trail: encodeTrail(trail.hops),
        ...(card.hop.callRef ? { line: card.hop.callRef.line } : {}),
      })
    );
  }

  /**
   * A row on the end cap: a candidate runtime target, or a continuation the
   * search refused to follow.
   *
   * It opens as a fresh start rather than as another hop, because neither is a
   * call the graph recorded — pushing one onto the trail would draw a step
   * nobody took. That is the whole reason the cap exists.
   */
  function openNode(nodeId: string): void {
    trail.clear();
    navigate(symbolHref(nodeId));
  }

  /** The accent link inside a card: step to the symbol it names. */
  function followCard(card: FlowCardLayout): void {
    const target = card.hop.callRef?.targetId;
    if (!target) return;
    const next = layout?.cards.find((c) => c.id === target);
    if (next) openCard(next);
  }

  function note(p: WireFlowPayload): string {
    if (p.query.kind === 'trail') {
      return 'Your trail, read as a flow: each card is opened at the line that carried you to the next one.';
    }
    if (p.flows.some((f) => f.partial)) {
      return 'No static path connects them. The card is where the looking stopped — a call whose target is chosen at runtime — and the cap names the form, the key and who could be on the other side.';
    }
    if (p.query.kind === 'directed') {
      return 'Every card is a call the graph recorded. A dashed link is a hop no one can see in the source — a callback, an interface, a re-render — and it names where it was wired.';
    }
    return 'The longest call path among the symbols you named, the same one codegraph_explore leads with.';
  }

  /**
   * The strip as it stands, for a PR comment or a README.
   *
   * Built from `layout` — the same object the canvas is drawing — so the image
   * cannot say something the screen does not. The caption names the path,
   * because an image pasted into a review has lost the header that did.
   */
  const exportLabel = $derived(
    showAll && flows.length > 1
      ? `all ${flows.length} paths`
      : (activeFlow?.label ?? 'flow')
  );

  /**
   * The "Every hop" table (§8 D-04): one row per call on the path — who calls
   * whom, under what condition (phase 2 ports the conditions; until then the
   * column reads "—"), at which line, and how sure the graph is.
   */
  const hopRows = $derived.by(() => {
    const hops = activeFlow?.hops ?? [];
    const rows: Array<{
      step: number;
      from: (typeof hops)[number];
      to: (typeof hops)[number];
      when: string | null;
      site: string;
      line: number | null;
      confidence: number | null;
      resolvedBy: string | null;
      label: string;
    }> = [];
    for (let i = 1; i < hops.length; i++) {
      const fromHop = hops[i - 1];
      const toHop = hops[i];
      if (!fromHop || !toHop) continue;
      const edge = toHop.edge;
      // An upward hop is read callee → caller: the call site is in the hop
      // it arrives at, not the one it leaves.
      const caller = edge?.upward ? toHop : fromHop;
      const line = edge?.line ?? toHop.callRef?.line ?? fromHop.callRef?.line ?? null;
      rows.push({
        step: i,
        from: fromHop,
        to: toHop,
        when: edge?.when ?? null,
        site: `${basename(caller.node.file)}${line !== null ? `:${line}` : ''}`,
        line,
        confidence: edge?.confidence ?? null,
        resolvedBy: edge?.resolvedBy ?? edge?.synthesizedBy ?? null,
        label: edge?.label ?? 'calls',
      });
    }
    return rows;
  });

  const callCount = $derived(hopRows.length);

  function openSite(row: (typeof hopRows)[number]): void {
    const caller = row.to.edge?.upward ? row.to : row.from;
    trail.clear();
    navigate(symbolHref(caller.node.id, row.line !== null ? { line: row.line } : {}));
  }

  function buildSvg(scale: number): string {
    if (layout === null) throw new Error('There is no strip to export yet.');
    const hops = activeFlow?.hops.length ?? 0;
    return flowSvg(layout, {
      scale,
      // The theme on screen, System resolved (§3.6).
      theme: effectiveTheme(theme.choice),
      activeFlowId: picked,
      showAll,
      caption: showAll ? exportLabel : `${exportLabel}${hops > 1 ? ` · ${hops} hops` : ''}`,
    });
  }
</script>

<div class="flowview">
  <header class="fhead island">
    <span class="ftile"><Icon name="workflow" /></span>
    <div class="ftitle">
      <h1>Flow</h1>
      <span class="sub">how one symbol reaches another, opened at each call</span>
    </div>
    {#if flows.length > 0}
      <label class="route">
        <span class="sr">Which path to draw</span>
        <select
          aria-label="Which path to draw"
          value={showAll ? ALL : (picked ?? '')}
          onchange={(event) => {
            const value = (event.currentTarget as HTMLSelectElement).value;
            showAll = value === ALL;
            if (!showAll) picked = value;
          }}
        >
          {#each flows as flow (flow.id)}
            <option value={flow.id}
              >{flow.label}{flow.hops.length > 1 ? ` · ${flow.hops.length} hops` : ''}</option
            >
          {/each}
          {#if flows.length > 1}
            <option value={ALL}>All {flows.length} paths</option>
          {/if}
        </select>
        <span class="chev"><Icon name="chevron-down" size={14} /></span>
      </label>
    {/if}
    {#if activeFlow && layout !== null}
      <span class="pill">{activeFlow.hops.length} symbol{activeFlow.hops.length === 1 ? '' : 's'}</span>
      {#if callCount > 0}<span class="pill cyan"><Icon name="arrow-right" size={14} />{callCount} call{callCount === 1 ? '' : 's'}</span>{/if}
    {/if}
    <span class="sp"></span>
    {#if layout !== null}
      <ExportButtons build={buildSvg} filename={exportFilename('flow', exportLabel)} />
    {/if}
  </header>

  <div class="fstage island">
    {#if error !== null}
      <div class="state">
        <h2>The flow could not be built</h2>
        <p>{error}</p>
      </div>
    {:else if loading && payload === null}
      <div class="state"><span class="pill"><Icon name="refresh-cw" size={14} />Following the calls…</span></div>
    {:else if payload === null}
      <div class="state">
        <h2>Nothing to follow yet</h2>
        <p>
          Ask for a path in the search box — “how does execute reach getFile”, or
          <span class="mono">execute -&gt; getFile</span> — or walk a trail and read it as a flow.
        </p>
      </div>
    {:else if layout === null}
      <div class="state">
        <h2>No path between them</h2>
        <p>{payload.reason}</p>
        {#if payload.query.from && payload.query.to}
          <p class="dim">
            Asked: <span class="mono">{payload.query.from}</span> to
            <span class="mono">{payload.query.to}</span>.
          </p>
        {/if}
      </div>
    {:else}
      <SvelteFlow
        {nodes}
        {edges}
        {nodeTypes}
        {edgeTypes}
        initialViewport={START_VIEWPORT}
        fitViewOptions={{ padding: 0.1, maxZoom: 1, minZoom: 0.2 }}
        minZoom={0.2}
        maxZoom={1.4}
        nodesDraggable={false}
        nodesConnectable={false}
        elementsSelectable={false}
        panOnDrag
        proOptions={{ hideAttribution: true }}
      >
        <Controls position="bottom-right" showLock={false} />
      </SvelteFlow>
      <p class="legend">{note(payload)}</p>
    {/if}
  </div>

  {#if payload && layout !== null && (hopRows.length > 0 || payload.reason !== null || payload.ambiguous.length > 0 || payload.unresolved.length > 0)}
    <section class="hops island" aria-label="Every hop">
      <div class="hh">
        <span class="title">Every hop</span>
        <span class="dim">the calls on this path, in order · conditions arrive with the branch-guard port</span>
      </div>
      {#if payload.reason !== null || payload.ambiguous.length > 0 || payload.unresolved.length > 0}
        <div class="fnote">
          {#if payload.reason !== null}
            <p class="callout"><Icon name="info" />{payload.reason}</p>
          {/if}
          {#each payload.ambiguous as amb (amb.token)}
            <p class="callout">
              <Icon name="circle-alert" /><span><span class="mono">{amb.token}</span> names {amb.others.length + 1} definitions.
              {#if amb.chosen}
                This path runs through the one in
                <span class="mono">{basename(amb.chosen.file)}:{amb.chosen.line}</span>.
              {:else}
                None of them are on this path.
              {/if}</span>
            </p>
          {/each}
          {#each payload.unresolved as token (token)}
            <p class="callout"><Icon name="circle-dashed" /><span><span class="mono">{token}</span> names nothing in this index.</span></p>
          {/each}
        </div>
      {/if}
      {#if hopRows.length > 0}
        <div class="table" role="table">
          <div class="tr th" role="row">
            <span role="columnheader">#</span>
            <span role="columnheader">Call</span>
            <span class="when" role="columnheader">Runs when</span>
            <span role="columnheader">Site</span>
            <span class="conf" role="columnheader">Confidence</span>
          </div>
          {#each hopRows as row (row.step)}
            <button type="button" class="tr" role="row" onclick={() => openSite(row)} title={`Open ${row.site}`}>
              <span class="badge" role="cell">{row.step}</span>
              <span class="call" role="cell">
                <KindGlyph kind={row.from.node.kind} />
                <span class="nm">{row.from.node.name}</span>
                <span class="arrow"><Icon name={row.to.edge?.upward ? 'corner-left-up' : 'arrow-right'} size={14} /></span>
                <KindGlyph kind={row.to.node.kind} />
                <span class="nm">{row.to.node.name}</span>
              </span>
              <span class="when" role="cell">
                {#if row.when}<span class="pill amber mono"><Icon name="route" size={14} />when {row.when}</span>{:else}<span class="dim">—</span>{/if}
              </span>
              <span class="site mono" role="cell">{row.site}</span>
              <span class="conf" role="cell">
                {#if row.confidence !== null}
                  <span class="bar"><i style:width={`${Math.round(row.confidence * 100)}%`}></i></span>
                  <span class="mono">{row.confidence.toFixed(2)}</span>
                {:else}<span class="dim">—</span>{/if}
                {#if row.resolvedBy}<span class="pill mono">{row.resolvedBy}</span>{/if}
              </span>
            </button>
          {/each}
        </div>
      {/if}
    </section>
  {/if}
</div>

<style>
  /* §8 D-04: three islands stacked — the header 64, the canvas, Every hop. */
  .flowview {
    display: grid;
    height: 100%;
    min-height: 0;
    gap: var(--gap);
    grid-template-rows: auto minmax(0, 1fr) auto;
  }

  .fhead {
    display: flex;
    min-height: 64px;
    flex-wrap: wrap;
    align-items: center;
    gap: 10px 14px;
    padding: 12px 16px;
    overflow: visible;
  }

  .ftile {
    display: inline-flex;
    width: 32px;
    height: 32px;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    border-radius: 10px;
    background: var(--primary-soft);
    color: var(--primary-ink);
  }

  .ftitle {
    display: flex;
    min-width: 0;
    flex-direction: column;
  }

  .ftitle h1 {
    margin: 0;
    color: var(--fg);
    font: var(--t-h1);
  }

  .sub {
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .route {
    position: relative;
    display: inline-flex;
    align-items: center;
  }

  .route select {
    height: 36px;
    max-width: 440px;
    padding: 0 34px 0 12px;
    appearance: none;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--raised);
    color: var(--fg);
    font: var(--t-mono);
  }

  .route select:focus-visible {
    border-color: var(--primary-line);
  }

  .route .chev {
    position: absolute;
    right: 12px;
    display: inline-flex;
    color: var(--fg-3);
    pointer-events: none;
  }

  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }

  .sp {
    flex: 1;
  }

  /* The canvas: dots and ambient glows (§4), the cards floating above. */
  .fstage {
    position: relative;
    background: var(--canvas);
  }

  .fstage :global(.svelte-flow) {
    background: transparent;
  }
  .fstage :global(.svelte-flow__handle) {
    width: 1px;
    height: 1px;
    min-width: 0;
    min-height: 0;
    border: 0;
    opacity: 0;
    pointer-events: none;
  }
  .fstage :global(.svelte-flow__node) {
    cursor: default;
  }
  .fstage :global(.svelte-flow__controls) {
    overflow: hidden;
    border: 1px solid var(--line);
    border-radius: 10px;
    box-shadow: var(--sh-pop);
  }
  .fstage :global(.svelte-flow__controls-button) {
    border: 0;
    border-bottom: 1px solid var(--line);
    background: var(--overlay);
    box-shadow: none;
    fill: var(--fg-2);
  }
  .fstage :global(.svelte-flow__controls-button:hover) {
    background: var(--raised);
    fill: var(--fg);
  }

  /* The note about what a card and a dashed link mean, as a legend. */
  .legend {
    position: absolute;
    bottom: 16px;
    left: 16px;
    z-index: 5;
    max-width: min(560px, calc(100% - 120px));
    margin: 0;
    padding: 9px 12px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--overlay);
    box-shadow: var(--sh-pop);
    color: var(--fg-2);
    font: var(--t-caption);
    line-height: 1.45;
  }

  .state {
    max-width: 56ch;
    padding: 32px;
  }
  .state h2 {
    margin: 0 0 8px;
    color: var(--fg);
    font: var(--t-h2);
  }
  .state p {
    margin: 0 0 8px;
    color: var(--fg-2);
    font: var(--t-body);
  }
  .dim {
    color: var(--fg-3);
  }
  .mono {
    font-family: var(--mono);
  }

  /* Every hop: columns # | call | runs when | site | confidence; rows 40. */
  .hops {
    max-height: 320px;
    overflow: auto;
    padding: 0 16px 12px;
  }

  .hh {
    position: sticky;
    top: 0;
    z-index: 1;
    display: flex;
    align-items: baseline;
    gap: 12px;
    padding: 16px 4px 10px;
    background: var(--panel);
    font: var(--t-caption);
  }

  .hh .title {
    color: var(--fg);
    font: var(--t-label);
  }

  .fnote {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 10px;
  }

  .fnote p {
    margin: 0;
  }

  .tr {
    display: grid;
    width: 100%;
    min-height: 40px;
    align-items: center;
    gap: 14px;
    padding: 0 10px;
    border: 1px solid transparent;
    border-radius: 10px;
    color: var(--fg);
    grid-template-columns: 28px minmax(260px, 1.4fr) minmax(160px, 1.2fr) 220px 240px;
    text-align: left;
  }

  button.tr:hover {
    border-color: var(--primary-line);
    background: color-mix(in srgb, var(--primary-soft) 85%, transparent);
  }

  .th {
    min-height: 30px;
    border-bottom: 1px solid var(--line-faint);
    border-radius: 0;
    color: var(--fg-3);
    font: var(--t-micro);
    letter-spacing: 0.6px;
    text-transform: uppercase;
  }

  .badge {
    display: inline-flex;
    width: 22px;
    height: 22px;
    align-items: center;
    justify-content: center;
    border-radius: 11px;
    background: var(--raised);
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .call {
    display: flex;
    min-width: 0;
    align-items: center;
    gap: 8px;
  }

  .call .nm {
    overflow: hidden;
    font: var(--t-mono);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .arrow {
    display: inline-flex;
    color: var(--cyan);
  }

  .when {
    min-width: 0;
    overflow: hidden;
  }

  .site {
    overflow: hidden;
    color: var(--fg-2);
    font: var(--t-mono-sm);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .conf {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .conf .bar {
    width: 56px;
  }

  .conf .mono {
    color: var(--fg);
    font: var(--t-mono-sm);
  }

  @media (max-width: 1023px) {
    .tr {
      grid-template-columns: 28px minmax(200px, 1fr) 120px 200px;
    }

    .when {
      display: none;
    }
  }

  @media (max-width: 599px) {
    .fhead .pill,
    .fhead .ftitle .sub,
    .legend {
      display: none;
    }

    .route select {
      max-width: calc(100vw - 120px);
    }

    .hops {
      max-height: 40vh;
    }

    .tr {
      grid-template-columns: 24px minmax(0, 1fr) 90px;
    }

    .conf {
      display: none;
    }
  }
</style>
