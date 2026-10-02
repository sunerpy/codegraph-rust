<script lang="ts" module>
  import type { IconName } from '../lib/icons';

  /** §7 kind tile colour: one tone per family of kinds. */
  const TONE: Record<string, string> = {
    function: 'cyan',
    component: 'cyan',
    module: 'cyan',
    method: 'primary',
    struct: 'green',
    class: 'green',
    union: 'green',
    type_alias: 'green',
    enum: 'amber',
    enum_member: 'amber',
    route: 'amber',
    trait: 'violet',
    interface: 'violet',
    protocol: 'violet',
  };

  /** Kinds that draw a 12px icon instead of a letter. */
  const ICON: Record<string, IconName> = {
    file: 'file-code-2',
    module: 'package',
  };
</script>

<script lang="ts">
  import Icon from './Icon.svelte';
  import { kindLetter, kindWord } from '../lib/kinds';

  interface Props {
    kind: string | null | undefined;
    /** Adds a tooltip; off by default so rails do not fight the browser. */
    titled?: boolean;
    /** Tile edge in px — §7 uses 18, 20, 22, 26 and 28. */
    size?: number;
  }

  let { kind, titled = false, size = 18 }: Props = $props();

  // An unknown kind (a trail hop restored from a URL, before its node is
  // fetched) draws an empty neutral tile. A '?' would read as a claim about
  // the symbol.
  let letter = $derived(kind ? kindLetter(kind) : '');
  let tone = $derived((kind && TONE[kind]) || 'neutral');
  let icon = $derived(kind ? ICON[kind] : undefined);
  let radius = $derived(Math.round(size * 0.3));
  let fontSize = $derived(size >= 26 ? 12 : size >= 22 ? 10.5 : 10);
</script>

<span
  class="k {tone}"
  class:wide={letter.length > 1}
  style:width={`${size}px`}
  style:height={`${size}px`}
  style:border-radius={`${radius}px`}
  style:font-size={`${fontSize}px`}
  title={titled ? kindWord(kind) : undefined}
  aria-hidden={titled ? undefined : 'true'}
  >{#if icon}<Icon name={icon} size={12} />{:else}{letter}{/if}</span
>

<style>
  .k {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    background: var(--raised);
    color: var(--fg-2);
    font-family: var(--mono);
    font-weight: 700;
    font-variant-ligatures: none;
    line-height: 1;
    user-select: none;
  }

  .k.cyan {
    background: var(--cyan-soft);
    color: var(--cyan);
  }

  .k.primary {
    background: var(--primary-soft);
    color: var(--primary-ink);
  }

  .k.green {
    background: var(--green-soft);
    color: var(--green);
  }

  .k.amber {
    background: var(--amber-soft);
    color: var(--amber);
  }

  .k.violet {
    background: var(--violet-soft);
    color: var(--violet);
  }

  /* Two-character letters (Tr, im, ex) lose a little tracking to sit inside
     the tile without touching its edge. */
  .k.wide {
    letter-spacing: -0.04em;
  }
</style>
