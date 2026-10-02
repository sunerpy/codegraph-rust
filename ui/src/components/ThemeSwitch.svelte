<!--
  System / Dark / Light (docs/design/viewer-d.md §3.6, "Switching").

  Two shapes of the same three-state control. `rail`: a 40px button in the
  nav-rail foot whose icon says the current choice, opening a three-row menu
  beside the rail. `segmented`: the three choices side by side, for the
  phone's More sheet. Both write through `theme`, which stores the choice and
  sets `data-theme` on <html>.
-->
<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from '../lib/icons';
  import { theme, THEME_CHOICES, THEME_LABEL, type ThemeChoice } from '../lib/theme-choice.svelte';

  interface Props {
    variant?: 'rail' | 'segmented';
    /** Rail only: whether the menu is open (the rail owns which panel is). */
    open?: boolean;
    ontoggle?: () => void;
    onclose?: () => void;
  }

  let { variant = 'rail', open = false, ontoggle, onclose }: Props = $props();

  const ICON: Record<ThemeChoice, IconName> = {
    system: 'monitor',
    dark: 'moon',
    light: 'sun',
  };

  let label = $derived(`Theme: ${THEME_LABEL[theme.choice]}`);

  function pick(choice: ThemeChoice): void {
    theme.set(choice);
    onclose?.();
  }
</script>

{#if variant === 'rail'}
  <div class="slot">
    <button
      type="button"
      class="navbtn"
      class:active={open}
      aria-label={label}
      aria-haspopup="menu"
      aria-expanded={open}
      data-tip={label}
      onclick={() => ontoggle?.()}
    >
      <Icon name={ICON[theme.choice]} />
    </button>
    {#if open}
      <div class="menu" role="menu" aria-label="Theme">
        <div class="micro head">Theme</div>
        {#each THEME_CHOICES as choice (choice)}
          <button
            type="button"
            role="menuitemradio"
            aria-checked={theme.choice === choice}
            class="item"
            class:on={theme.choice === choice}
            onclick={() => pick(choice)}
          >
            <Icon name={ICON[choice]} />
            <span>{THEME_LABEL[choice]}</span>
            {#if choice === 'system'}<span class="dim note">follows the OS</span>{/if}
            {#if theme.choice === choice}<span class="tick"><Icon name="check" size={14} /></span>{/if}
          </button>
        {/each}
      </div>
    {/if}
  </div>
{:else}
  <div class="segmented" role="radiogroup" aria-label="Theme">
    {#each THEME_CHOICES as choice (choice)}
      <button
        type="button"
        role="radio"
        aria-checked={theme.choice === choice}
        class:on={theme.choice === choice}
        onclick={() => pick(choice)}
      >
        <Icon name={ICON[choice]} size={14} />
        {THEME_LABEL[choice]}
      </button>
    {/each}
  </div>
{/if}

<style>
  .slot {
    position: relative;
  }

  .menu {
    position: absolute;
    bottom: 0;
    left: calc(100% + 12px);
    z-index: 60;
    display: flex;
    width: 220px;
    flex-direction: column;
    gap: 2px;
    padding: 8px;
    border: 1px solid var(--line-strong);
    border-radius: 12px;
    background: var(--overlay);
    box-shadow: var(--sh-pop);
  }

  .head {
    padding: 4px 8px 6px;
  }

  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 34px;
    padding: 0 8px;
    border-radius: 8px;
    color: var(--fg-2);
    font: var(--t-small-500);
    text-align: left;
  }

  .item:hover {
    background: var(--raised);
    color: var(--fg);
  }

  .item.on {
    background: var(--primary-soft);
    color: var(--primary-ink);
  }

  .note {
    font: var(--t-caption);
  }

  .tick {
    display: inline-flex;
    margin-left: auto;
  }

  .segmented {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 4px;
    padding: 4px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--card);
  }

  .segmented button {
    display: inline-flex;
    height: 34px;
    align-items: center;
    justify-content: center;
    gap: 6px;
    border: 1px solid transparent;
    border-radius: 8px;
    color: var(--fg-2);
    font: var(--t-small-500);
  }

  .segmented button.on {
    border-color: var(--primary-line);
    background: var(--primary-soft);
    color: var(--primary-ink);
  }
</style>
