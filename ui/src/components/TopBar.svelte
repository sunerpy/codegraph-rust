<!--
  The command bar (docs/design/viewer-d.md §2, §10).

  Desktop: the project on the left, the ⌘K command input centred, the index
  status right-aligned 16px from the edge. Tablet: the input folds into a
  search button beside the status pill. Phone: the brand, the name of what is
  open, a search button and More. In every layout the search opens the same
  palette — `SearchPalette` owns its keyboard — and the status pill is where a
  page that stopped updating itself says so.
-->
<script lang="ts">
  import Icon from './Icon.svelte';
  import BrandMark from './BrandMark.svelte';
  import SearchPalette from './SearchPalette.svelte';
  import { router } from '../lib/router.svelte';
  import { trail } from '../lib/trail.svelte';
  import { live } from '../lib/live.svelte';
  import { palette } from '../lib/palette.svelte';
  import { project } from '../lib/project.svelte';

  interface Props {
    /** Indexed project name, e.g. "codegraph". Null until stats load. */
    project?: string | null;
    /** Opens the phone's More sheet. */
    onmore?: () => void;
  }

  let { project: projectName = null, onmore }: Props = $props();

  let search: SearchPalette | null = $state(null);
  /** Tablet and phone: the palette is folded behind the search button. */
  let unfolded = $state(false);

  /** What `/` and Cmd-K reach — the palette owns its own keyboard. */
  export function focusSearch(): void {
    unfolded = true;
    queueMicrotask(() => search?.focus());
  }

  // Folding the palette away again once it closes keeps the tablet bar tidy.
  $effect(() => {
    if (!palette.open) unfolded = false;
  });

  let stats = $derived(project.stats);
  let n = (value: number): string => value.toLocaleString();

  /**
   * The index-status pill. Green when the index is current and the page is
   * live; amber when either has stopped being true, with the reason on hover.
   */
  let status = $derived.by(() => {
    if (live.degraded !== null) {
      return {
        tone: 'amber',
        short: 'Live updates off',
        text: 'Live updates off',
        title: `${live.degraded} This page no longer refreshes itself — reload it after a sync.`,
      };
    }
    if (live.stopped) {
      return {
        tone: 'amber',
        short: 'Not live',
        text: 'Not live',
        title:
          'Lost the connection to codegraph ui and stopped retrying. Focus this tab to try again, or reload the page.',
      };
    }
    if (!stats) return null;
    const stale = stats.index.stale || (stats.index.state !== null && stats.index.state !== 'complete');
    const counts = `${n(stats.graph.files)} files · ${n(stats.graph.nodes)} nodes`;
    return stale
      ? {
          tone: 'amber',
          short: 'Stale',
          text: `Index stale · ${counts}`,
          title: 'Files changed since the last index. Run codegraph sync to bring the graph up to date.',
        }
      : {
          tone: 'green',
          short: 'Current',
          text: `Index current · ${counts}`,
          title: `${n(stats.graph.edges)} edges · extraction version ${stats.index.extractionVersion ?? '?'}`,
        };
  });

  /** The phone bar names what is open: the symbol, else the view. */
  let phoneTitle = $derived.by(() => {
    const route = router.route;
    if (route.view === 'symbol' && route.id !== null) return trail.current?.name ?? 'Symbol';
    if (route.view === 'file') return route.path.split('/').pop() ?? 'File';
    const words: Record<string, string> = {
      home: projectName ?? 'Start',
      map: 'Map',
      flow: 'Flow',
      entry: 'Entry points',
      screens: 'Screens',
      steps: 'Steps',
      dead: 'Dead code',
      symbol: 'Symbol',
      unknown: 'Not found',
    };
    return words[route.view] ?? 'CodeGraph';
  });
</script>

<header class="topbar">
  <a class="phone-brand" href="#/" aria-label="CodeGraph — start"><BrandMark size={30} /></a>
  <span class="phone-title mono">{phoneTitle}</span>

  <a class="project" href="#/" title={stats?.project.root ?? 'Indexed project'}>
    <Icon name="box" />
    <span class="name">{projectName ?? '…'}</span>
  </a>

  <div class="cmd" class:unfolded>
    <SearchPalette bind:this={search} />
  </div>

  <div class="right">
    <button
      type="button"
      class="iconbtn bordered searchbtn"
      aria-label="Search symbols and files"
      onclick={focusSearch}
    >
      <Icon name="search" />
    </button>
    {#if status}
      <span class="pill status {status.tone}" title={status.title}>
        <span class="dot"></span>
        <span class="long">{status.text}</span>
        <span class="short">{status.short}</span>
      </span>
    {/if}
    <button type="button" class="iconbtn bordered morebtn" aria-label="More" onclick={() => onmore?.()}>
      <Icon name="ellipsis" />
    </button>
  </div>
</header>

{#if palette.open}
  <!-- The page under an open palette dims, from the bar down (§7). -->
  <div class="scrim" aria-hidden="true"></div>
{/if}

<style>
  .topbar {
    grid-area: top;
    position: relative;
    z-index: 30;
    display: grid;
    grid-template-columns: minmax(0, calc(50% - 282px)) 540px minmax(0, 1fr);
    align-items: center;
    padding: 0 16px 0 14px;
  }

  .project {
    display: inline-flex;
    min-width: 0;
    align-items: center;
    gap: 8px;
    color: var(--fg-2);
  }

  .project .name {
    overflow: hidden;
    color: var(--fg);
    font: var(--t-body-500);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .cmd {
    position: relative;
    min-width: 0;
  }

  .right {
    display: flex;
    min-width: 0;
    align-items: center;
    justify-content: flex-end;
    gap: 8px;
  }

  .status {
    overflow: hidden;
    max-width: 100%;
  }

  .status .long {
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .status .short,
  .searchbtn,
  .morebtn,
  .phone-brand,
  .phone-title {
    display: none;
  }

  .scrim {
    position: fixed;
    inset: var(--topbar-h) 0 0 0;
    z-index: 25;
    background: color-mix(in srgb, var(--bg) 60%, transparent);
  }

  /* §10 tablet: the input folds into a search button; the palette unfolds
     over the bar when it is asked for. */
  @media (max-width: 1023px) {
    .topbar {
      grid-template-columns: minmax(0, 1fr) auto;
      padding: 0 12px 0 16px;
    }

    .cmd {
      display: none;
    }

    .cmd.unfolded {
      position: absolute;
      top: 8px;
      right: 12px;
      left: 12px;
      z-index: 40;
      display: block;
    }

    .searchbtn {
      display: inline-flex;
      width: 32px;
      height: 32px;
    }

    .status .long {
      display: none;
    }

    .status .short {
      display: inline;
    }
  }

  /* §10 phone: brand 30, the open thing's name, search and More. */
  @media (max-width: 599px) {
    .topbar {
      grid-template-columns: auto minmax(0, 1fr) auto;
      gap: 10px;
      padding: 0 12px;
    }

    .project,
    .status {
      display: none;
    }

    .phone-brand {
      display: inline-flex;
    }

    .phone-title {
      display: block;
      overflow: hidden;
      color: var(--fg);
      font: var(--t-mono-500);
      text-overflow: ellipsis;
      white-space: nowrap;
    }

    .morebtn {
      display: inline-flex;
      width: 32px;
      height: 32px;
    }

    .cmd.unfolded {
      top: 10px;
    }
  }
</style>
