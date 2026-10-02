<script lang="ts">
  /**
   * Dead code — symbols nothing in this repository reaches, grouped by file
   * (design spec §3.11).
   *
   * The screen is a list and a disclaimer, deliberately in that order and
   * deliberately inseparable. The caveat line sits above the rows and never
   * goes away, because the claim behind every row is "no static reference in
   * the index" and not "unused": reflection, a framework registry and a
   * template can all reach code the graph cannot follow. Underneath, every
   * reason a candidate was left off is printed with its count — a list of
   * twenty drawn from two and a half thousand candidates means something very
   * different from a list of twenty drawn from twenty-one.
   *
   * The one switch is "including exported". Off (the default) the list is only
   * symbols nothing outside this repository could import either; on, it widens
   * to symbols the index has no way to check, and says so. It travels in the
   * URL like the map's shape does, so a link reopens the same list.
   *
   * The other half of this task lives on the Map: a module nothing depends on
   * says so in its own count line. See `lib/map-model.ts`.
   */
  import KindGlyph from '../components/KindGlyph.svelte';
  import Icon from '../components/Icon.svelte';
  import { fetchDeadCode, ApiFailure, type WireDeadCode, type WireDeadCodeRow } from '../lib/api';
  import { deadHref, fileHref, navigate, symbolHref } from '../lib/navigation';
  import { live } from '../lib/live.svelte';
  import {
    DEAD_CODE_CAVEAT,
    deadCodeHeadline,
    deadCodeRowMeta,
    deadCodeScale,
    emptyMessage,
    exclusionPhrases,
    groupMeta,
  } from '../lib/deadcode-model';

  interface Props {
    /** Include symbols something outside the index could import. */
    exported?: boolean;
  }

  let { exported = false }: Props = $props();

  let payload = $state<WireDeadCode | null>(null);
  let failure = $state<string | null>(null);
  let loading = $state(true);

  $effect(() => {
    const includeExported = exported;
    // The index moving invalidates every row: a symbol is on this list because
    // of what the graph does NOT contain, which is exactly what a sync changes.
    void live.indexTick;
    const controller = new AbortController();
    loading = true;
    failure = null;
    fetchDeadCode({ includeExported }, controller.signal)
      .then((next) => {
        payload = next;
        loading = false;
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        failure = error instanceof ApiFailure ? error.message : 'The list could not be read.';
        loading = false;
      });
    return () => controller.abort();
  });

  let headline = $derived(deadCodeHeadline(payload));
  let scale = $derived(deadCodeScale(payload));
  let phrases = $derived(exclusionPhrases(payload));

  /** Size bars (§8 D-07): each row against the largest one listed, 140px wide. */
  let maxLines = $derived(
    Math.max(1, ...(payload?.groups ?? []).flatMap((g) => g.rows.map((r) => r.lines)))
  );
  let maxExcluded = $derived(Math.max(1, ...(payload?.excluded ?? []).map((e) => e.count)));
  let listedShare = $derived(
    payload && payload.candidates > 0 ? (100 * payload.rows.total) / payload.candidates : 0
  );

  function open(row: WireDeadCodeRow): void {
    navigate(symbolHref(row.id, { line: row.line }));
  }
</script>

<div class="deadview">
<div class="scroll island">
  <div class="head">
    <span class="dtile"><Icon name="ghost" /></span>
    <div class="ht">
      <h1>Dead code</h1>
      <p>symbols nothing in the index reaches · largest first · grouped by file</p>
    </div>
    <!-- §8 D-07: Internal only / Including exported. -->
    <div class="segmented" role="radiogroup" aria-label="Which symbols to list">
      <a
        role="radio"
        aria-checked={!exported}
        class:on={!exported}
        href={deadHref({ exported: false })}
        title="Symbols nothing outside this repository could import either"
        onclick={(event) => {
          event.preventDefault();
          navigate(deadHref({ exported: false }));
        }}>Internal only</a
      >
      <a
        role="radio"
        aria-checked={exported}
        class:on={exported}
        href={deadHref({ exported: true })}
        title="Also list symbols something outside this repository could import — the index cannot check those"
        onclick={(event) => {
          event.preventDefault();
          navigate(deadHref({ exported: true }));
        }}>Including exported</a
      >
    </div>
  </div>

  <!-- The caveat is never dismissible and never collapsed: it is the
       difference between "nothing references this" and "nobody uses this". -->
  <p class="callout caveat"><Icon name="circle-alert" /><span>{DEAD_CODE_CAVEAT}</span><span class="why">macros, trait objects and reflection leave no edge</span></p>

  {#if failure}
    <p class="state">Could not read the list — {failure}</p>
  {:else if loading && payload === null}
    <div class="state"><span class="pill"><Icon name="refresh-cw" size={14} />Reading the graph…</span></div>
  {:else if payload}
    {#if exported}
      <p class="callout amber">
        <Icon name="triangle-alert" /><span>Exported symbols are on this list. Nothing in this repository references them, but anything
        outside it can — a published package, another service, a script. Read each one before you
        believe it.</span>
      </p>
    {/if}

    {#if payload.groups.length === 0}
      <p class="state">{emptyMessage(payload)}</p>
    {:else}
      <p class="headline">{headline}</p>
      <div class="groups">
        {#each payload.groups as group (group.file)}
          <div class="filegroup" class:gen={group.generated}>
            <div class="fpath">
              <Icon name="folder" size={14} />
              <a href={fileHref(group.file)} title={group.file}>{group.file}</a>
              <b>{groupMeta(group)}</b>
            </div>
            {#each group.rows as row (row.id)}
              <div class="row">
                <KindGlyph kind={row.kind} size={22} />
                <div class="body">
                  <div class="line">
                    <button
                      type="button"
                      class="nm"
                      title={row.qualifiedName}
                      data-dead-row={row.id}
                      onclick={() => open(row)}>{row.name}</button
                    >
                    <a class="ln" href={fileHref(group.file, { source: true, line: row.line })}
                      >{row.file.slice(row.file.lastIndexOf('/') + 1)}:{row.line}</a
                    >
                    {#if row.exported}<span class="pill amber">exported</span>{/if}
                  </div>
                  <div class="meta">{deadCodeRowMeta(row)}</div>
                  {#if row.members.items.length > 0}
                    <div class="members">
                      {#each row.members.items as member (member.id)}
                        <a class="member" href={symbolHref(member.id)}>{member.name}</a>
                      {/each}
                      {#if row.members.truncated}
                        <span class="member more"
                          >+{row.members.total - row.members.shown} more</span
                        >
                      {/if}
                    </div>
                  {/if}
                </div>
                <span class="size bar brand" title={`${row.lines} lines`}><i style:width={`${Math.max(3, Math.round((100 * row.lines) / maxLines))}%`}></i></span>
              </div>
            {/each}
          </div>
        {/each}
      </div>
    {/if}

    {#if payload.rows.truncated}
      <p class="foot">
        Showing {payload.rows.shown} of {payload.rows.total} — the rest are in the index, not on
        this list.
      </p>
    {/if}
  {/if}
</div>

<!-- §8 D-07: what the list leaves out, and why. -->
<aside class="leftoff island">
  <div class="lh"><Icon name="eye-off" /><span>What the list leaves out</span></div>
  {#if payload}
    {#if payload.candidates > 0}
      <div class="big">
        <div class="bn"><b>{payload.candidates.toLocaleString()}</b><span>symbols carry no incoming reference at all</span></div>
        <span class="bar brand"><i style:width={`${Math.max(1, listedShare)}%`}></i></span>
        <div class="split">
          <span><i class="sw on"></i>{payload.rows.total.toLocaleString()} listed</span>
          <span><i class="sw"></i>{payload.excludedTotal.toLocaleString()} left off</span>
        </div>
      </div>
    {:else}
      <p class="dim">{scale || 'Every symbol in this index is referenced by something.'}</p>
    {/if}

    {#if payload.excluded.length > 0}
      <div class="micro sec">Why they were left off</div>
      {#each payload.excluded as entry (entry.reason)}
        <div class="reason">
          <span class="rl">{entry.label}</span>
          <span class="rc mono">{entry.count.toLocaleString()}</span>
          <span class="bar"><i style:width={`${Math.max(2, Math.round((100 * entry.count) / maxExcluded))}%`}></i></span>
        </div>
      {/each}
    {/if}

    {#if !payload.corroborated}
      <p class="dim note">
        The rows were not checked against the text of the files that can reach them, so a
        reference the extractor did not record would not have been caught.
      </p>
    {/if}
    {#if payload.bounded}
      <p class="dim note">
        The scan stopped at its cap — this index holds more unreferenced symbols than were
        considered.
      </p>
    {/if}
    <p class="dim note">
      Every rule removes a symbol something could still reach. What remains has no static
      reference — not proof that it is unused.
    </p>
    <!-- The same facts as one sentence each, for a reader of the text alone. -->
    <div class="sr">
      <p>{scale}</p>
      {#if phrases.length > 0}<ul>{#each phrases as phrase (phrase)}<li>{phrase}</li>{/each}</ul>{/if}
    </div>
  {/if}
</aside>
</div>

<style>
  /* §8 D-07: the list island 924 | what it leaves out 428. */
  .deadview {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 428px;
    gap: var(--gap);
    height: 100%;
    min-height: 0;
  }

  .scroll {
    height: 100%;
    overflow: auto;
    padding: 20px 24px 28px;
  }

  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px 14px;
  }

  .dtile {
    display: inline-flex;
    width: 40px;
    height: 40px;
    align-items: center;
    justify-content: center;
    border-radius: 12px;
    background: var(--raised);
    color: var(--fg-2);
  }

  .ht {
    min-width: 0;
    flex: 1;
  }

  .ht h1 {
    margin: 0;
    color: var(--fg);
    font: var(--t-h1);
  }

  .ht p {
    margin: 0;
    color: var(--fg-3);
    font: var(--t-small);
  }

  .segmented {
    display: inline-flex;
    gap: 4px;
    padding: 4px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--card);
  }

  .segmented a {
    display: inline-flex;
    height: 32px;
    align-items: center;
    padding: 0 14px;
    border: 1px solid transparent;
    border-radius: 9px;
    color: var(--fg-2);
    font: var(--t-small-500);
    text-decoration: none;
    white-space: nowrap;
  }

  .segmented a:hover {
    color: var(--fg);
  }

  .segmented a.on {
    border-color: var(--primary-line);
    background: var(--primary-soft);
    color: var(--primary-ink);
  }

  .caveat {
    margin: 18px 0 0;
    border: 1px solid var(--line-faint);
    background: var(--card);
    color: var(--fg);
    font: var(--t-body);
  }

  .caveat .why {
    margin-left: auto;
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .callout.amber {
    margin: 12px 0 0;
  }

  .state {
    padding: 20px 0;
    color: var(--fg-2);
    font: var(--t-body);
  }

  .headline {
    margin: 16px 0 6px;
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .filegroup {
    margin-top: 14px;
  }

  .filegroup.gen {
    opacity: 0.6;
  }

  .fpath {
    display: flex;
    align-items: center;
    gap: 8px;
    margin: 0 2px 8px;
    color: var(--fg-3);
    font: var(--t-mono-sm);
  }

  .fpath a {
    min-width: 0;
    flex: 1;
    overflow: hidden;
    color: var(--fg-2);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .fpath a:hover {
    color: var(--fg);
    text-decoration: underline;
  }

  .fpath b {
    color: var(--fg-3);
    font: var(--t-caption);
  }

  /* Rows: `card` + `line-faint`, r 10; the size bar GRAD.brand, 140 max. */
  .row {
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr) 140px;
    align-items: center;
    gap: 12px;
    margin-bottom: 6px;
    padding: 10px 14px;
    border: 1px solid var(--line-faint);
    border-radius: 10px;
    background: var(--card);
  }

  .row:hover {
    border-color: var(--line);
    background: var(--raised);
  }

  .body {
    min-width: 0;
  }

  .line {
    display: flex;
    min-width: 0;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 4px 10px;
  }

  .nm {
    color: var(--fg);
    font: var(--t-mono-500);
    text-align: left;
  }

  .nm:hover {
    color: var(--primary-ink);
  }

  .ln {
    color: var(--fg-4);
    font: var(--t-mono-sm);
  }

  .ln:hover {
    color: var(--fg-2);
  }

  .meta {
    margin-top: 2px;
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .members {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 6px;
    margin-top: 6px;
  }

  .member {
    padding: 1px 7px;
    border-radius: 9px;
    background: var(--raised);
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .member.more {
    background: none;
    color: var(--fg-4);
  }

  .size {
    height: 4px;
  }

  .foot {
    margin: 16px 0 0;
    color: var(--fg-2);
    font: var(--t-body);
  }

  .leftoff {
    overflow: auto;
    padding: 0 20px 20px;
  }

  .lh {
    display: flex;
    height: 52px;
    align-items: center;
    gap: 10px;
    color: var(--fg);
    font: var(--t-label);
  }

  .lh :global(.icon) {
    color: var(--fg-3);
  }

  .big {
    padding: 16px 18px;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--card);
  }

  .bn {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 12px;
    margin-bottom: 14px;
    color: var(--fg-2);
    font: var(--t-small);
  }

  .bn b {
    color: var(--fg);
    font: 600 28px / 34px var(--sans);
    font-variant-numeric: tabular-nums;
  }

  .big .bar {
    height: 8px;
    border-radius: 4px;
  }

  .split {
    display: flex;
    gap: 24px;
    margin-top: 10px;
    color: var(--fg-2);
    font: var(--t-small);
  }

  .split span {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .sw {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    background: var(--line-strong);
  }

  .sw.on {
    background: var(--primary);
  }

  .sec {
    margin: 22px 0 10px;
  }

  .reason {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    gap: 6px 10px;
    margin-bottom: 14px;
    color: var(--fg);
    font: var(--t-body);
  }

  .rc {
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .reason .bar {
    grid-column: 1 / -1;
    height: 4px;
  }

  .note {
    margin: 12px 0 0;
    font: var(--t-caption);
    line-height: 1.5;
  }

  .dim {
    color: var(--fg-3);
  }

  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }

  @media (max-width: 1023px) {
    .deadview {
      grid-template-columns: minmax(0, 1fr);
      grid-template-rows: minmax(0, 1fr) auto;
    }

    .leftoff {
      max-height: 40vh;
    }
  }

  @media (max-width: 599px) {
    .scroll {
      padding: 14px;
    }

    .row {
      grid-template-columns: 22px minmax(0, 1fr);
    }

    .size {
      display: none;
    }

    .caveat .why {
      display: none;
    }
  }
</style>
