<script context="module">
export const moduleValue = setupServer();
</script>
<script lang="ts">
import Child from './Child.svelte';
export function handleClick() { return saveItem(); }
const count = $state(0);
</script>

<button on:click={handleClick()} class={cn('x')}>
  <Child />
  <Widget value={formatValue(count)} />
</button>
