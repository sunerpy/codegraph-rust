<!--
  The bottom-centre note. Ink fill, paper text, 2.6 s (design spec §appendix).

  `aria-live="polite"` rather than `alert`: the screen has already refreshed by
  the time this appears, so it is a confirmation, not something to interrupt for.
-->
<script lang="ts">
  import { toast } from '../lib/toast.svelte';
</script>

<div class="live-region" aria-live="polite">
  {#if toast.message}
    <div class="toast">{toast.message}</div>
  {/if}
</div>

<style>
  /* `overlay` + `line-strong` + SH.pop, r 10 — a note, not a dialog. */
  .toast {
    position: fixed;
    bottom: 22px;
    left: 50%;
    z-index: 80;
    max-width: 70ch;
    padding: 9px 16px;
    border: 1px solid var(--line-strong);
    border-radius: 10px;
    background: var(--overlay);
    box-shadow: var(--sh-pop);
    color: var(--fg);
    font: var(--t-small-500);
    line-height: 1.4;
    transform: translateX(-50%);
    animation: rise 140ms ease-out;
  }

  @media (max-width: 599px) {
    .toast {
      bottom: 80px;
    }
  }

  @keyframes rise {
    from {
      opacity: 0;
      transform: translate(-50%, 6px);
    }
    to {
      opacity: 1;
      transform: translate(-50%, 0);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .toast {
      animation: none;
    }
  }
</style>
