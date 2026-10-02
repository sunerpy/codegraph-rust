<!--
  "This file changed on disk after the last index sync."

  One block, said the same way on every screen that can say it. Direction D
  (docs/design/viewer-d.md §3.3, §7) gives drift amber — conditional or stale —
  as an `amber-soft` callout with a triangle-alert icon; upstream kept it grey
  because its amber meant "untested" only (§12 records the change). Not a
  modal: the screen underneath is still mostly true, and interrupting to say
  so would be the overclaim.

  The caller supplies the tail of the sentence, because what follows the dash is
  the only part that differs: what this particular screen did about it.
-->
<script lang="ts">
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';

  interface Props {
    /** Project-relative path, shown in mono. */
    file: string;
    /** The rest of the sentence: what this screen is showing instead. */
    children: Snippet;
  }

  let { file, children }: Props = $props();
</script>

<div class="drift" role="status">
  <span class="glyph"><Icon name="triangle-alert" /></span>
  <span class="body"><code>{file}</code> changed on disk after the last index sync — {@render children()}</span>
</div>

<style>
  .drift {
    display: grid;
    grid-template-columns: 16px minmax(0, 1fr);
    gap: 10px;
    align-items: start;
    padding: 11px 14px;
    border-radius: 10px;
    background: var(--amber-soft);
    color: var(--amber);
    font: var(--t-small);
    line-height: 1.5;
  }

  .glyph {
    display: inline-flex;
    padding-top: 1px;
  }

  .body :global(code) {
    font: var(--t-mono-sm);
    font-weight: 500;
  }

  .body :global(button),
  .body :global(a) {
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    cursor: pointer;
    font: inherit;
    font-weight: 600;
    text-decoration: underline;
    text-underline-offset: 3px;
  }
</style>
