<!--
  Outline or source — the two readings of a file (design spec §3.4).

  A link, not a toggle, because the choice belongs in the URL: a file opened at
  a line from a search result, a flow card or a review comment has to reopen in
  the same mode, and `?src=1` is how it travels.
-->
<script lang="ts">
  import Icon from '../Icon.svelte';
  import { fileHref } from '../../lib/navigation';

  interface Props {
    path: string;
    line: number | null;
    /** Which mode is showing. */
    source: boolean;
  }

  let { path, line, source }: Props = $props();
</script>

<nav class="modes" aria-label="File view mode">
  <a
    class="mode"
    class:on={!source}
    href={fileHref(path, line ? { line } : {})}
    aria-current={!source ? 'page' : undefined}><Icon name="list-tree" />Outline</a
  >
  <a
    class="mode"
    class:on={source}
    href={fileHref(path, { source: true, ...(line ? { line } : {}) })}
    aria-current={source ? 'page' : undefined}><Icon name="file-code-2" />Whole-file source</a
  >
</nav>

<style>
  .modes {
    display: inline-flex;
    gap: 4px;
    padding: 3px;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--card);
  }

  .mode {
    display: inline-flex;
    height: 28px;
    align-items: center;
    gap: 6px;
    padding: 0 10px;
    border: 1px solid transparent;
    border-radius: 8px;
    color: var(--fg-2);
    font: var(--t-small-500);
    text-decoration: none;
    white-space: nowrap;
  }

  .mode:hover {
    background: var(--raised);
    color: var(--fg);
  }

  .mode.on {
    border-color: var(--primary-line);
    background: var(--primary-soft);
    color: var(--primary-ink);
  }
</style>
