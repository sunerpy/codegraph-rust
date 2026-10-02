<!--
  The brand tile (docs/design/viewer-d.md §2): GRAD.brand corner to corner with
  the 35 % glow, and two linked squares in `on-primary` — an outlined node at
  10,10, a filled one at 19,19, and the edge between them — on the 36 grid.
  Drawn at 36 on the desktop rail, 32 on the tablet rail, 30 on the phone.
-->
<script lang="ts">
  interface Props {
    size?: number;
  }

  let { size = 36 }: Props = $props();
</script>

<span class="brand" style:width={`${size}px`} style:height={`${size}px`} style:border-radius={`${Math.round(size * 0.28)}px`}>
  <svg viewBox="0 0 36 36" width={size} height={size} aria-hidden="true" focusable="false">
    <rect x="10.75" y="10.75" width="5.5" height="5.5" rx="1.4" fill="none" stroke="currentColor" stroke-width="1.5" />
    <rect x="19" y="19" width="7" height="7" rx="1.8" fill="currentColor" />
    <path d="M16.25 13.5 C 21 13.5 22.5 15 22.5 19" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
  </svg>
</span>

<style>
  .brand {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    justify-content: center;
    background: var(--grad-brand-d);
    box-shadow: var(--glow-35);
    color: var(--on-primary);
  }

  svg {
    display: block;
  }
</style>
