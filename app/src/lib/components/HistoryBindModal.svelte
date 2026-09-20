<script lang="ts">
  // 绑定历史会话: pick an existing adapter session and bind it to the current context.
  // 表格形式：可按 工作目录/时间 排序（默认时间倒序），搜索覆盖标题/ID/目录。
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
  let sortKey = $state<"time" | "cwd" | "title">("time");
  let sortAsc = $state(false);

  $effect(() => {
    if (!app.historyBind) return;
    loading = true;
    const a = app.historyBind;
    api
      .acpSessionsList(a)
      .then((list) => {
        // 适配器若未按时间排序，这里兜底倒序
        sessions = [...list].sort((x, y) => (y.updated_at ?? "").localeCompare(x.updated_at ?? ""));
      })
      .catch((e) => {
        toast("error", String(e));
        sessions = [];
      })
      .finally(() => (loading = false));
  });

  const filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    const rows = sessions.filter(
      (s) =>
        !q ||
        (s.title ?? "").toLowerCase().includes(q) ||
        s.session_id.toLowerCase().includes(q) ||
        (s.cwd ?? "").toLowerCase().includes(q),
    );
    const dir = sortAsc ? 1 : -1;
    return [...rows].sort((x, y) => {
      if (sortKey === "time") return dir * (x.updated_at ?? "").localeCompare(y.updated_at ?? "");
      if (sortKey === "cwd") return dir * (x.cwd ?? "").localeCompare(y.cwd ?? "", "zh");
      return dir * (x.title ?? "").localeCompare(y.title ?? "", "zh");
    });
  });

  function setSort(k: "time" | "cwd" | "title") {
    if (sortKey === k) sortAsc = !sortAsc;
    else {
      sortKey = k;
      sortAsc = k !== "time"; // 时间默认倒序，其余默认升序
    }
  }

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
        <span>绑定历史会话 — {agent === "codex" ? "Codex" : "ZCode"}（{ctx?.name ?? ""}）· 共 {sessions.length} 条</span>
        <button class="btn ghost sm" onclick={() => (app.historyBind = null)}>✕</button>
      </header>
      <div class="body">
        <input class="search" placeholder="按标题、会话 ID 或工作目录搜索…" bind:value={query} />
        {#if loading}
          <p class="none">正在从适配器读取会话列表…</p>
        {:else}
          <div class="tbl-wrap">
            <table class="tbl">
              <thead>
                <tr>
                  <th class="sortable" onclick={() => setSort("title")}>标题 {sortKey === "title" ? (sortAsc ? "↑" : "↓") : ""}</th>
                  <th class="sortable" onclick={() => setSort("cwd")}>工作目录 {sortKey === "cwd" ? (sortAsc ? "↑" : "↓") : ""}</th>
                  <th class="sortable" onclick={() => setSort("time")}>时间 {sortKey === "time" ? (sortAsc ? "↑" : "↓") : ""}</th>
                  <th class="op-col">操作</th>
                </tr>
              </thead>
              <tbody>
                {#each filtered as s (s.session_id)}
                  <tr>
                    <td class="t-title" title={s.session_id}>{s.title || s.session_id.slice(0, 8) + "…"}</td>
                    <td class="t-cwd" title={s.cwd || ""}>{s.cwd || "—"}</td>
                    <td class="t-time">{s.updated_at || "—"}</td>
                    <td class="op-col">
                      <button class="btn sm primary" disabled={binding !== null} onclick={() => bind(s)}>
                        {#if binding === s.session_id}<span class="spin">◠</span> 绑定中…{:else}绑定{/if}
                      </button>
                    </td>
                  </tr>
                {:else}
                  <tr><td colspan="4" class="none">{query ? "没有匹配的会话" : "适配器没有返回历史会话"}</td></tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </div>
      <p class="tip"><Icon name="link" size={12} /> 绑定后，使驾会通过 session/load 恢复该会话；如需共享上下文，请复制「接入提示词」发送给 Agent。</p>
    </div>
  </div>
{/if}

<style>
  .modal.wide {
    min-width: 980px;
    max-width: 94vw;
    max-height: 80vh;
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
  .tbl-wrap {
    overflow: auto;
    max-height: 52vh;
  }
  .tbl {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.88em;
  }
  .tbl th,
  .tbl td {
    text-align: left;
    padding: 7px 10px;
    border-bottom: 1px solid var(--border-soft);
    vertical-align: top;
  }
  .tbl thead th {
    position: sticky;
    top: 0;
    background: var(--bg-panel);
    color: var(--text-dim);
    font-weight: 600;
    white-space: nowrap;
    z-index: 1;
  }
  th.sortable {
    cursor: pointer;
    user-select: none;
  }
  th.sortable:hover {
    color: var(--accent);
  }
  .t-title {
    font-weight: 600;
    max-width: 260px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .t-cwd {
    color: var(--text-faint);
    font-family: var(--mono);
    font-size: 0.92em;
    max-width: 300px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    user-select: text;
  }
  .t-time {
    white-space: nowrap;
    color: var(--text-dim);
  }
  .op-col {
    text-align: right;
    white-space: nowrap;
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
