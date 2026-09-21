<script lang="ts">
  import { t } from "../i18n";
  // 全局对话框宿主：confirm / prompt，Enter 确认，Esc 取消。
  import { dialogBox, settleDialog } from "../dialog.svelte";

  const d = $derived(dialogBox.current);
  let inputEl: HTMLInputElement | undefined = $state();

  $effect(() => {
    if (d?.kind === "prompt") {
      setTimeout(() => {
        inputEl?.focus();
        inputEl?.select();
      }, 30);
    }
  });

  function cancel() {
    settleDialog(d?.kind === "prompt" ? null : false);
  }

  function confirm() {
    if (!d) return;
    settleDialog(d.kind === "prompt" ? d.value.trim() : true);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      confirm();
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancel();
    }
  }
</script>

{#if d}
  <div class="modal-backdrop">
    <div class="modal" style="min-width: 420px" role="dialog" aria-label={d.title} onkeydown={onKeydown}>
      <header>{d.title}</header>
      <div class="body">
        {#if d.message}
          <p class="msg">{d.message}</p>
        {/if}
        {#if d.kind === "prompt"}
          <div class="field">
            <label>{d.label}</label>
            <input bind:this={inputEl} bind:value={d.value} placeholder={d.label} />
          </div>
        {/if}
      </div>
      <footer>
        <button class="btn" onclick={cancel}>{t("取消")}</button>
        <button class="btn {d.danger ? 'danger' : 'primary'}" onclick={confirm}>{t(d.confirmText)}</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .msg {
    user-select: text;
    line-height: 1.6;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .field label {
    color: var(--text-dim);
    font-size: 0.9em;
  }
</style>
