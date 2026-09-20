<script lang="ts">
  import type { MenuItem } from "./menu-item";

  let { x, y, items, onclose }: { x: number; y: number; items: MenuItem[]; onclose: () => void } = $props();

  const style = $derived(`left:${Math.min(x, window.innerWidth - 190)}px; top:${Math.min(y, window.innerHeight - 40 - items.length * 34)}px`);

  function onClick(item: MenuItem) {
    onclose();
    if (item !== "sep") item.run?.();
  }

  function onWindowClick() {
    onclose();
  }

  $effect(() => {
    window.addEventListener("click", onWindowClick, { once: true });
    window.addEventListener("contextmenu", onWindowClick, { once: true });
    return () => {
      window.removeEventListener("click", onWindowClick);
      window.removeEventListener("contextmenu", onWindowClick);
    };
  });
</script>

<div class="ctx-menu" {style}>
  {#each items as item, i}
    {#if item === "sep"}
      <hr />
    {:else}
      <button class={item.danger ? "danger" : ""} onclick={() => onClick(item)}>{item.label}</button>
    {/if}
  {/each}
</div>
