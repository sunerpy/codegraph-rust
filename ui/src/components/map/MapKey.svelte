<script lang="ts">
  import Icon from '../Icon.svelte';
  /**
   * The Map's key (design spec §3.6), matching the Screens and Steps views'.
   *
   * Each row draws the actual stroke or box rather than a word for it — a
   * reader matches shapes. Two rows here exist because the Map hides things at
   * rest and a picture that hides must say so: the thin links, and the dashed
   * back-edges that appear only once a module is selected. A reader who selects
   * `src/utils` and watches four maroon dashes appear has no way to guess what
   * they are, and the side panel's prose is not where anyone looks for a stroke.
   */

  interface Props {
    /** The weight below which a link waits for a selection. */
    minWeight: number;
    /** How many links are waiting on one right now; the row is skipped at zero. */
    thinCount: number;
    /** Whether the vertical order came from declared edges or from raw counts. */
    declaredBasis: boolean;
    open: boolean;
    onToggle: (open: boolean) => void;
  }
  let { minWeight, thinCount, declaredBasis, open, onToggle }: Props = $props();
</script>

<div class="legend" class:open>
  <button class="legend-h" onclick={() => onToggle(!open)} aria-expanded={open} title={open ? 'Fold the key' : 'Open the full key'}>
    <span class="kl"><svg width="20" height="8" aria-hidden="true"><path d="M1 4 H19" class="k-line" /></svg>calls · width = volume</span>
    <span class="kl"><svg width="20" height="8" aria-hidden="true"><path d="M1 4 H19" class="k-line k-hot" /></svg>touches the selection</span>
    {#if thinCount > 0}<span class="kl dim">{thinCount} link{thinCount === 1 ? '' : 's'} &lt; {minWeight} calls hidden</span>{/if}
    <span class="chev"><Icon name={open ? 'chevron-down' : 'chevron-right'} size={14} /></span>
  </button>
  {#if open}
    <div class="legend-body">
      <div class="lrow">
        <span class="k-box mono">src/api</span>
        <span>A module — one directory, with the symbols and files in it</span>
      </div>
      <div class="lrow">
        <span class="k-box k-weight mono">src/db</span>
        <span>
          The bar along the bottom is how much leans on it — files elsewhere that reference
          straight into it, against the most depended-on box here. The count is on the box
        </span>
      </div>
      <div class="lrow">
        <svg width="44" height="12" aria-hidden="true"><path d="M2 6 H42" class="k-line" /></svg>
        <span>
          Depends on — the box above calls, imports, extends or names a type from the box below.
          Thicker is more references{declaredBasis
            ? ''
            : '; here the layering had too few imports to trust, so it used raw counts'}
        </span>
      </div>
      <div class="lrow">
        <svg width="44" height="12" aria-hidden="true"><path d="M2 6 H42" class="k-line k-back" /></svg>
        <span>
          Points back up — the lighter half of a mutual dependency, or a link with no import or
          declared type behind it. Drawn only while a module it touches is selected
        </span>
      </div>
      <div class="lrow">
        <span class="k-label">top / bottom</span>
        <span>
          A module sits one layer above everything it depends on, so entry points end up at the top
          and the foundations — which depend on nothing below — at the bottom
        </span>
      </div>
      <div class="lrow">
        <span class="k-box k-sel mono">src/api</span>
        <span>Selected: click a module to bring out its links and list its files; everything more than one hop away fades</span>
      </div>
      <div class="lrow">
        <span class="k-label">nothing depends on this</span>
        <span>No link in the index arrives here — a script, a workflow, an unreferenced corner</span>
      </div>
      <div class="lrow">
        <span class="k-box k-test mono">__tests__</span>
        <span>More than half its files are tests; off unless you turn tests on</span>
      </div>
      <div class="lrow">
        <span class="k-box k-gen mono">gen</span>
        <span>Every file in it is tool-generated — nobody wrote it and nobody edits it</span>
      </div>
      {#if thinCount > 0}
        <div class="lrow">
          <span class="k-label">{thinCount} hidden</span>
          <span>
            Links carrying fewer than {minWeight} references wait until you select a module they
            touch, so a weak coincidence never draws as a dependency
          </span>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  /* §7 legend: 34 high at 16 from the bottom-left, `overlay` + `line` +
     SH.pop, r 10 — the one-line key at rest, the full key when opened. */
  .legend {
    position: absolute;
    bottom: 16px;
    left: 16px;
    z-index: 4;
    max-width: min(460px, calc(100% - 32px));
    overflow: hidden;
    border: 1px solid var(--line);
    border-radius: 10px;
    background: var(--overlay);
    box-shadow: var(--sh-pop);
    color: var(--fg-2);
    font: var(--t-caption);
  }
  .legend-h {
    display: flex;
    width: 100%;
    min-height: 34px;
    flex-wrap: nowrap;
    align-items: center;
    gap: 6px 16px;
    padding: 6px 12px;
    border: 0;
    background: transparent;
    color: var(--fg-2);
    cursor: pointer;
    font: var(--t-caption);
    text-align: left;
  }
  .kl {
    display: inline-flex;
    min-width: 0;
    align-items: center;
    gap: 6px;
    white-space: nowrap;
  }

  .kl.dim {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .chev {
    display: inline-flex;
    margin-left: auto;
    color: var(--fg-3);
  }
  .legend-body {
    padding: 4px 12px 10px;
    border-top: 1px solid var(--line-faint);
  }
  .lrow {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 4px 0;
  }
  .lrow > :first-child {
    display: inline-flex;
    flex: 0 0 56px;
    justify-content: center;
  }
  .k-line {
    fill: none;
    stroke: var(--line-strong);
    stroke-width: 2;
  }
  .k-line.k-hot {
    stroke: var(--cyan);
  }
  .k-line.k-back {
    stroke: var(--violet);
    stroke-dasharray: 4 3;
  }
  .k-label {
    color: var(--fg-3);
    font: var(--t-caption);
    line-height: 1.2;
    text-align: center;
  }
  .k-box {
    box-sizing: border-box;
    padding: 1px 6px;
    border: 1px solid var(--line);
    border-radius: 6px;
    background: var(--card);
    color: var(--fg);
    font: var(--t-mono-sm);
    line-height: 16px;
  }
  /* The bar, drawn the way the canvas draws it: inside the bottom edge. */
  .k-box.k-weight {
    position: relative;
  }
  .k-box.k-weight::after {
    position: absolute;
    bottom: 1px;
    left: 4px;
    width: 60%;
    height: 2px;
    border-radius: 1px;
    background: var(--grad-data);
    content: '';
  }
  /* The same treatments the canvas uses, at key size. */
  .k-box.k-sel {
    border-color: var(--primary-line);
    background: var(--primary-soft);
    color: var(--primary-ink);
  }
  .k-box.k-test {
    border-style: dashed;
    border-color: var(--line-strong);
    color: var(--fg-3);
  }
  .k-box.k-gen {
    color: var(--fg-4);
  }
  .dim {
    color: var(--fg-4);
  }
</style>
