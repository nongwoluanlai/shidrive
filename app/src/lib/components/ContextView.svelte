<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { app, currentContext, toast } from "../state.svelte";
  import { api } from "../ipc";
  import type { ContextEntry, ScCommit } from "../types";

  const ctx = $derived(currentContext());
  let entries = $state<ContextEntry[]>([]);
  let overview = $state("");
  let constraints = $state("");
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  const inputs = $state<Record<string, string>>({ todo: "", progress: "", decision: "", note: "" });
  let commits = $state<ScCommit[]>([]);
  let openCommit = $state<number | null>(null);

  async function refreshCommits() {
    if (!ctx) {
      commits = [];
      return;
    }
    try {
      commits = await api.contextCommits(ctx.id);
    } catch (e) {
      toast("error", "加载提交历史失败: " + String(e));
    }
  }

  function parseFiles(raw: string): string[] {
    try {
      return JSON.parse(raw) ?? [];
    } catch {
      return [];
    }
  }

  interface Snap {
    overview?: string;
    todos?: { content: string; status?: string }[];
    progress?: { content: string }[];
    notes?: { content: string }[];
    constraints?: string;
  }
  function parseSnap(raw: string): Snap {
    try {
      return JSON.parse(raw) ?? {};
    } catch {
      return {};
    }
  }

  // load when context changes
  $effect(() => {
    const c = ctx;
    if (!c) return;
    overview = c.overview;
    constraints = c.constraints;
    openCommit = null;
    void refresh();
    void refreshCommits();
  });

  let unlistenCommit: (() => void) | null = null;
  onMount(() => {
    // Agent 通过 MCP 提交后实时刷新条目与提交历史
    void listen<{ contextId: string }>("sc://commit", (e) => {
      void refresh();
      void refreshCommits();
    }).then((f) => (unlistenCommit = f));
    return () => {
      unlistenCommit?.();
    };
  });

  async function refresh() {
    if (!ctx) return;
    try {
      entries = await api.entriesList(ctx.id);
    } catch (e) {
      toast("error", String(e));
    }
  }

  function queueSave() {
    const target = ctx;
    if (!target) return;
    if (saveTimer) clearTimeout(saveTimer);
    // snapshot at queue time: the timer must never write this context's
    // edits into whichever context is selected when it fires
    const snap = { id: target.id, name: target.name, overview, constraints };
    saveTimer = setTimeout(async () => {
      try {
        await api.contextsUpdate(snap.id, snap.name, snap.overview, snap.constraints);
      } catch (e) {
        toast("error", String(e));
      }
    }, 600);
  }

  async function add(kind: keyof typeof inputs) {
    if (!ctx) return;
    const content = inputs[kind].trim();
    if (!content) return;
    try {
      await api.entryAdd(ctx.id, kind, content);
      inputs[kind] = "";
      await refresh();
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function toggle(e: ContextEntry) {
    try {
      await api.entryUpdate(e.id, e.content, e.status === "done" ? "open" : "done");
      await refresh();
    } catch (err) {
      toast("error", String(err));
    }
  }

  async function remove(e: ContextEntry) {
    try {
      await api.entryDelete(e.id);
      await refresh();
    } catch (err) {
      toast("error", String(err));
    }
  }

  const of = (kind: string) => entries.filter((e) => e.kind === kind);
</script>

<div class="ctxview">
  {#if ctx}
    <div class="col">
      <section class="card">
        <h3>🧭 概述 Overview</h3>
        <textarea
          rows="4"
          bind:value={overview}
          oninput={queueSave}
          placeholder="这个上下文要做什么？当前目标、范围、背景…（自动保存）"
        ></textarea>
      </section>

      <section class="card">
        <h3>✅ 待办 Todo</h3>
        <div class="addrow">
          <input
            placeholder="添加待办，回车确认"
            bind:value={inputs.todo}
            onkeydown={(e) => e.key === "Enter" && add("todo")}
          />
          <button class="btn sm" onclick={() => add("todo")}>添加</button>
        </div>
        <ul class="todos">
          {#each of("todo") as e (e.id)}
            <li class:done={e.status === "done"}>
              <input type="checkbox" checked={e.status === "done"} onchange={() => toggle(e)} />
              <span class="content">{e.content}</span>
              <button class="btn ghost sm" onclick={() => remove(e)}>✕</button>
            </li>
          {:else}
            <li class="none">暂无待办</li>
          {/each}
        </ul>
      </section>

      <section class="card">
        <h3>📈 进展 Progress</h3>
        <div class="addrow">
          <input placeholder="记录阶段性进展，回车确认" bind:value={inputs.progress} onkeydown={(e) => e.key === "Enter" && add("progress")} />
          <button class="btn sm" onclick={() => add("progress")}>添加</button>
        </div>
        <ul>
          {#each of("progress") as e (e.id)}
            <li><span class="content">{e.content}</span><span class="time">{e.created_at.slice(5, 16)}</span><button class="btn ghost sm" onclick={() => remove(e)}>✕</button></li>
          {:else}
            <li class="none">暂无进展记录</li>
          {/each}
        </ul>
      </section>

      <div class="pair">
        <section class="card">
          <h3>⚖️ 决策 Decisions</h3>
          <div class="addrow">
            <input placeholder="记录重要决策" bind:value={inputs.decision} onkeydown={(e) => e.key === "Enter" && add("decision")} />
            <button class="btn sm" onclick={() => add("decision")}>添加</button>
          </div>
          <ul>
            {#each of("decision") as e (e.id)}
              <li><span class="content">{e.content}</span><button class="btn ghost sm" onclick={() => remove(e)}>✕</button></li>
            {:else}
              <li class="none">暂无决策记录</li>
            {/each}
          </ul>
        </section>
        <section class="card">
          <h3>📝 注意 Notes</h3>
          <div class="addrow">
            <input placeholder="记录注意事项" bind:value={inputs.note} onkeydown={(e) => e.key === "Enter" && add("note")} />
            <button class="btn sm" onclick={() => add("note")}>添加</button>
          </div>
          <ul>
            {#each of("note") as e (e.id)}
              <li><span class="content">{e.content}</span><button class="btn ghost sm" onclick={() => remove(e)}>✕</button></li>
            {:else}
              <li class="none">暂无注意事项</li>
            {/each}
          </ul>
        </section>
      </div>

      <section class="card commits">
        <h3>🕘 提交历史 <span class="ver">当前 v{commits[0]?.seq ?? 0}</span><span class="spacer"></span><button class="btn ghost sm" title="刷新" onclick={refreshCommits}>⟳</button></h3>
        <div class="commit-list">
          {#each commits as c (c.seq)}
            <div class="commit">
              <button class="chead" onclick={() => (openCommit = openCommit === c.seq ? null : c.seq)}>
                <span class="badge accent">v{c.seq}</span>
                <span class="cagent">{c.agent_type || "agent"}</span>
                <span class="csummary">{c.summary}</span>
                <span class="ctime">{c.created_at.slice(5, 16)}</span>
              </button>
              {#if openCommit === c.seq}
                {@const snap = parseSnap(c.snapshot)}
                <div class="cbody">
                  {#if parseFiles(c.files).length}
                    <div class="cfiles">涉及文件：{#each parseFiles(c.files) as f, i (f + i)}{#if i > 0}、{/if}<code>{f}</code>{/each}</div>
                  {/if}
                  {#if snap.overview}<div class="csec"><b>概述</b><div class="ctext">{snap.overview}</div></div>{/if}
                  {#if snap.todos?.length}<div class="csec"><b>待办</b><ul>{#each snap.todos as t (t.content)}<li>{t.content}{t.status === "done" ? " ✓" : ""}</li>{/each}</ul></div>{/if}
                  {#if snap.progress?.length}<div class="csec"><b>进展</b><ul>{#each snap.progress as t (t.content)}<li>{t.content}</li>{/each}</ul></div>{/if}
                  {#if snap.notes?.length}<div class="csec"><b>注意</b><ul>{#each snap.notes as t (t.content)}<li>{t.content}</li>{/each}</ul></div>{/if}
                  {#if snap.constraints}<div class="csec"><b>约束</b><div class="ctext">{snap.constraints}</div></div>{/if}
                </div>
              {/if}
            </div>
          {:else}
            <p class="none">还没有提交记录（Agent 通过 MCP 提交后会出现在这里）</p>
          {/each}
        </div>
      </section>

      <section class="card">
        <h3>⛔ 约束 Constraints</h3>
        <textarea
          rows="3"
          bind:value={constraints}
          oninput={queueSave}
          placeholder="项目约束：技术栈限定、不能动的部分、性能要求…（自动保存）"
        ></textarea>
      </section>
    </div>
  {:else}
    <div class="empty">选择一个上下文</div>
  {/if}
</div>

<style>
  .ctxview {
    flex: 1;
    overflow-y: auto;
    padding: 18px 22px;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 14px;
    max-width: 980px;
    margin: 0 auto;
  }
  .pair {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 14px;
  }
  .card {
    background: var(--bg-panel);
    border: 1px solid var(--border-soft);
    border-radius: var(--radius);
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .card h3 {
    font-size: 0.95em;
    color: var(--text-dim);
    font-weight: 600;
  }
  .card textarea {
    background: var(--bg-elev);
  }
  .addrow {
    display: flex;
    gap: 8px;
  }
  .addrow input {
    flex: 1;
  }
  ul {
    list-style: none;
    display: flex;
    flex-direction: column;
  }
  li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 5px 2px;
    border-bottom: 1px solid var(--border-soft);
    font-size: 0.93em;
  }
  li:last-child {
    border-bottom: none;
  }
  li .content {
    flex: 1;
    user-select: text;
    overflow-wrap: anywhere;
  }
  li .time {
    color: var(--text-faint);
    font-size: 0.85em;
  }
  li.done .content {
    text-decoration: line-through;
    color: var(--text-faint);
  }
  li.none {
    color: var(--text-faint);
    justify-content: center;
    padding: 8px;
  }
  .commits h3 {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .commits .ver {
    color: var(--accent);
    font-size: 0.9em;
  }
  .commits .spacer {
    flex: 1;
  }
  .commit-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
    max-height: 420px;
    overflow-y: auto;
  }
  .commit {
    border: 1px solid var(--border-soft);
    border-radius: 8px;
    overflow: hidden;
  }
  .chead {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 10px;
    text-align: left;
  }
  .chead:hover {
    background: var(--bg-elev);
  }
  .cagent {
    color: var(--text-faint);
    font-size: 0.82em;
    flex: none;
  }
  .csummary {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text);
    font-size: 0.9em;
  }
  .ctime {
    color: var(--text-faint);
    font-size: 0.8em;
    flex: none;
  }
  .cbody {
    border-top: 1px solid var(--border-soft);
    padding: 8px 12px;
    font-size: 0.86em;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .cfiles code {
    background: var(--code-bg);
    padding: 1px 5px;
    border-radius: 4px;
    font-size: 0.92em;
  }
  .csec b {
    color: var(--text-dim);
    font-weight: 600;
    margin-right: 6px;
  }
  .csec ul {
    margin: 2px 0 0 1.2em;
  }
  .ctext {
    color: var(--text-dim);
    white-space: pre-wrap;
    margin-top: 2px;
    user-select: text;
  }
</style>
