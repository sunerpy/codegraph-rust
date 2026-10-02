<!--
  The verbatim body, with a gutter port on every line that has an outgoing
  edge and an accent link on every call site (design spec §3.2).

  Two things make this more than a <pre>:

  * Syntax classification arrives already done, from `/api/source` — taken off
    the engine's own tree-sitter parse, server-side, indexed by file line. The
    whole slice is classified in one pass there, so a window that starts 200
    lines into a body still knows it is inside a block comment; nothing is
    re-lexed here.
  * Each ref is matched to an actual token rather than to a column, because the
    recorded column points at the start of the calling expression — see
    `assignRefs`. The overlay CLAIMS a token the highlighter produced; it never
    re-cuts one, which is what keeps the accent underline landing on the
    callee's own name whatever boundaries a grammar chose.
-->
<script lang="ts">
  import Icon from '../Icon.svelte';
  import { tokenClassFor, type Token } from '../../lib/highlight';
  import { toast } from '../../lib/toast.svelte';
  import { assignRefs, type CodeBlock, type LineRef } from '../../lib/symbol-model';
  import { hot } from '../../lib/focus.svelte';

  interface Props {
    block: CodeBlock;
    /** Classified source by 1-based file line — see `tokensByLine`. */
    tokens: Map<number, Token[]>;
    refs: Map<number, LineRef[]>;
    /** The line the definition's own name sits on — it is set in bold there. */
    defLine: number;
    defName: string;
    /** Line from `?hl=` — tinted and scrolled to. */
    highlight: number | null;
    /** Follow a call site; `line` is the file line it sits on. */
    onfollow: (ref: LineRef, line: number) => void;
    /** The card header's file name; omitted, the card has no header. */
    file?: string | null;
  }

  let { block, tokens, refs, defLine, defName, highlight, onfollow, file = null }: Props = $props();

  /** `L109–130`: the first and last line the card draws. */
  let range = $derived.by(() => {
    const first = block.windows[0];
    const last = block.windows[block.windows.length - 1];
    if (!first || !last) return '';
    const end = last.start + last.lines.length - 1;
    return `L${first.start}–${end}`;
  });

  /** Distinct resolved targets called from the body, and how many are lit. */
  let calls = $derived.by(() => {
    const targets = new Set<string>();
    for (const lineRefs of refs.values()) {
      for (const ref of lineRefs) if (ref.targetId && !ref.outside && !ref.uncertain) targets.add(ref.targetId);
    }
    return targets.size;
  });

  async function copyBody(): Promise<void> {
    const text = block.windows.map((w) => w.lines.join('\n')).join('\n…\n');
    try {
      await navigator.clipboard.writeText(text);
      toast.show('Copied the source');
    } catch {
      toast.show('Copy is not available in this browser');
    }
  }

  interface Part {
    text: string;
    cls: string | null;
    ref: LineRef | null;
    def: boolean;
  }

  interface RenderedLine {
    n: number;
    parts: Part[];
    /** 'sure' = at least one resolved edge here; 'unsure' = only guesses. */
    port: 'sure' | 'unsure' | null;
    /** Targets named on this line, so a hovered rail row can light it. */
    targets: string[];
  }

  interface Chunk {
    /** Lines skipped before this window; 0 for the first. */
    gapBefore: number;
    lines: RenderedLine[];
  }

  let chunks = $derived.by<Chunk[]>(() =>
    block.windows.map((window, windowIndex) => ({
      gapBefore: windowIndex === 0 ? 0 : (block.gapsAfter[windowIndex - 1] ?? 0),
      lines: window.lines.map((text, offset) => {
        const n = window.start + offset;
        const lineTokens = tokens.get(n) ?? [{ cls: 'other' as const, text, col: 0 }];
        const lineRefs = refs.get(n) ?? [];
        const claimed = assignRefs(lineTokens, lineRefs);
        return {
          n,
          parts: toParts(lineTokens, claimed, n === defLine ? defName : null),
          port: portFor(lineRefs),
          targets: [...new Set(lineRefs.map((r) => r.targetId).filter((id): id is string => !!id))],
        };
      }),
    }))
  );

  function toParts(line: Token[], claimed: Map<number, LineRef>, definition: string | null): Part[] {
    return line.map((token, index) => {
      const ref = claimed.get(index) ?? null;
      return {
        text: token.text,
        cls: ref ? null : tokenClassFor(token),
        ref,
        def:
          !ref &&
          definition !== null &&
          token.text === definition &&
          token.cls !== 'comment' &&
          token.cls !== 'string',
      };
    });
  }

  /**
   * A filled port means the graph resolved something on this line; a hollow one
   * means it only guessed. A line with no outgoing edge has no port at all —
   * absence is the signal, so an empty gutter must stay empty.
   */
  function portFor(lineRefs: readonly LineRef[]): 'sure' | 'unsure' | null {
    if (lineRefs.length === 0) return null;
    return lineRefs.some((r) => !r.uncertain && !r.outside) ? 'sure' : 'unsure';
  }

  function isHot(line: RenderedLine): boolean {
    return line.n === highlight || line.targets.some((id) => hot.is(id));
  }

  /** Lit lines, for the header's `N calls · 1 hot`. */
  let hotLines = $derived(chunks.reduce((n, c) => n + c.lines.filter((l) => isHot(l)).length, 0));
</script>

<div class="code" data-code-card>
  {#if file}
    <div class="ch">
      <span class="fi"><Icon name="file-code-2" /></span>
      <span class="fname">{file.slice(file.lastIndexOf('/') + 1)}</span>
      <span class="range">{range}</span>
      <span class="sp"></span>
      {#if calls > 0}
        <span class="pill cyan">{calls} call{calls === 1 ? '' : 's'}{#if hotLines > 0} · {hotLines} hot{/if}</span>
      {/if}
      <button type="button" class="iconbtn" aria-label="Copy the source" title="Copy the source" onclick={copyBody}>
        <Icon name="copy" />
      </button>
    </div>
  {/if}
  <div class="lines">
  {#each chunks as chunk (chunk.lines[0]?.n ?? -1)}
    {#if chunk.gapBefore > 0}
      <div class="gap">⋯ {chunk.gapBefore} lines without calls</div>
    {/if}
    {#each chunk.lines as line (line.n)}
      <div class="ln" class:hot={isHot(line)} data-line={line.n}>
        <span class="no">{line.n}</span>
        <span class="tx"
          >{#each line.parts as part, i (i)}{#if part.ref && !part.ref.outside}{@const ref = part.ref}<span
                class="ref"
                class:uncertain={ref.uncertain}
                class:hot={hot.is(ref.targetId)}
                role="link"
                tabindex="0"
                title={ref.title}
                onclick={() => onfollow(ref, line.n)}
                onkeydown={(e) => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault();
                    onfollow(ref, line.n);
                  }
                }}
                onmouseenter={() => hot.set(ref.targetId)}
                onmouseleave={() => hot.clear(ref.targetId)}>{part.text}</span
              >{:else if part.ref}<span class="ref stub" title={part.ref.title}>{part.text}</span
              >{:else if part.def}<span class="t-def">{part.text}</span
              >{:else if part.cls}<span class={part.cls}>{part.text}</span
              >{:else}{part.text}{/if}{/each}</span
        >
        <span class="port">
          {#if line.port}<i class:sure={line.port === 'sure'}></i>{/if}
        </span>
      </div>
    {/each}
  {/each}

  {#if block.tailGap > 0}
    <div class="gap">⋯ {block.tailGap} more lines</div>
  {/if}
  </div>
</div>

<style>
  /* §7 code card: `card` + `line` + SH.card, r 12; a 40px header over a
     `line-faint` rule; 20px lines from y 47; gutter 48 with the numbers
     right-aligned in 34; the port column at the right edge. */
  .code {
    margin-top: 20px;
    overflow: hidden;
    border: 1px solid var(--line);
    border-radius: 12px;
    background: var(--card);
    box-shadow: var(--sh-card);
    font: var(--t-code);
    font-variant-ligatures: none;
  }

  .ch {
    display: flex;
    height: 40px;
    align-items: center;
    gap: 10px;
    padding: 0 8px 0 16px;
    border-bottom: 1px solid var(--line-faint);
  }

  .fi {
    display: inline-flex;
    color: var(--fg-3);
  }

  .fname {
    color: var(--fg);
    font: var(--t-mono-500);
  }

  .range {
    color: var(--fg-3);
    font: var(--t-mono-sm);
    white-space: nowrap;
  }

  .fname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .sp {
    flex: 1;
  }

  .lines {
    padding: 7px 0 14px;
  }

  /* 48px gutter | source | 26px port cell. The port lives in its own column
     so a long line scrolling sideways never slides under it. */
  .ln {
    position: relative;
    display: grid;
    grid-template-columns: 48px minmax(0, 1fr) 26px;
    align-items: stretch;
    height: 20px;
  }

  .ln:hover {
    background: var(--raised);
  }

  /* Hot: `primary-soft` across the card with a 2px GRAD.brand bar at x 0. */
  .ln.hot {
    background: var(--primary-soft);
  }

  .ln.hot::before {
    position: absolute;
    top: 0;
    bottom: 0;
    left: 0;
    width: 2px;
    background: var(--grad-brand-v);
    content: '';
  }

  .no {
    padding-right: 14px;
    color: var(--fg-4);
    font: var(--t-lineno);
    text-align: right;
    user-select: none;
  }

  .tx {
    white-space: pre;
    overflow-x: auto;
    scrollbar-width: none;
  }

  .tx::-webkit-scrollbar {
    display: none;
  }

  .port {
    position: relative;
  }

  /* §7 port: a 7px dot at card width − 18, centred on the line — `cyan`,
     hollow when the line only holds guesses, glowing when hot. */
  .port i {
    position: absolute;
    top: 6.5px;
    left: 8px;
    width: 7px;
    height: 7px;
    border: 1px solid var(--cyan-line);
    border-radius: 50%;
    background: transparent;
  }

  .port i.sure {
    border-color: var(--cyan);
    background: var(--cyan);
  }

  .ln.hot .port i {
    border-color: var(--cyan);
    background: var(--cyan);
    box-shadow: var(--glow-cyan-90);
  }

  .gap {
    margin: 4px 0;
    padding: 2px 0 2px 48px;
    border-top: 1px dashed var(--line);
    border-bottom: 1px dashed var(--line);
    color: var(--fg-4);
    font: var(--t-caption);
  }

  /* ---- §3 syntax: keywords 500, comments AA on every surface code sits on */
  .t-c {
    color: var(--syn-com);
  }

  .t-s {
    color: var(--syn-str);
  }

  .t-k {
    color: var(--syn-kw);
    font-weight: 500;
  }

  .t-n {
    color: var(--syn-num);
  }

  .t-t {
    color: var(--syn-type);
  }

  .t-p {
    color: var(--syn-punct);
  }

  .t-def {
    color: var(--fg);
    font-weight: 600;
  }

  /* §7 call capsule: `cyan-soft` at 85 % behind the callee's name, which
     is `cyan`; on the hot line 100 % with a 1px `cyan-line` border. */
  .ref {
    margin: 0 -3px;
    padding: 1px 3px;
    border-radius: 5px;
    background: color-mix(in srgb, var(--cyan-soft) 85%, transparent);
    color: var(--cyan);
    cursor: pointer;
  }

  .ref:hover,
  .ref.hot,
  .ln.hot .ref {
    background: var(--cyan-soft);
    box-shadow: inset 0 0 0 1px var(--cyan-line);
  }

  .ref.uncertain {
    background: transparent;
    color: var(--fg-2);
    text-decoration: underline dotted var(--fg-4);
    text-underline-offset: 3px;
  }

  /* Outside the index: there is nothing to open, so it does not offer to. */
  .ref.stub {
    background: transparent;
    color: var(--fg-2);
    cursor: default;
    text-decoration: underline dotted var(--line-strong);
    text-underline-offset: 3px;
  }

  .ref.stub:hover {
    box-shadow: none;
  }
</style>
