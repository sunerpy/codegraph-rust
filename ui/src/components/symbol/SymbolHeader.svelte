<!--
  The focus card: what this symbol is, where it lives, and the claims worth
  making before the body (docs/design/viewer-d.md §8 D-02).

  The crumb names the file's path and the symbol's owners; the title row is a
  28px kind tile, the name in `title-mono`, the kind pills and the icon
  actions. The metric pills are the honesty layer — hub, callees, the test
  claim, the extent. "No test reaches this within 3 caller hops" is the one
  that changes behaviour, so it is an amber pill rather than an inference
  from an empty rail.

  A name too long for the row is cut where it would run into its pills, and
  the whole name appears in a tooltip under it (D-02s longname).
-->
<script lang="ts">
  import Icon from '../Icon.svelte';
  import KindGlyph from '../KindGlyph.svelte';
  import { fileHref } from '../../lib/navigation';
  import { kindWord } from '../../lib/kinds';
  import { plural } from '../../lib/symbol-model';
  import { toast } from '../../lib/toast.svelte';
  import type {
    WireNodeDetail,
    WireNodeRef,
    WireRelation,
    WireSymbolPayload,
  } from '../../lib/api';

  interface Props {
    payload: WireSymbolPayload;
    onopen: (node: WireNodeRef) => void;
    /**
     * Draw the `extends X` / `implemented by …` chips.
     *
     * Off when the type-hierarchy tree is on screen: the tree answers the same
     * question with more of the truth in it (depth, synthesized edges, the
     * subtypes that are not direct), and two renderings of one relation in one
     * column is how a reader ends up trusting neither.
     */
    relationChips?: boolean;
    /** The signature line; off when the code card below already starts with it. */
    signature?: boolean;
  }

  let { payload, onopen, relationChips = true, signature = true }: Props = $props();

  let node = $derived<WireNodeDetail>(payload.node);
  let tests = $derived(payload.tests);

  /** The crumb: the file's directories and name, then the owning symbols. */
  let crumb = $derived.by(() => {
    const parts = node.file.split('/');
    const owners = payload.ancestors.filter((a) => a.kind !== 'file');
    return { dirs: parts.slice(0, -1), file: parts[parts.length - 1] ?? node.file, owners };
  });

  /** `extends`/`implements` this symbol declares, and the ones declared on it. */
  let supertypes = $derived(
    relationChips
      ? payload.outgoing.items.filter((r) =>
          r.edgeKinds.some((k) => k === 'extends' || k === 'implements')
        )
      : []
  );
  let subtypes = $derived(
    relationChips
      ? payload.incoming.items.filter((r) =>
          r.edgeKinds.some((k) => k === 'extends' || k === 'implements')
        )
      : []
  );

  const TYPE_CHIP_LIMIT = 12;
  let typeChips = $derived(payload.typesUsed.slice(0, TYPE_CHIP_LIMIT));

  function relationWord(relation: WireRelation): string {
    return relation.edgeKinds.includes('implements') ? 'implements' : 'extends';
  }

  /** Kind pills: the kind, then each modifier the node carries. */
  let kindPills = $derived.by(() => {
    const pills = [node.kind === 'type_alias' ? 'type' : kindWord(node.kind)];
    if (node.async) pills.push('async');
    if (node.static) pills.push('static');
    if (node.abstract) pills.push('abstract');
    if (node.visibility && node.visibility !== 'public') pills.push(node.visibility);
    // Rust says `pub`; the extractors record it as public visibility or export.
    if (node.language === 'rust' ? node.exported || node.visibility === 'public' : node.exported) {
      pills.push(node.language === 'rust' ? 'pub' : 'exported');
    }
    return pills;
  });

  /**
   * The test claim, worded to exactly what was checked. An interrupted search
   * (`exhaustive: false`) only ever established that no test calls the symbol
   * directly, so the pill must not widen that to three hops.
   */
  let testBadge = $derived.by(() => {
    if (tests.reached) {
      return {
        warn: false,
        text: `Tests reach · ${plural(tests.fileCount, 'file')}`,
        title: `Reached by tests within ${tests.hopsSearched} hop${tests.hopsSearched === 1 ? '' : 's'}: ${tests.files.join(', ')}`,
      };
    }
    return {
      warn: true,
      text: tests.exhaustive
        ? `No test reaches this within ${tests.hopsSearched} caller hops`
        : 'No test calls this directly',
      title: tests.exhaustive
        ? 'No test file reaches this symbol within the caller hops searched.'
        : 'The caller search ran out of budget — only direct callers were checked.',
    };
  });

  /* ---- the long-name tooltip: only when the title was actually cut ---- */
  let titleEl: HTMLElement | null = $state(null);
  let cut = $state(false);
  let tipOpen = $state(false);

  $effect(() => {
    const el = titleEl;
    void node.name;
    if (!el) return;
    const measure = () => (cut = el.scrollWidth > el.clientWidth + 1);
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    void document.fonts?.ready.then(measure);
    return () => observer.disconnect();
  });

  async function copyName(): Promise<void> {
    const text = node.qualifiedName || node.name;
    try {
      await navigator.clipboard.writeText(text);
      toast.show(`Copied · ${text}`);
    } catch {
      toast.show('Copy is not available in this browser');
    }
  }
</script>

<nav class="crumb" aria-label="Where this lives">
  {#each crumb.dirs as dir, i (i)}<span>{dir}</span><span class="sep"
      ><Icon name="chevron-right" size={12} /></span
    >{/each}<a href={fileHref(node.file, { line: node.line })}>{crumb.file}</a
  >{#each crumb.owners as owner (owner.id)}<span class="sep"><Icon name="chevron-right" size={12} /></span
    ><button type="button" onclick={() => onopen(owner)}>{owner.name}</button>{/each}
</nav>

<div class="titlerow">
  <KindGlyph kind={node.kind} titled size={28} />
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <h1
    bind:this={titleEl}
    tabindex={cut ? 0 : undefined}
    aria-describedby={tipOpen ? 'symbol-fullname' : undefined}
    onmouseenter={() => (tipOpen = cut)}
    onmouseleave={() => (tipOpen = false)}
    onfocus={() => (tipOpen = cut)}
    onblur={() => (tipOpen = false)}
  >
    {node.name}
  </h1>
  <span class="kinds">
    {#each kindPills as pill (pill)}<span class="pill">{pill}</span>{/each}
  </span>
  <span class="actions">
    <button type="button" class="iconbtn bordered" aria-label="Copy the qualified name" title="Copy the qualified name" onclick={copyName}>
      <Icon name="copy" />
    </button>
    <a class="iconbtn bordered" href={fileHref(node.file, { line: node.line })} aria-label="Open the file" title="Open the file">
      <Icon name="external-link" />
    </a>
  </span>
  {#if tipOpen}
    <div class="tip mono" role="tooltip" id="symbol-fullname">{node.qualifiedName || node.name}</div>
  {/if}
</div>

<div class="metrics">
  {#if payload.hierarchy?.polymorphic}
    <span class="pill violet" title="A call through this type has no single static target">
      <Icon name="layers" size={14} />Polymorphic · {plural(payload.hierarchy.implementers, 'implementation')}
    </span>
  {/if}
  {#if payload.members.total > 0}
    <span class="pill"><Icon name="list-tree" size={14} />{plural(payload.members.total, 'member')}</span>
  {/if}
  {#if payload.counts.hub}
    <span class="pill violet" title="Changing this reaches a lot of the repo">
      <Icon name="zap" size={14} />Hub · {plural(payload.counts.callers, 'caller')}
    </span>
  {/if}
  {#if payload.counts.callees > 0}
    <span class="pill cyan"><Icon name="arrow-right" size={14} />{plural(payload.counts.callees, 'callee')}</span>
  {/if}
  <span class="pill" class:green={!testBadge.warn} class:amber={testBadge.warn} title={testBadge.title}>
    <Icon name={testBadge.warn ? 'triangle-alert' : 'flask-conical'} size={14} />{testBadge.text}
  </span>
  <span class="pill">
    <Icon name="file-code-2" size={14} />{plural(node.lines, 'line')} · {node.line}–{node.endLine}
  </span>
</div>

{#if signature && node.signature}
  <div class="sig">{node.name}{node.signature}</div>
{/if}

{#if node.docstring}
  <div class="doc">{node.docstring}</div>
{/if}

{#if supertypes.length > 0 || subtypes.length > 0 || typeChips.length > 0}
  <div class="rel">
    {#if supertypes.length > 0}
      <span>
        {#each supertypes as relation (relation.node.id)}
          {relationWord(relation)}
          <button type="button" class="chip" onclick={() => onopen(relation.node)}>
            {relation.node.name}
          </button>
        {/each}
      </span>
    {/if}
    {#if subtypes.length > 0}
      <span>
        {subtypes[0]?.edgeKinds.includes('implements') ? 'implemented by' : 'extended by'}
        {#each subtypes as relation (relation.node.id)}
          <button type="button" class="chip" onclick={() => onopen(relation.node)}>
            {relation.node.name}
          </button>
        {/each}
      </span>
    {/if}
    {#if typeChips.length > 0}
      <span>
        uses types
        {#each typeChips as relation (relation.node.id)}
          <button type="button" class="chip" onclick={() => onopen(relation.node)}>
            {relation.node.name}
          </button>
        {/each}
        {#if payload.typesUsed.length > TYPE_CHIP_LIMIT}
          <span class="dim">+{payload.typesUsed.length - TYPE_CHIP_LIMIT}</span>
        {/if}
      </span>
    {/if}
  </div>
{/if}

<style>
  .crumb {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 2px;
    color: var(--fg-3);
    font: var(--t-small);
  }

  .crumb .sep {
    display: inline-flex;
    margin: 0 4px;
    color: var(--fg-4);
  }

  .crumb a,
  .crumb button {
    color: var(--fg-2);
    font: inherit;
  }

  .crumb a:hover,
  .crumb button:hover {
    color: var(--fg);
    text-decoration: underline;
    text-underline-offset: 2px;
  }

  /* Title row: tile 28, the name, its pills 12px after it, the actions 12px
     after those — the name is the one thing that gives way. */
  .titlerow {
    position: relative;
    display: flex;
    min-width: 0;
    align-items: center;
    gap: 12px;
    margin-top: 12px;
  }

  h1 {
    min-width: 0;
    margin: 0;
    overflow: hidden;
    color: var(--fg);
    font: var(--t-title-mono);
    font-variant-ligatures: none;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  h1:focus-visible {
    outline-offset: 2px;
  }

  .kinds {
    display: inline-flex;
    flex: 0 0 auto;
    gap: 6px;
  }

  .actions {
    display: inline-flex;
    flex: 0 0 auto;
    gap: 6px;
    margin-left: auto;
  }

  .tip {
    position: absolute;
    top: calc(100% + 8px);
    left: 40px;
    z-index: 20;
    max-width: calc(100% - 40px);
    padding: 7px 12px;
    overflow-wrap: anywhere;
    border: 1px solid var(--line-strong);
    border-radius: 8px;
    background: var(--overlay);
    box-shadow: var(--sh-pop);
    color: var(--fg);
    font: var(--t-mono);
  }

  .metrics {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    margin-top: 12px;
  }

  .sig {
    margin-top: 12px;
    color: var(--fg-2);
    font: 400 12px / 18px var(--mono);
    font-variant-ligatures: none;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .doc {
    max-width: 78ch;
    margin-top: 12px;
    color: var(--fg-2);
    font: var(--t-body);
    white-space: pre-wrap;
  }

  .rel {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-top: 12px;
    color: var(--fg-3);
    font: var(--t-small);
  }

  .rel > span {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }

  .chip {
    height: 22px;
    padding: 0 9px;
    border: 1px solid var(--line);
    border-radius: 11px;
    background: var(--raised);
    color: var(--fg-2);
    font: var(--t-mono-sm);
  }

  .chip:hover {
    border-color: var(--line-strong);
    color: var(--fg);
  }

  @media (max-width: 599px) {
    .actions,
    .kinds {
      display: none;
    }
  }
</style>
