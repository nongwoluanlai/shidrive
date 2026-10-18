<script lang="ts">
  import { ui, answerPermission } from "./store.svelte";
  import Icon from "./Icon.svelte";

  const req = $derived(ui.permissions[0] ?? null);
  const agentName = $derived(req ? ui.agents.find((a) => a.id === req.agentType)?.name ?? req.agentType : "");
  let busy = $state(false);

  function preview(v: unknown): string {
    if (v == null) return "";
    let s = typeof v === "string" ? v : JSON.stringify(v, null, 2);
    if (s.length > 4000) s = s.slice(0, 4000) + "\n…";
    return s;
  }
  const raw = $derived(preview(req?.params?.toolCall?.rawInput));

  async function choose(optionId: string) {
    if (!req || busy) return;
    busy = true;
    try { await answerPermission(req, optionId); } finally { busy = false; }
  }
  const cls = (kind: string) => (kind?.startsWith("allow") ? "primary" : kind?.startsWith("reject") ? "danger" : "");
</script>

{#if req}
  <div class="backdrop">
    <div class="sheet card" role="dialog" aria-modal="true" aria-label="权限请求">
      <div class="head">
        <Icon name="shield" size={20} />
        <div class="ht">
          <div class="t">{agentName} 请求权限</div>
          {#if ui.permissions.length > 1}<div class="faint small">还有 {ui.permissions.length - 1} 个待处理</div>{/if}
        </div>
      </div>
      <div class="body">
        <div class="tool">
          {#if req.params?.toolCall?.kind}<span class="badge">{req.params.toolCall.kind}</span>{/if}
          <span class="tt">{req.params?.toolCall?.title || "工具调用"}</span>
        </div>
        {#if raw}<pre class="raw mono scroll">{raw}</pre>{/if}
      </div>
      <div class="opts">
        {#each req.params?.options ?? [] as o (o.optionId)}
          <button class="btn {cls(o.kind)}" disabled={busy} onclick={() => choose(o.optionId)}>{o.name}</button>
        {/each}
      </div>
    </div>
  </div>
{/if}

<style>
  .backdrop { position: fixed; inset: 0; z-index: 60; background: rgba(0, 0, 0, .45); display: flex; align-items: center; justify-content: center; padding: 16px; }
  .sheet { width: min(560px, 100%); max-height: min(80vh, 640px); display: flex; flex-direction: column; box-shadow: var(--shadow); background: var(--bg-panel); }
  .head { display: flex; align-items: center; gap: 10px; padding: 14px 16px 8px; color: var(--warn); }
  .ht { color: var(--text); min-width: 0; }
  .t { font-weight: 600; }
  .small { font-size: 12px; }
  .body { padding: 4px 16px; min-height: 0; display: flex; flex-direction: column; gap: 8px; overflow: hidden; }
  .tool { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .tt { word-break: break-word; }
  .raw { margin: 0; padding: 10px; background: var(--code-bg); border: 1px solid var(--border-soft); border-radius: 8px; font-size: 12.5px; white-space: pre-wrap; word-break: break-all; max-height: 40vh; }
  .opts { display: flex; flex-wrap: wrap; gap: 8px; justify-content: flex-end; padding: 12px 16px 14px; }
  @media (max-width: 640px) {
    .backdrop { align-items: flex-end; padding: 0; }
    .sheet { width: 100%; border-radius: 16px 16px 0 0; max-height: 85vh; padding-bottom: var(--safe-b); }
    .opts { flex-direction: column-reverse; }
    .opts .btn { width: 100%; }
  }
</style>
