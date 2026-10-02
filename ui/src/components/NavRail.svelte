<!--
  The icon nav rail (docs/design/viewer-d.md §2, §10).

  Desktop: 64 wide on the page background, the brand tile at the top, one 40px
  destination per view 46px apart, and the rail foot — the theme switch above
  the keyboard sheet and the viewer settings. Tablet: the same at 56 with 36px
  destinations. The phone has no rail; its tab bar (`PhoneTabBar`) carries the
  same destinations.

  The destinations are upstream's top-bar tabs plus Start; Saved trails opens
  the entry-points screen, which lists them first.
-->
<script lang="ts">
  import Icon from './Icon.svelte';
  import BrandMark from './BrandMark.svelte';
  import ThemeSwitch from './ThemeSwitch.svelte';
  import ShortcutSheet from './ShortcutSheet.svelte';
  import ViewerInfo from './ViewerInfo.svelte';
  import type { IconName } from '../lib/icons';
  import { destinations, type Destination } from '../lib/destinations';

  interface Props {
    /** The project's graph holds screen navigation — `#/` renders Screens. */
    hasScreens?: boolean;
  }

  let { hasScreens = false }: Props = $props();

  let items = $derived(destinations(hasScreens));

  type Panel = 'theme' | 'keys' | 'about';
  let open = $state<Panel | null>(null);
  let foot: HTMLElement | null = $state(null);

  function toggle(panel: Panel): void {
    open = open === panel ? null : panel;
  }

  function onpointerdown(event: PointerEvent): void {
    if (open === null) return;
    if (event.target instanceof Node && foot?.contains(event.target)) return;
    open = null;
  }

  function onkeydown(event: KeyboardEvent): void {
    if (open !== null && event.key === 'Escape') {
      event.preventDefault();
      open = null;
    }
  }

  const FOOT: Array<{ panel: Panel; icon: IconName; label: string }> = [
    { panel: 'keys', icon: 'keyboard', label: 'Keyboard shortcuts' },
    { panel: 'about', icon: 'settings', label: 'Viewer settings' },
  ];
</script>

<svelte:window {onpointerdown} {onkeydown} />

<nav class="navrail" aria-label="Views">
  <a class="brand" href="#/" aria-label="CodeGraph — start">
    <BrandMark />
  </a>

  <ul class="dest">
    {#each items as item (item.id)}
      {@render destination(item)}
    {/each}
  </ul>

  <div class="foot" bind:this={foot}>
    <ThemeSwitch open={open === 'theme'} ontoggle={() => toggle('theme')} onclose={() => (open = null)} />
    {#each FOOT as entry (entry.panel)}
      <div class="slot">
        <button
          type="button"
          class="navbtn"
          class:active={open === entry.panel}
          aria-label={entry.label}
          aria-expanded={open === entry.panel}
          data-tip={entry.label}
          onclick={() => toggle(entry.panel)}
        >
          <Icon name={entry.icon} />
        </button>
        {#if open === entry.panel}
          <div class="pop" role="dialog" aria-label={entry.label}>
            {#if entry.panel === 'keys'}<ShortcutSheet />{:else}<ViewerInfo />{/if}
          </div>
        {/if}
      </div>
    {/each}
  </div>
</nav>

{#snippet destination(item: Destination)}
  <li>
    <a
      class="navbtn"
      class:active={item.active}
      href={item.href}
      aria-label={item.label}
      aria-current={item.active ? 'page' : undefined}
      data-tip={item.label}
    >
      <Icon name={item.icon} />
    </a>
  </li>
{/snippet}

<style>
  /* The nav buttons themselves are global (`.navbtn`, app.css): the theme
     switch draws the same button from its own component. */
  .navrail {
    grid-area: nav;
    position: relative;
    z-index: 35;
    display: flex;
    min-height: 0;
    flex-direction: column;
    align-items: center;
    padding: 10px 0 14px;
  }

  .brand {
    display: inline-flex;
    margin-bottom: 20px;
    border-radius: 10px;
  }

  .dest {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .foot {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: auto;
  }

  .slot {
    position: relative;
  }

  /* The active destination's 3x20 GRAD.brand indicator, against the rail's
     left edge (§2: x 0, y + 10). */
  .dest .navbtn.active::before {
    position: absolute;
    top: 10px;
    left: -12px;
    width: 3px;
    height: 20px;
    border-radius: 2px;
    background: var(--grad-brand-v);
    content: '';
  }

  .pop {
    position: absolute;
    bottom: 0;
    left: calc(100% + 12px);
    z-index: 60;
    width: 300px;
    padding: 14px;
    border: 1px solid var(--line-strong);
    border-radius: 12px;
    background: var(--overlay);
    box-shadow: var(--sh-pop);
  }

  @media (max-width: 1023px) {
    .navrail {
      padding: 10px 0 12px;
    }

    .brand {
      margin-bottom: 16px;
    }

    .dest .navbtn.active::before {
      top: 8px;
      left: -10px;
    }
  }

  @media (max-width: 599px) {
    .navrail {
      display: none;
    }
  }
</style>
