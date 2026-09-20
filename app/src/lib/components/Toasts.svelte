<script lang="ts">
  import { app } from "../state.svelte";
</script>

<div class="toasts">
  {#each app.toasts as t (t.id)}
    <div class="toast {t.kind}">{t.text}</div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: 18px;
    bottom: 18px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    z-index: 300;
    max-width: 420px;
  }
  .toast {
    background: var(--bg-panel);
    border: 1px solid var(--border);
    border-left: 3px solid var(--accent);
    border-radius: 8px;
    padding: 10px 14px;
    box-shadow: var(--shadow);
    animation: slide 0.2s ease;
    font-size: 0.92em;
  }
  .toast.ok {
    border-left-color: var(--ok);
  }
  .toast.warn {
    border-left-color: var(--warn);
  }
  .toast.error {
    border-left-color: var(--danger);
  }
  @keyframes slide {
    from {
      transform: translateY(8px);
      opacity: 0;
    }
  }
</style>
