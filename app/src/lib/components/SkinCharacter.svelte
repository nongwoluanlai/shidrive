<script lang="ts">
  import { app } from "../state.svelte";
</script>

{#if app.skin}
  <div class="skin-character-stage" aria-hidden="true">
    <div class="skin-character" role="presentation"></div>
  </div>
{/if}
