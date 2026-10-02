<script lang="ts">
  import KindGlyph from '../KindGlyph.svelte';
  /**
   * One module box on the Map (design spec §3.6): a 40px rectangle carrying
   * the module's path and what is inside it.
   *
   * The handles are the point of the component. Svelte Flow routes an edge
   * between two handles, so giving each box one hidden handle per link — laid
   * out along its top and bottom edges at `(i+1)/(n+1)` — is what makes a
   * bundle of eight dependencies fan across the box instead of converging on a
   * single corner. They are invisible and non-connectable: this canvas is a
   * drawing, never an editor.
   */
  import { Handle, Position, type NodeProps } from '@xyflow/svelte';
  import { moduleMetaLabel, type MapNodeLayout } from '../../lib/map-model';

  let { data }: NodeProps = $props();

  const node = $derived(
    data as unknown as {
      layout: MapNodeLayout;
      selected: boolean;
      dimmed: boolean;
      onSelect: (id: string) => void;
    }
  );
  const layout = $derived(node.layout);
  const module = $derived(layout.module);

  function portStyle(index: number, total: number): string {
    return `left:${((index + 1) / (total + 1)) * 100}%`;
  }
</script>

{#each layout.targetHandles as handle, i (handle)}
  <Handle
    type="target"
    id={`t:${handle}`}
    position={Position.Top}
    style={portStyle(i, layout.targetHandles.length)}
    isConnectable={false}
  />
{/each}

<button
  class="mnode"
  class:sel={node.selected}
  class:dimmed={node.dimmed}
  class:test={module.test}
  class:gen={layout.generated}
  style={`width:${layout.width}px;height:${layout.height}px`}
  onclick={() => node.onSelect(layout.id)}
  aria-pressed={node.selected}
  title={`${module.id} — ${module.symbols} symbols in ${module.files} file${
    module.files === 1 ? '' : 's'
  }${
    (module.dependents?.files ?? 0) > 0
      ? `. ${module.dependents.files} file${module.dependents.files === 1 ? '' : 's'} outside it, across ${module.dependents.modules} module${module.dependents.modules === 1 ? '' : 's'}, reference into it.`
      : ''
  }${layout.island ? '. Nothing in the index depends on it.' : ''}${
    layout.generated ? '. Every file in it is tool-generated.' : ''
  }`}
>
  <span class="tile"><KindGlyph kind="module" size={22} /></span>
  <span class="name">{module.id}</span>
  <!-- The same string nodeWidth() sized the box for; they must not drift. -->
  <span class="count" class:island={layout.island}
    >{moduleMetaLabel(module, layout.island)}</span
  >
  <!-- How much leans on this box, as a share of the heaviest one drawn. Inside
       the border rather than on it, so it reads as a level in the box and not
       as a second, thicker edge. -->
  {#if layout.weight > 0}
    <span class="weight" style={`width:${(layout.weight * 100).toFixed(1)}%`}></span>
  {/if}
</button>

{#each layout.sourceHandles as handle, i (handle)}
  <Handle
    type="source"
    id={`s:${handle}`}
    position={Position.Bottom}
    style={portStyle(i, layout.sourceHandles.length)}
    isConnectable={false}
  />
{/each}

<style>
  /* §7 map node: 52 high, r 12, `card` + `line` + SH.card; tile 22 at 12,15,
     the label `mono-500` at 44,8, the meta `caption` at 44,27, and a 2px
     GRAD.data weight bar along the bottom. Selected: `primary-soft`, a 1.5px
     GRAD.brand stroke and the 45 % glow. Dimmed 45 %. */
  .mnode {
    position: relative;
    display: flex;
    flex-direction: column;
    justify-content: center;
    gap: 2px;
    box-sizing: border-box;
    padding: 0 18px 0 44px;
    overflow: hidden;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--card);
    box-shadow: var(--sh-card);
    color: var(--fg);
    cursor: pointer;
    font: inherit;
    text-align: left;
    transition:
      border-color 120ms,
      background-color 120ms,
      opacity 120ms;
  }
  .mnode:hover {
    border-color: var(--line-strong);
  }
  .mnode.sel {
    border: 1.5px solid transparent;
    background:
      linear-gradient(var(--primary-soft), var(--primary-soft)) padding-box,
      var(--grad-brand) border-box;
    box-shadow: var(--glow-45);
  }
  .mnode.sel .name {
    color: var(--primary-ink);
  }
  .mnode.dimmed {
    opacity: 0.45;
  }
  .tile {
    position: absolute;
    top: 14px;
    left: 11px;
    display: inline-flex;
  }
  /* Nothing depends on it — the box is not a lesser module, it is an
     unreached one; the meta line says so in amber (§9.1). */
  .count.island {
    color: var(--amber);
  }
  /* Generated code: nobody wrote it by hand and nobody deletes it by hand. */
  .mnode.gen {
    color: var(--fg-4);
  }
  .mnode.gen .count {
    color: var(--fg-4);
  }
  /* Test modules read as scaffolding, not as part of the program. */
  .mnode.test {
    border-style: dashed;
    border-color: var(--line-strong);
  }
  .mnode:focus-visible {
    outline-offset: 4px;
  }
  /* How much leans on the box, as a share of the heaviest one drawn. */
  .weight {
    position: absolute;
    bottom: 3px;
    left: 12px;
    max-width: calc(100% - 24px);
    height: 2px;
    border-radius: 1px;
    background: var(--grad-data);
    pointer-events: none;
  }
  .mnode.dimmed .weight,
  .mnode.gen .weight {
    opacity: 0.3;
  }
  .name {
    overflow: hidden;
    font: var(--t-mono-500);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    overflow: hidden;
    color: var(--fg-3);
    font: var(--t-caption);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
