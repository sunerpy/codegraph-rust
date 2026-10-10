<script>
  import { goto } from '$app/navigation';
  function about() {
    goto('/about');
  }
</script>
<button on:click={about}>About</button>
