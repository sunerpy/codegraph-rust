<script lang="ts">
  /**
   * The path the reader walked — and the one place they can keep it.
   *
   * "Save trail" is the viewer's only write. It opens a one-field form rather
   * than a dialog because naming a walk is a thought the reader is already
   * having; anything modal would stop the reading to ask about filing.
   */
  import KindGlyph from './KindGlyph.svelte';
  import Icon from './Icon.svelte';
  import { trail, hopLabel, encodeTrail } from '../lib/trail.svelte';
  import { navigate, symbolHref, flowHref } from '../lib/navigation';
  import { trails } from '../lib/trails.svelte';
  import { replacedTrail, trailNameProblem } from '../lib/trails-model';
  import { toast } from '../lib/toast.svelte';

  /** Matches `MAX_TRAIL_NAME` in `src/ui-server/api/trail-store.ts`. */
  const MAX_NAME = 120;

  let hops = $derived(trail.hops);

  // The read-only pill needs to know before Save is pressed whether saving is
  // on at all, so the list is asked for as soon as there is a trail to keep.
  $effect(() => {
    if (hops.length > 0) void trails.ensure();
  });

  /* ---- fitting the hops: the earliest collapse into a `+N` pill (§2) ----
     JetBrains Mono advances 0.6 em, so a hop's width is arithmetic: 8 + tile
     18 + 8 + 7.2 per character + 10, and 20 for the chevron before it. */
  const HOP_CHROME = 44;
  const CHAR_W = 7.2;
  const SEP_W = 20;
  const MORE_W = 44;

  let hopsWidth = $state(0);
  let expanded = $state(false);

  function hopWidth(index: number): number {
    const hop = hops[index];
    return (hop ? HOP_CHROME + hopLabel(hop).length * CHAR_W : 0) + (index > 0 ? SEP_W : 0);
  }

  /** How many of the earliest hops fold into the `+N` pill. */
  let hidden = $derived.by(() => {
    if (expanded || hopsWidth === 0) return 0;
    let total = 0;
    for (let i = 0; i < hops.length; i++) total += hopWidth(i);
    if (total <= hopsWidth) return 0;
    let used = MORE_W;
    let shown = 0;
    for (let i = hops.length - 1; i >= 0; i--) {
      const width = hopWidth(i);
      if (used + width > hopsWidth && shown > 0) break;
      used += width;
      shown += 1;
    }
    return hops.length - shown;
  });

  // A new walk starts folded again.
  $effect(() => {
    void hops.length;
    expanded = false;
  });

  /** The phone's back chip steps to the hop before the current one. */
  let previous = $derived(hops.length > 1 ? hops[hops.length - 2] : null);

  let naming = $state(false);
  let name = $state('');
  let nameInput: HTMLInputElement | null = $state(null);

  // The list is wanted before Save is pressed, not after: it decides whether
  // this name would REPLACE something, which the form has to say beforehand.
  $effect(() => {
    if (naming) void trails.ensure();
  });

  let problem = $derived(trailNameProblem(name, MAX_NAME));
  let replaces = $derived(naming ? replacedTrail(name, trails.list) : null);

  function openForm() {
    trails.clearFailure();
    naming = true;
    // The last hop is the thing the reader is looking at, so it is the most
    // likely name for the walk that got there — offered, not imposed.
    name = trail.current?.name ?? '';
    queueMicrotask(() => {
      nameInput?.focus();
      nameInput?.select();
    });
  }

  function closeForm() {
    naming = false;
    name = '';
  }

  async function submit(event: Event) {
    event.preventDefault();
    if (problem || trails.busy) return;
    const replacing = replaces !== null;
    const saved = await trails.save(name, '', hops);
    if (saved === null) return; // the reason is on `trails.failure`, shown below
    toast.show(replacing ? `Trail replaced · ${name.trim()}` : `Trail saved · ${name.trim()}`);
    closeForm();
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      event.preventDefault();
      event.stopPropagation();
      closeForm();
    }
  }

  function step(index: number) {
    const hop = hops[index];
    if (!hop) return;
    trail.truncateTo(index);
    navigate(symbolHref(hop.id, { trail: encodeTrail(trail.hops) }));
  }

  /**
   * The walk itself IS the flow: the Flow view does not search for a path, it
   * looks up the edge already joining each consecutive pair and draws the cards
   * at those lines. So the trail travels under the same `t` param it uses
   * everywhere else — a flow read from a trail is one walk under two lenses.
   */
  function readAsFlow() {
    navigate(flowHref({ trail: encodeTrail(hops) }));
  }

  /**
   * Clear the path, keep the place.
   *
   * Emptying the trail while you are reading a symbol would also throw the
   * symbol away, which is not what "Clear" says. It restarts the trail at
   * where you are — one `start` hop — and only leaves for the empty screen
   * when there is nowhere to stay.
   */
  function clear() {
    const here = trail.current;
    trail.clear();
    if (!here) {
      navigate('#/');
      return;
    }
    trail.push({ id: here.id, name: here.name, kind: here.kind, dir: 'start' });
    navigate(symbolHref(here.id, { trail: encodeTrail(trail.hops) }), { replace: true });
  }
</script>

<!-- One root element, always: the save form is a second row inside it rather
     than a sibling, so a host's layout still sees the trail bar as one box
     whose height grows only while the form is open. With no trail there is
     nothing to draw, and the islands move up under the command bar (§2). -->
<div class="trailwrap" class:empty={hops.length === 0}>
{#if hops.length > 0}
<div class="trailbar">
  <span class="label micro">Trail</span>

  <!-- Phone: one back chip — the hop before this one — and the count. -->
  <span class="phone">
    {#if previous}
      <button type="button" class="back" onclick={() => step(hops.length - 2)} title="Step back to {hopLabel(previous)}">
        <Icon name="chevron-left" size={14} />
        <KindGlyph kind={previous.kind} />
        <span>{hopLabel(previous)}</span>
      </button>
    {/if}
    <span class="count">trail · {hops.length} hop{hops.length === 1 ? '' : 's'}</span>
  </span>

  <div class="hops" bind:clientWidth={hopsWidth}>
    {#if hidden > 0}
      <button
        type="button"
        class="more"
        title="Show the {hidden} earlier hop{hidden === 1 ? '' : 's'}"
        onclick={() => (expanded = true)}>+{hidden}</button
      >
    {/if}
    {#each hops as hop, i (hop.id)}
      {#if i >= hidden}
        {#if i > hidden}
          <span
            class="hop-arrow"
            class:up={hop.dir === 'up'}
            title={hop.dir === 'up'
              ? 'stepped up to a caller'
              : hop.dir === 'down'
                ? 'stepped down into a call'
                : 'jumped here'}
            aria-hidden="true"
          >
            {#if hop.dir === 'up'}<Icon name="corner-left-up" size={14} />{:else if hop.dir === 'down'}<Icon
                name="chevron-right"
                size={14}
              />{:else}<i class="jump"></i>{/if}
          </span>
        {/if}
        <button
          type="button"
          class="hop"
          class:cur={i === hops.length - 1}
          aria-current={i === hops.length - 1 ? 'true' : undefined}
          onclick={() => step(i)}
        >
          <KindGlyph kind={hop.kind} />
          <span>{hopLabel(hop)}</span>
        </button>
      {/if}
    {/each}
  </div>

  <div class="actions">
    {#if hops.length > 1}
      <button type="button" class="btn" onclick={readAsFlow}><Icon name="workflow" />Read as flow</button>
    {/if}
    {#if trails.canSave && !naming}
      <button type="button" class="btn secondary" onclick={openForm}><Icon name="bookmark" />Save trail</button>
    {:else if !trails.canSave}
      <span class="pill bordered ro" title={trails.readOnlyReason ?? undefined}>
        <Icon name="lock" size={14} />{trails.payload?.readOnly
          ? 'Read-only (--read-only) · trails not saved'
          : 'Read-only · trails not saved'}
      </span>
    {/if}
    <button type="button" class="btn" onclick={clear}>Clear</button>
  </div>
</div>
{/if}

{#if naming}
  <form class="saveform" onsubmit={submit}>
    <label for="trail-name">Name this trail</label>
    <input
      bind:this={nameInput}
      bind:value={name}
      {onkeydown}
      id="trail-name"
      type="text"
      maxlength={MAX_NAME}
      autocomplete="off"
      spellcheck="false"
      placeholder="How a request reaches the handler"
    />
    <button type="submit" class="btn primary" disabled={problem !== null || trails.busy}>
      {trails.busy ? 'Saving…' : replaces ? 'Replace' : 'Save'}
    </button>
    <button type="button" class="btn" onclick={closeForm}>Cancel</button>
    <!-- Everything the reader should know BEFORE pressing, in one line: what
         it will be called, that it will overwrite, and where it lands. -->
    <span class="hint" class:warn={replaces !== null}>
      {#if replaces}
        Replaces the saved trail of the same name.
      {:else if trails.directory}
        {hops.length} hop{hops.length === 1 ? '' : 's'} · saved to {trails.directory}
      {:else}
        {hops.length} hop{hops.length === 1 ? '' : 's'}
      {/if}
    </span>
    {#if trails.failure}
      <span class="err">{trails.failure}</span>
    {/if}
  </form>
{/if}
</div>

<style>
  /* §2 trail ribbon: 72,56, W−80 x 40, r 12, `panel` + `line-faint`. */
  .trailwrap {
    grid-area: trail;
    display: flex;
    min-height: 0;
    min-width: 0;
    flex-direction: column;
    margin: 0 var(--gap);
    border: 1px solid var(--line-faint);
    border-radius: var(--island-r);
    background: var(--panel);
  }

  .trailwrap.empty {
    display: none;
  }

  .trailbar {
    display: flex;
    height: calc(var(--trailbar-h, 40px) - 2px);
    min-width: 0;
    align-items: center;
    flex: 0 0 auto;
    gap: 0;
    padding: 0 10px 0 16px;
    white-space: nowrap;
  }

  .label {
    margin-right: 12px;
  }

  .hops {
    display: flex;
    min-width: 0;
    flex: 1;
    align-items: center;
    overflow-x: auto;
    scrollbar-width: none;
  }

  .hops::-webkit-scrollbar {
    display: none;
  }

  .more {
    height: 22px;
    flex: 0 0 auto;
    margin-right: 10px;
    padding: 0 8px;
    border-radius: 11px;
    background: var(--raised);
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .more:hover {
    color: var(--fg);
  }

  /* Hops: 28 high, r 8; tile 18 at 8,5 and the name in `mono` at 34. The
     current one is `primary-soft` + `primary-line` with the 25 % glow. */
  .hop {
    display: inline-flex;
    height: 28px;
    flex: 0 0 auto;
    align-items: center;
    gap: 8px;
    padding: 0 10px 0 8px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--raised);
    color: var(--fg);
    font: var(--t-mono);
  }

  .hop:hover {
    border-color: var(--line-strong);
  }

  .hop.cur {
    border-color: var(--primary-line);
    background: var(--primary-soft);
    box-shadow: var(--glow-25);
    color: var(--primary-ink);
  }

  .hop-arrow {
    display: inline-flex;
    width: 20px;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    color: var(--fg-4);
  }

  .hop-arrow.up {
    color: var(--fg-3);
  }

  .jump {
    width: 3px;
    height: 3px;
    border-radius: 50%;
    background: currentColor;
  }

  .actions {
    display: flex;
    flex: 0 0 auto;
    align-items: center;
    gap: 6px;
    margin-left: 12px;
  }

  .ro {
    color: var(--fg-2);
  }

  .phone {
    display: none;
  }

  /* ---------- the one-field save form ---------- */

  .saveform {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 16px 10px;
    border-top: 1px solid var(--line-faint);
    flex-wrap: wrap;
  }

  .saveform label {
    color: var(--fg-2);
    font: var(--t-small-500);
  }

  .saveform input {
    width: 320px;
    height: 32px;
    max-width: 100%;
    padding: 0 12px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--raised);
    color: var(--fg);
    font: var(--t-body);
  }

  .saveform input:focus {
    border-color: var(--primary-line);
    outline: none;
    box-shadow: var(--glow-25);
  }

  .saveform input::placeholder {
    color: var(--fg-4);
  }

  .hint {
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .hint.warn {
    color: var(--amber);
  }

  .err {
    color: var(--red);
    font: var(--t-caption);
  }

  @media (max-width: 1023px) {
    .actions .btn:not(.secondary) {
      display: none;
    }
  }

  /* §10 phone: a back chip (r 15, 30 high) with the previous hop, and the
     trail's length. The full walk is one tap away on the chip. */
  @media (max-width: 599px) {
    .trailwrap {
      border: 0;
      background: transparent;
    }

    .trailbar {
      padding: 0 4px;
    }

    .label,
    .hops,
    .actions {
      display: none;
    }

    .phone {
      display: flex;
      min-width: 0;
      align-items: center;
      gap: 10px;
    }

    .back {
      display: inline-flex;
      height: 30px;
      min-width: 0;
      align-items: center;
      gap: 6px;
      padding: 0 12px 0 8px;
      border: 1px solid var(--line);
      border-radius: 15px;
      background: var(--raised);
      color: var(--fg);
      font: var(--t-mono);
    }

    .back span:last-child {
      overflow: hidden;
      text-overflow: ellipsis;
    }

    .count {
      color: var(--fg-3);
      font: var(--t-small);
    }
  }
</style>
