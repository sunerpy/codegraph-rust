<script lang="ts">
  /**
   * Entry points — where a project starts, and where a flow starts.
   *
   * Four lists, all derived from the graph rather than from a filename
   * convention (see `src/ui-server/api/entrypoints.ts` for what each is derived
   * from), regrouped by the file or directory their rows share.
   *
   * The second half of the screen is the flow: a row that names a callable
   * symbol arms a flow from it, and the panel then wants one more name. That
   * second name can be typed, or picked by arming another row — "how does
   * `POST /v1/payroll/cycles/{cycleID}/run` reach the database" is two clicks
   * once both ends are on screen, which is the whole reason this list and the
   * Flow strip belong on speaking terms.
   *
   * The payload is the palette's: one `/api/entrypoints` serves the search box
   * at rest, the empty screen and this panel, so all three agree on the order.
   */
  import EntrySection from '../components/entry/EntrySection.svelte';
  import Icon from '../components/Icon.svelte';
  import SavedTrails from '../components/SavedTrails.svelte';
  import { palette } from '../lib/palette.svelte';
  import { buildEntryPanel, flowPair, type EntryRow } from '../lib/entry-model';
  import { flowHref, navigate } from '../lib/navigation';
  import { openEntryTarget } from '../lib/walk';

  interface Props {
    project?: string | null;
  }
  let { project = null }: Props = $props();

  $effect(() => {
    void palette.ensureEntries();
  });

  let panel = $derived(buildEntryPanel(palette.entries));

  /** The row a flow is being drawn from, and the name it will start at. */
  let armed = $state<{ id: string; name: string } | null>(null);
  let reaches = $state('');
  let input: HTMLInputElement | null = $state(null);

  // A refetch (the index moved) can retire the armed row. Dropping the arming
  // is the honest response: the symbol it named may not be there any more.
  $effect(() => {
    const id = armed?.id;
    if (id && !panel.rows.some((row) => row.id === id)) armed = null;
  });

  function open(row: EntryRow): void {
    openEntryTarget(row.target);
  }

  function draw(from: string, to: string): void {
    const pair = flowPair(from, to);
    if (!pair) return;
    armed = null;
    reaches = '';
    navigate(flowHref(pair));
  }

  function onflow(row: EntryRow): void {
    if (!row.flowFrom) return;
    if (armed === null) {
      armed = { id: row.id, name: row.flowFrom };
      reaches = '';
      // The input is the faster path for anyone who already knows the other
      // end; focusing it costs nothing to anyone who would rather click a row.
      queueMicrotask(() => input?.focus());
      return;
    }
    if (armed.id === row.id) {
      armed = null;
      return;
    }
    draw(armed.name, row.flowFrom);
  }

  function onkeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape') {
      event.preventDefault();
      armed = null;
    }
  }
</script>

<div class="scroll">
  <div class="head">
    <span class="etile"><Icon name="bookmark" /></span>
    <h2>Saved trails and entry points</h2>
    <p>
      Where a flow starts{project ? ` in ${project}` : ''} — every list below is read out of the
      graph, not guessed from a filename. Open a row to read the code, or use
      <span class="chiplike">Flow</span> to draw the path from it to a second symbol.
    </p>
  </div>

  {#if armed}
    <div class="arming" role="group" aria-label="Draw a flow">
      <span class="from">{armed.name}</span>
      <span class="arrow" aria-hidden="true">→</span>
      <input
        bind:this={input}
        bind:value={reaches}
        {onkeydown}
        type="text"
        autocomplete="off"
        spellcheck="false"
        placeholder="a symbol it reaches"
        aria-label={`The symbol ${armed.name} should reach`}
        onkeypress={(event) => {
          if (event.key === 'Enter' && armed) draw(armed.name, reaches);
        }}
      />
      <button
        type="button"
        class="btn primary"
        disabled={flowPair(armed.name, reaches) === null}
        onclick={() => armed && draw(armed.name, reaches)}><Icon name="workflow" />Draw the flow</button
      >
      <button type="button" class="btn" onclick={() => (armed = null)}>Cancel</button>
      <span class="hint">or pick the other end with <span class="chiplike">→ here</span></span>
    </div>
  {/if}

  <!-- The one list here that a person wrote rather than the graph derived. It
       is drawn in full (not hidden when empty) because this screen is where a
       reader comes looking for one. -->
  <div class="saved">
    <SavedTrails hideWhenEmpty={false} />
  </div>

  {#if palette.entriesFailure}
    <p class="state">Could not read the entry points — {palette.entriesFailure}</p>
  {:else if !palette.entriesSettled}
    <p class="state"><span class="pill"><Icon name="refresh-cw" size={14} />Reading the graph…</span></p>
  {:else if panel.empty}
    <p class="state">{panel.empty}</p>
  {:else}
    <div class="sections">
      {#each panel.sections as section (section.id)}
        <EntrySection {section} armed={armed?.id ?? null} onopen={open} {onflow} />
      {/each}
    </div>
  {/if}
</div>

<style>
  /* One island; the lists inside it are D cards, two columns when wide. */
  .scroll {
    height: 100%;
    overflow: auto;
    border: 1px solid var(--line-faint);
    border-radius: var(--island-r);
    background: var(--bg);
  }

  .head {
    display: grid;
    grid-template-columns: 40px minmax(0, 1fr);
    gap: 4px 14px;
    max-width: 920px;
    padding: 22px 24px 4px;
  }

  .etile {
    display: inline-flex;
    width: 40px;
    height: 40px;
    grid-row: span 2;
    align-items: center;
    justify-content: center;
    border-radius: 12px;
    background: var(--primary-soft);
    color: var(--primary-ink);
  }

  .head h2 {
    margin: 0;
    color: var(--fg);
    font: var(--t-h1);
  }

  .head p {
    margin: 0;
    color: var(--fg-2);
    font: var(--t-body);
  }

  .chiplike {
    padding: 1px 7px;
    border: 1px solid var(--line);
    border-radius: 9px;
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .saved {
    max-width: 920px;
    padding: 14px 24px 0;
  }

  .arming {
    position: sticky;
    top: 0;
    z-index: 4;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin: 14px 24px 0;
    padding: 10px 14px;
    border: 1px solid var(--primary-line);
    border-radius: 12px;
    background: var(--primary-soft);
    box-shadow: var(--sh-pop);
  }

  .arming .from {
    color: var(--primary-ink);
    font: var(--t-mono-500);
  }

  .arming .arrow {
    color: var(--fg-3);
  }

  .arming input {
    width: 240px;
    height: 30px;
    padding: 0 10px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--raised);
    color: var(--fg);
    font: var(--t-mono);
  }

  .arming input:focus {
    border-color: var(--primary-line);
    outline: none;
    box-shadow: var(--glow-25);
  }

  .arming .hint {
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .state {
    max-width: 760px;
    padding: 16px 24px 40px;
    color: var(--fg-2);
    font: var(--t-body);
  }

  .sections {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(420px, 1fr));
    gap: var(--gap);
    margin: 14px 24px 32px;
  }

  @media (max-width: 599px) {
    .head,
    .saved {
      padding-right: 14px;
      padding-left: 14px;
    }

    .sections {
      grid-template-columns: minmax(0, 1fr);
      margin: 12px 14px 24px;
    }
  }
</style>
