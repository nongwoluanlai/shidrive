<script lang="ts">
  // 绑定历史会话: pick an existing adapter session and bind it to the current context.
  import { app, currentContext, toast, setChatRows, chatKey } from "../state.svelte";
  import { api } from "../ipc";
  import Icon from "./Icon.svelte";
  import type { AgentType, Context, SessionInfo } from "../types";

  const agent = $derived(app.historyBind);
  const ctx = $derived(currentContext());
  let sessions = $state<SessionInfo[]>([]);
  let loading = $state(true);
  let query = $state("");
  let binding = $state<string | null>(null);

  $effect(() => {
    if (!app.historyBind) return;
    loading = true;
    const a = app.historyBind;
    api
      .acpSessionsList(a)
      .then((list) => (sessions = list))
      .catch((e) => {
        toast("error", String(e));
        sessions = [];
      })
      .finally(() => (loading = false));
  });

  const filtered = $derived(
    sessions.filter(
      (s) =>
        !query.trim() ||
        (s.title ?? "").toLowerCase().includes(query.toLowerCase()) ||
        s.session_id.includes(query.trim()),
    ),
  );

  async function bind(s: SessionInfo) {
    if (!ctx || !agent || binding) return;
    const a: string = agent; // capture before clearing historyBind (derived becomes null)
    const c: Context = ctx;
    binding = s.session_id;
    try {
      const rows = await api.acpSessionBind(c, a, s.session_id, s.title ?? undefined);
      setChatRows(chatKey(c.id, a), rows);
      app.historyBind = null;
      toast("ok", rows.length ? `已绑定并加载会话历史（${rows.length} 条）` : "已绑定会话（该会话暂无历史记录）");
    } catch (e) {
      toast("error", `绑定失败: ${e}`);
    } finally {
      binding = null;
    }
  }
</script>

{#if agent}
  <div class="modal-backdrop">
    <div class="modal wide">
      <header>
        <span>绑定历史会话 — {agent === "codex" ? "Codex" : "ZCode"}（{ctx?.name ?? ""}）</span>
        <button class="btn ghost sm" onclick={() => (app.historyBind = null)}>✕</button>
      </header>
      <div class="body">
        <input class="search" placeholder="按标题或会话 ID 搜索…" bind:value={query} />
        {#if loading}
          <p class="none">正在从适配器读取会话列表…</p>
        {:else}
          <div class="list">
            {#each filtered as s (s.session_id)}
              <div class="row">
                <div class="info">
                  <div class="l1">{s.title || s.session_id.slice(0, 8) + "…"}</div>
                  <div class="l2 dim">{s.session_id}{s.cwd ? " · " + s.cwd : ""}{s.updated_at ? " · " + s.updated_at : ""}</div>
                </div>
                <button class="btn sm primary" disabled={binding !== null} onclick={() => bind(s)}>
                  {#if binding === s.session_id}<span class="spin">◠</span> 绑定中…{:else}绑定{/if}
                </button>
              </div>
            {:else}
              <p class="none">{loading ? "" : query ? "没有匹配的会话" : "适配器没有返回历史会话"}</p>
            {/each}
          </div>
        {/if}
      </div>
      <p class="tip"><Icon name="link" size={12} /> 绑定后，使驾会通过 session/load 恢复该会话；如需共享上下文，请复制「接入提示词」发送给 Agent。</p>
    </div>
  </div>
{/if}

<style>
  .modal.wide {
    min-width: 640px;
    max-height: 78vh;
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .search {
    width: 100%;
  }
  .spin {
    display: inline-block;
    animation: spin 1s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }
  .list {
    overflow: auto;
    max-height: 46vh;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border: 1px solid var(--border-soft);
    border-radius: 8px;
  }
  .info {
    flex: 1;
    min-width: 0;
    font-size: 0.9em;
  }
  .l1 {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .l2 {
    font-size: 0.8em;
    margin-top: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dim {
    color: var(--text-faint);
  }
  .none {
    color: var(--text-faint);
    text-align: center;
    padding: 18px;
    font-size: 0.9em;
  }
  .tip {
    padding: 8px 18px 12px;
    color: var(--text-faint);
    font-size: 0.8em;
    border-top: 1px solid var(--border-soft);
    display: flex;
    gap: 6px;
    align-items: center;
  }
</style>
