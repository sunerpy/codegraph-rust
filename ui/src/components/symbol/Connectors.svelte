<!--
  The hairlines from a gutter port to its callee row (design spec §3.2).

  One curve per CALL SITE, not per row: a helper called from three lines gets
  three connectors into one row, which is the honest drawing — the row is the
  symbol, the curves are the calls.

  Line style carries the claim. Solid means the resolver matched it; dashed
  `2 3` means it is a name-only guess; dashed `6 3` in a lighter ink means the
  edge was synthesized rather than parsed (dynamic dispatch), so the reader can
  see at a glance which parts of the picture the parser actually saw.
-->
<script lang="ts">
  import { hot } from '../../lib/focus.svelte';
  import type { Connector } from '../../lib/symbol-model';

  interface Props {
    connectors: Connector[];
    width: number;
    height: number;
  }

  let { connectors, width, height }: Props = $props();

  // §7: the hot connector is drawn last, so it crosses over the resting ones.
  let ordered = $derived.by(() => {
    const resting: Array<{ connector: Connector; key: string }> = [];
    const lit: Array<{ connector: Connector; key: string }> = [];
    connectors.forEach((connector, i) => {
      const entry = { connector, key: `${connector.targetId}:${i}` };
      (hot.is(connector.targetId) ? lit : resting).push(entry);
    });
    return [...resting, ...lit];
  });
</script>

<svg
  class="overlay"
  {width}
  {height}
  viewBox={`0 0 ${width} ${height}`}
  aria-hidden="true"
  focusable="false"
>
  {#each ordered as { connector, key } (key)}
    <path
      d={connector.d}
      class:uncertain={connector.uncertain}
      class:heur={connector.heuristic}
      class:origin={connector.origin}
      class:hot={hot.is(connector.targetId)}
    />
  {/each}
</svg>

<style>
  .overlay {
    position: absolute;
    inset: 0;
    z-index: 1;
    overflow: visible;
    pointer-events: none;
  }

  /* §9.1 edges: code port → callee row `cyan-line` 1.5; the hot one `cyan`
     2 with the 80 % glow; a name-only guess `fg-3` dashed 2 3; a synthesized
     edge `fg-2` dashed 6 3. */
  path {
    fill: none;
    stroke: var(--cyan-line);
    stroke-width: 1.5;
  }

  path.uncertain {
    stroke: var(--fg-3);
    stroke-dasharray: 2 3;
  }

  path.heur {
    stroke: var(--fg-2);
    stroke-dasharray: 6 3;
  }

  path.origin {
    stroke: var(--primary);
  }

  path.hot {
    stroke: var(--cyan);
    stroke-width: 2;
    filter: var(--glow-cyan-80-f);
  }
</style>
