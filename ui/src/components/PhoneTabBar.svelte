<!--
  The phone's bottom tab bar (docs/design/viewer-d.md §10, D-11): 64 high on
  `rail`, five tabs — Start, Map, Symbol, Flow and More — icon 20 over a caption.
  The active tab is `primary-ink` with a 28x3 GRAD.brand indicator at the top.
  More opens the sheet with the other destinations and the theme switch.
-->
<script lang="ts">
  import Icon from './Icon.svelte';
  import { destinations } from '../lib/destinations';

  interface Props {
    hasScreens?: boolean;
    moreOpen?: boolean;
    onmore: () => void;
  }

  let { hasScreens = false, moreOpen = false, onmore }: Props = $props();

  const TABS = new Set(['start', 'map', 'symbol', 'flow']);

  let all = $derived(destinations(hasScreens));
  let tabs = $derived(all.filter((d) => TABS.has(d.id)));
  /** More is the current tab when what is open lives behind it. */
  let moreActive = $derived(moreOpen || all.some((d) => !TABS.has(d.id) && d.active));
</script>

<nav class="tabbar" aria-label="Views">
  {#each tabs as tab (tab.id)}
    <a class="tab" class:active={tab.active && !moreOpen} href={tab.href} aria-current={tab.active ? 'page' : undefined}>
      <Icon name={tab.icon} size={20} />
      <span>{tab.short}</span>
    </a>
  {/each}
  <button type="button" class="tab" class:active={moreActive} aria-expanded={moreOpen} onclick={onmore}>
    <Icon name="menu" size={20} />
    <span>More</span>
  </button>
</nav>

<style>
  .tabbar {
    grid-area: tabs;
    display: none;
  }

  @media (max-width: 599px) {
    .tabbar {
      position: relative;
      z-index: 30;
      display: grid;
      height: 64px;
      grid-template-columns: repeat(5, minmax(0, 1fr));
      border-top: 1px solid var(--line-faint);
      background: var(--rail);
    }
  }

  .tab {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 4px;
    color: var(--fg-3);
    font: var(--t-caption);
  }

  .tab.active {
    color: var(--primary-ink);
  }

  .tab.active::before {
    position: absolute;
    top: -1px;
    left: 50%;
    width: 28px;
    height: 3px;
    border-radius: 0 0 2px 2px;
    background: var(--grad-brand);
    content: '';
    transform: translateX(-50%);
  }
</style>
