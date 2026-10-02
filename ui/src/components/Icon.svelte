<script lang="ts" module>
  import { ICON_SHAPES, type IconName } from '../lib/icons';

  /**
   * One data URI per icon, built once: the shape stroked at 1.5 on its 24
   * viewBox (§6 — 1px at 16, 0.875 at 14), used as a CSS mask so the icon
   * takes `currentColor` like text does.
   *
   * A mask rather than inline <svg>: the icon adds no nodes to the document,
   * so the components that count their own SVG (the hierarchy's connectors,
   * the map's edges) count only what they drew, and the CSP's `img-src data:`
   * already allows it.
   */
  const urls = new Map<IconName, string>();

  export function iconUrl(name: IconName): string {
    let url = urls.get(name);
    if (!url) {
      const svg =
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 24 24' fill='none' stroke='black' " +
        "stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'>" +
        ICON_SHAPES[name] +
        '</svg>';
      url = `url("data:image/svg+xml,${encodeURIComponent(svg)}")`;
      urls.set(name, url);
    }
    return url;
  }
</script>

<script lang="ts">
  interface Props {
    name: IconName;
    /** Edge length in px: 12 inline, 14 in pills, 16 in the UI, 20 on the tab bar. */
    size?: number;
    /** Announced name; omit for a decorative icon next to its own label. */
    label?: string;
  }

  let { name, size = 16, label }: Props = $props();
  let url = $derived(iconUrl(name));
</script>

<span
  class="icon"
  style:width={`${size}px`}
  style:height={`${size}px`}
  style:mask-image={url}
  style:-webkit-mask-image={url}
  role={label ? 'img' : undefined}
  aria-label={label}
  aria-hidden={label ? undefined : 'true'}
></span>

<style>
  .icon {
    display: inline-block;
    flex: 0 0 auto;
    background-color: currentColor;
    mask-position: center;
    mask-repeat: no-repeat;
    mask-size: contain;
    -webkit-mask-position: center;
    -webkit-mask-repeat: no-repeat;
    -webkit-mask-size: contain;
  }
</style>
