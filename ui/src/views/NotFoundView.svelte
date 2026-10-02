<script lang="ts">
  interface Props {
    path: string;
  }
  let { path }: Props = $props();
</script>

<div class="scroll island">
  <div class="emptystate">
    <h2>No such view</h2>
    <p>
      <span class="mono">#{path}</span> does not match a view. Try
      <a class="mono link" href="#/">the start</a>.
    </p>
  </div>
</div>

<style>
  .scroll {
    height: 100%;
    overflow: auto;
  }

  .link {
    color: var(--primary-ink);
    text-decoration: underline;
    text-decoration-color: var(--primary-line);
    text-underline-offset: 3px;
  }
</style>
