<!--
  The error state (docs/design/viewer-d.md §7 callouts, D-02s error): a card in
  place of what failed to load — a red tile with circle-x, what went wrong in
  `h2`, the server's own sentence, its remedy when it gave one (with a copy
  button when the remedy is a command), and Retry when trying again can help.
  Red is used for failure and nothing else (§3.3).
-->
<script lang="ts">
  import Icon from './Icon.svelte';
  import { toast } from '../lib/toast.svelte';

  interface Props {
    title: string;
    message: string;
    /** The server's hint — often the command that fixes it. */
    guidance?: string | null;
    /** Offered only when a second attempt could succeed. */
    onretry?: (() => void) | null;
  }

  let { title, message, guidance = null, onretry = null }: Props = $props();

  /** A hint that is one `codegraph …` command gets a copy button. */
  let command = $derived.by(() => {
    if (!guidance) return null;
    const match = guidance.match(/`(codegraph[^`]*)`|\b(codegraph (?:init|sync|index)[^.;\n]*)/);
    return (match?.[1] ?? match?.[2] ?? '').trim() || null;
  });

  async function copy(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      toast.show(`Copied · ${text}`);
    } catch {
      toast.show('Copy is not available in this browser');
    }
  }
</script>

<div class="errorcard" role="alert">
  <span class="tile"><Icon name="circle-x" size={20} /></span>
  <div>
    <h2>{title}</h2>
    <p>{message}</p>
    {#if guidance}<p class="dim">{guidance}</p>{/if}
    {#if command || onretry}
      <div class="actions">
        {#if command}
          <span class="cmd mono">{command}</span>
          <button type="button" class="btn secondary" onclick={() => copy(command as string)}>
            <Icon name="copy" />Copy
          </button>
        {/if}
        {#if onretry}
          <button type="button" class="btn primary" onclick={() => onretry?.()}>
            <Icon name="refresh-cw" />Retry
          </button>
        {/if}
      </div>
    {/if}
  </div>
</div>

<style>
  .cmd {
    display: inline-flex;
    height: 30px;
    align-items: center;
    padding: 0 12px;
    border: 1px solid var(--line);
    border-radius: 8px;
    background: var(--raised);
    color: var(--fg);
    font: var(--t-mono);
  }
</style>
