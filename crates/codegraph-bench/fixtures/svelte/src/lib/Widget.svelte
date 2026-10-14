<script context="module">
  export function shared() {
    return 'shared';
  }
</script>

<script>
  import { doSetup, formatLabel } from './util';

  doSetup();
  let label = formatLabel(' widget ');

  function click() {
    return doSetup();
  }
</script>

<button on:click={click}>{label}</button>
