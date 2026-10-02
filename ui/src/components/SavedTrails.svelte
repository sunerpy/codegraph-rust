<script lang="ts">
  import Icon from './Icon.svelte';
  /**
   * The trails somebody kept — the fifth answer to "where do I start".
   *
   * The other four (routes, executable files, tests, hubs) are derived from the
   * graph and describe the project. This one is written by hand and describes
   * what a person thought was worth explaining, which is why it sits above them
   * on the empty screen: a named walk beats a ranked list every time there is
   * one.
   *
   * Rows follow the search-result grid (18px glyph · name · meta) so the empty
   * screen reads as one list rather than two lists in one column. What they add
   * is the honesty line: a trail is a claim about code that has since moved, and
   * every row says what became of its hops.
   */
  import KindGlyph from './KindGlyph.svelte';
  import { symbolHref, navigate } from '../lib/navigation';
  import { trails } from '../lib/trails.svelte';
  import { trail } from '../lib/trail.svelte';
  import { decodeTrail } from '../lib/trail-codec';
  import {
    isOpenable,
    trailDecay,
    trailExport,
    trailMeta,
    trailOpens,
    trailTitle,
  } from '../lib/trails-model';
  import type { WireTrail } from '../lib/api';

  interface Props {
    /** Heading text. The empty screen and the entry-points panel word it alike. */
    title?: string;
    /** Render nothing at all when there are no saved trails (the empty screen). */
    hideWhenEmpty?: boolean;
  }
  let { title = 'Saved trails', hideWhenEmpty = true }: Props = $props();

  $effect(() => {
    void trails.ensure();
  });

  /** Which row is asking to be confirmed before it is deleted. */
  let confirming = $state<string | null>(null);

  let list = $derived(trails.list);

  /**
   * Open a trail: adopt its hops, then navigate to the one it ends on.
   *
   * The store is primed BEFORE the URL changes so the bar draws named hops
   * immediately rather than a row of hashes that resolve a moment later — the
   * encoded trail carries ids and nothing else, and every name is already here.
   */
  function open(saved: WireTrail) {
    if (!isOpenable(saved)) return;
    const hops = decodeTrail(saved.encoded);
    trail.clear();
    const resolved = saved.hops.filter((hop) => hop.id !== null);
    for (const hop of hops) {
      const known = resolved.find((h) => h.id === hop.id);
      trail.push({ id: hop.id, name: known?.name ?? null, kind: known?.kind ?? null, dir: hop.dir });
    }
    navigate(symbolHref(saved.openId as string, { trail: saved.encoded as string }));
  }

  async function remove(saved: WireTrail) {
    if (confirming !== saved.id) {
      confirming = saved.id;
      return;
    }
    confirming = null;
    await trails.remove(saved.id);
  }

  /**
   * Hand the trail over as the file it is.
   *
   * `.codegraph/` is gitignored wholesale, which is right for a scratch walk
   * and wrong for a tour worth committing — so exporting is a copy the reader
   * makes deliberately, and lands wherever their browser puts downloads.
   */
  function download(saved: WireTrail) {
    const blob = new Blob([trailExport(saved)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `${saved.id}.json`;
    link.click();
    URL.revokeObjectURL(url);
  }
</script>

{#if !(hideWhenEmpty && list.length === 0 && trails.failure === null)}
  <section class="trails" aria-label={title}>
    <div class="head">
      <h3><Icon name="bookmark" />{title}</h3>
      {#if trails.directory}
        <span class="where">newest first · stored in {trails.directory}</span>
      {/if}
    </div>

    {#if trails.failure}
      <p class="msg err">{trails.failure}</p>
    {:else if !trails.settled}
      <p class="msg">Reading saved trails…</p>
    {:else if list.length === 0}
      <p class="msg">
        No saved trails yet. Walk a path through the code, then press
        <strong>Save trail</strong> on the trail bar to keep it.
        {#if trails.readOnlyReason}
          <br />{trails.readOnlyReason}
        {/if}
      </p>
    {:else}
      <div class="rows">
        {#each list as saved (saved.id)}
          {@const decay = trailDecay(saved)}
          {@const opens = trailOpens(saved)}
          <div class="row" class:dead={!isOpenable(saved)}>
            <button
              type="button"
              class="pick"
              title={trailTitle(saved)}
              disabled={!isOpenable(saved)}
              onclick={() => open(saved)}
            >
              <KindGlyph kind={saved.hops[0]?.kind ?? null} size={22} />
              <span class="mid">
                <span class="nm">{saved.name}</span>
                {#if saved.note}<span class="note">{saved.note}</span>{/if}
              </span>
              <span class="meta">{trailMeta(saved)}</span>
            </button>

            <div class="acts">
              <button type="button" class="act" title="Export this trail" onclick={() => download(saved)}><Icon name="external-link" size={14} />Export</button>
              {#if trails.canSave}
                <button
                  type="button"
                  class="act"
                  class:armed={confirming === saved.id}
                  disabled={trails.busy}
                  onclick={() => remove(saved)}
                  onblur={() => (confirming = confirming === saved.id ? null : confirming)}
                >
                  {confirming === saved.id ? 'Delete?' : 'Delete'}
                </button>
              {/if}
            </div>

            <!-- The honesty line. A saved trail is a claim about code that has
                 since moved; this is where the graph gets to say so. -->
            {#if decay || opens}
              <p class="decay" class:warn={decay?.tone === 'warn'}>
                {#if decay?.tone === 'warn'}<Icon name="triangle-alert" size={14} />{/if}<span>{[decay?.text, opens].filter(Boolean).join(' ')}</span>
              </p>
            {/if}
          </div>
        {/each}
      </div>
      {#if trails.payload?.bounded}
        <p class="msg">Only the first trails in the directory are listed.</p>
      {/if}
    {/if}
  </section>
{/if}

<style>
  /* §8 D-01 saved trails: rows 64 high as `card` + `line-faint` r 10; a hop
     the index no longer has gets an `amber-soft` callout under its row. */
  .trails {
    max-width: 920px;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 4px 12px;
    margin-bottom: 10px;
  }

  .trails h3 {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    margin: 0;
    color: var(--fg);
    font: var(--t-label);
  }

  .trails h3 :global(.icon) {
    color: var(--fg-3);
  }

  .where {
    color: var(--fg-4);
    font: var(--t-caption);
  }

  .msg {
    margin: 0;
    padding: 4px 0 0;
    color: var(--fg-3);
    font: var(--t-small);
    line-height: 1.5;
  }

  .msg strong {
    color: var(--fg);
  }

  .msg.err {
    color: var(--red);
  }

  .rows {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .row {
    position: relative;
    border: 1px solid var(--line-faint);
    border-radius: 10px;
    background: var(--card);
  }

  .row:hover {
    border-color: var(--line);
  }

  .pick {
    display: grid;
    width: 100%;
    min-height: 62px;
    align-items: center;
    padding: 10px 14px;
    color: var(--fg);
    gap: 12px;
    grid-template-columns: 22px minmax(0, 1fr) auto;
    text-align: left;
  }

  .pick:disabled {
    color: var(--fg-3);
    cursor: default;
  }

  .mid {
    display: flex;
    min-width: 0;
    flex-direction: column;
  }

  .nm {
    overflow: hidden;
    font: var(--t-body-500);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .pick:hover:not(:disabled) .nm {
    color: var(--primary-ink);
  }

  .note {
    overflow: hidden;
    color: var(--fg-3);
    font: var(--t-caption);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* Room for the actions, which overlay the row's right edge. */
  .meta {
    padding-right: 150px;
    color: var(--fg-3);
    font: var(--t-caption);
    white-space: nowrap;
  }

  /* Always drawn, never revealed on hover: a control that appears when the
     pointer arrives is one a keyboard reader has to guess at. */
  .acts {
    position: absolute;
    top: 16px;
    right: 12px;
    display: flex;
    gap: 6px;
  }

  .act {
    display: inline-flex;
    height: 28px;
    align-items: center;
    gap: 6px;
    padding: 0 10px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--raised);
    color: var(--fg-2);
    font: var(--t-small-500);
  }

  .act:hover:not(:disabled) {
    border-color: var(--line-strong);
    color: var(--fg);
  }

  .act.armed {
    border-color: color-mix(in srgb, var(--red) 40%, transparent);
    background: var(--red-soft);
    color: var(--red);
  }

  .decay {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin: 0 14px 12px 48px;
    padding: 8px 12px;
    border-radius: 8px;
    background: var(--raised);
    color: var(--fg-3);
    font: var(--t-caption);
    line-height: 1.45;
  }

  .decay.warn {
    background: var(--amber-soft);
    color: var(--amber);
  }

  @media (max-width: 599px) {
    .meta {
      display: none;
    }

    .pick {
      grid-template-columns: 22px minmax(0, 1fr);
      padding-right: 150px;
    }
  }
</style>
