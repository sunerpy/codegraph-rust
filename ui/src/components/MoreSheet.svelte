<!--
  The phone's More sheet: the destinations the tab bar has no room for, the
  theme switch (§3.6 puts it here on the phone) and what this viewer is
  reading. A bottom sheet in the §10 recipe — `overlay` + `line` + SH.pop,
  r 18 on top, a 36x4 grabber — over the page dimmed to 60 %.
-->
<script lang="ts">
  import Icon from './Icon.svelte';
  import ThemeSwitch from './ThemeSwitch.svelte';
  import ViewerInfo from './ViewerInfo.svelte';
  import { destinations } from '../lib/destinations';

  interface Props {
    hasScreens?: boolean;
    onclose: () => void;
  }

  let { hasScreens = false, onclose }: Props = $props();

  const IN_TABS = new Set(['start', 'map', 'symbol', 'flow']);
  let rest = $derived(destinations(hasScreens).filter((d) => !IN_TABS.has(d.id)));

  function onkeydown(event: KeyboardEvent): void {
    if (event.key === 'Escape') {
      event.preventDefault();
      onclose();
    }
  }
</script>

<svelte:window {onkeydown} />

<button type="button" class="scrim" aria-label="Close" onclick={onclose}></button>
<div class="sheet" role="dialog" aria-modal="true" aria-label="More">
  <span class="grabber" aria-hidden="true"></span>
  <div class="micro">Go to</div>
  <ul>
    {#each rest as dest (dest.id)}
      <li>
        <a class="dest" class:active={dest.active} href={dest.href} onclick={onclose}>
          <Icon name={dest.icon} />
          <span>{dest.label}</span>
          <span class="chev"><Icon name="chevron-right" size={14} /></span>
        </a>
      </li>
    {/each}
  </ul>
  <div class="micro">Theme</div>
  <ThemeSwitch variant="segmented" />
  <div class="info"><ViewerInfo /></div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 70;
    background: color-mix(in srgb, var(--bg) 60%, transparent);
    cursor: default;
  }

  .sheet {
    position: fixed;
    right: 0;
    bottom: 0;
    left: 0;
    z-index: 71;
    display: flex;
    max-height: 80vh;
    flex-direction: column;
    gap: 10px;
    overflow: auto;
    padding: 18px 16px 24px;
    border: 1px solid var(--line);
    border-bottom: 0;
    border-radius: 18px 18px 0 0;
    background: var(--overlay);
    box-shadow: var(--sh-pop);
  }

  .grabber {
    width: 36px;
    height: 4px;
    flex: 0 0 auto;
    align-self: center;
    margin: -8px 0 4px;
    border-radius: 2px;
    background: var(--line-strong);
  }

  ul {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .dest {
    display: flex;
    align-items: center;
    gap: 12px;
    height: 44px;
    padding: 0 12px;
    border: 1px solid var(--line-faint);
    border-radius: 10px;
    background: var(--card);
    color: var(--fg);
    font: var(--t-body-500);
  }

  .dest.active {
    border-color: var(--primary-line);
    background: var(--primary-soft);
    color: var(--primary-ink);
  }

  .chev {
    display: inline-flex;
    margin-left: auto;
    color: var(--fg-3);
  }

  .info {
    margin-top: 6px;
    padding: 12px;
    border: 1px solid var(--line-faint);
    border-radius: 10px;
    background: var(--card);
  }
</style>
