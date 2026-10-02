<!--
  The keyboard sheet behind the rail's keyboard button: every key the viewer
  answers, read off the handlers that answer them (App.svelte for the global
  keys, SymbolView for the rails). Nothing here is configurable.
-->
<script lang="ts">
  const MOD = /Mac|iPhone|iPad/.test(globalThis.navigator?.platform ?? '') ? '⌘' : 'Ctrl';

  const GLOBAL: Array<[string[], string]> = [
    [['/'], 'Search symbols and files'],
    [[MOD, 'K'], 'Search, from anywhere'],
    [['m'], 'Map'],
    [['f'], 'Flow'],
    [['e'], 'Entry points and saved trails'],
    [['s'], 'Screens'],
    [['d'], 'Dead code'],
    [['⌫'], 'Back one step'],
  ];

  const SYMBOL: Array<[string[], string]> = [
    [['↑', '↓'], 'Move through a rail'],
    [['←', '→'], 'Switch between Called by and Calls'],
    [['⏎'], 'Follow the selected row'],
  ];
</script>

<div class="sheet">
  <div class="micro">Everywhere</div>
  {#each GLOBAL as [keys, what] (what)}
    <div class="row">
      <span class="keys">{#each keys as key (key)}<span class="kbd">{key}</span>{/each}</span>
      <span>{what}</span>
    </div>
  {/each}
  <div class="micro second">On a symbol</div>
  {#each SYMBOL as [keys, what] (what)}
    <div class="row">
      <span class="keys">{#each keys as key (key)}<span class="kbd">{key}</span>{/each}</span>
      <span>{what}</span>
    </div>
  {/each}
</div>

<style>
  .sheet {
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: var(--fg-2);
    font: var(--t-small);
  }

  .second {
    margin-top: 8px;
  }

  .row {
    display: grid;
    grid-template-columns: 64px minmax(0, 1fr);
    align-items: center;
    gap: 10px;
  }

  .keys {
    display: inline-flex;
    gap: 4px;
  }
</style>
