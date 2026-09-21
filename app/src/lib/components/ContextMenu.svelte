<script lang="ts">
  import type { MenuItem } from "./menu-item";
  import { t } from "../i18n";

  let { x, y, items, onclose }: { x: number; y: number; items: MenuItem[]; onclose: () => void } = $props();
  let element: HTMLDivElement;
  let left = $state(0);
  let top = $state(0);

  // Fixed menus must escape transformed/scrolling parents and skin stacking contexts.
  function portal(node: HTMLDivElement) {
    document.body.appendChild(node);
    const activate = (event: MouseEvent) => {
      const button = (event.target as Element).closest<HTMLButtonElement>("button[data-menu-index]");
      if (button) { event.stopPropagation(); onClick(items[Number(button.dataset.menuIndex)]); }
    };
    const context = (event: MouseEvent) => { event.preventDefault(); event.stopPropagation(); };
    node.addEventListener("click", activate);
    node.addEventListener("contextmenu", context);
    return { destroy() { node.removeEventListener("click", activate); node.removeEventListener("contextmenu", context); node.remove(); } };
  }

  $effect(() => {
    const bounds = element?.getBoundingClientRect();
    left = Math.max(8, Math.min(x, window.innerWidth - (bounds?.width ?? 220) - 8));
    top = Math.max(8, Math.min(y, window.innerHeight - (bounds?.height ?? 200) - 8));
  });

  function onClick(item: MenuItem) {
    onclose();
    if (item !== "sep") item.run?.();
  }

  $effect(() => {
    const outside = (event: PointerEvent) => {
      if (!element.contains(event.target as Node)) onclose();
    };
    const keydown = (event: KeyboardEvent) => {
      if (event.key === "Escape") { event.preventDefault(); onclose(); }
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        const buttons = Array.from(element.querySelectorAll("button"));
        const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
        const next = (index + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
        buttons[next]?.focus();
        event.preventDefault();
      }
    };
    // Use the next pointer-down, not the opening contextmenu event bubbling to window.
    document.addEventListener("pointerdown", outside, true);
    window.addEventListener("keydown", keydown);
    return () => {
      document.removeEventListener("pointerdown", outside, true);
      window.removeEventListener("keydown", keydown);
      window.removeEventListener("resize", onclose);
    };
  });
</script>

<div class="ctx-menu" role="menu" aria-label={t("操作菜单")} bind:this={element} use:portal style:left="{left}px" style:top="{top}px" oncontextmenu={(e) => { e.preventDefault(); e.stopPropagation(); }}>
  {#each items as item, index}
    {#if item === "sep"}
      <hr role="separator" />
    {:else}
      <button role="menuitem" data-menu-index={index} class={item.danger ? "danger" : ""}>{t(item.label)}</button>
    {/if}
  {/each}
</div>

<style>
  .ctx-menu { position: fixed; z-index: 10000; max-width: calc(100vw - 16px); max-height: calc(100vh - 16px); overflow: auto; user-select: none; }
</style>
