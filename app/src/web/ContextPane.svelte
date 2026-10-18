<script lang="ts">
  import { api, type Commit, type Context, type ContextEntry } from "./api";
  import { ui, toast, errText } from "./store.svelte";
  import { md } from "../lib/markdown";
  import Icon from "./Icon.svelte";

  let data = $state<{ context: Context; entries: ContextEntry[]; commits: Commit[] } | null>(null);
  let loading = $state(false);
  let showDone = $state(false);
  let seq = 0;

  const KINDS: [string, string][] = [["todo", "待办"], ["progress", "进展"], ["decision", "决策"], ["note", "备注"]];
  const isDone = (s: string) => /^(done|closed|resolved|completed|cancel(l)?ed)$/i.test(s || "");

  async function load() {
    const id = ui.contextId;
    const my = ++seq;
    if (!id) { data = null; return; }
    loading = true;
    try {
      const r = await api.context(id);
      if (my === seq) data = r;
    } catch (e) {
      if (my === seq) { data = null; toast("error", errText(e)); }
    } finally {
      if (my === seq) loading = false;
    }
  }

  $effect(() => { void ui.contextId; void load(); });

  const groups = $derived.by(() => {
    const entries = data?.entries ?? [];
    const known = new Set(KINDS.map(([k]) => k));
    const out = KINDS.map(([k, label]) => ({ kind: k, label, items: entries.filter((e) => e.kind === k) }));
    const other = entries.filter((e) => !known.has(e.kind));
    if (other.length) out.push({ kind: "other", label: "其它", items: other });
    return out
      .map((g) => ({ ...g, total: g.items.length, items: showDone ? g.items : g.items.filter((e) => !isDone(e.status)) }))
      .filter((g) => g.total > 0);
  });

  const hiddenCount = $derived((data?.entries ?? []).filter((e) => isDone(e.status)).length);
  const fileCount = (f: unknown) => (Array.isArray(f) ? f.length : 0);
  const shortTime = (t: string) => (t || "").replace("T", " ").slice(0, 16);
</script>

<div class="pane scroll">
  {#if !ui.contextId}
    <div class="empty"><Icon name="book" size={28} />当前项目还没有上下文</div>
  {:else if !data && loading}
    <div class="empty"><span class="spin"></span>加载中…</div>
  {:else if data}
    <header class="head">
      <div class="title">
        <h2>{data.context.name}</h2>
        <span class="faint small">更新于 {shortTime(data.context.updated_at)}</span>
      </div>
      <button class="icon-btn" title="刷新" onclick={load} disabled={loading}>
        {#if loading}<span class="spin"></span>{:else}<Icon name="refresh" />{/if}
      </button>
    </header>

    {#if data.context.overview?.trim()}
      <section class="card sec">
        <h3>概述</h3>
        <div class="md">{@html md(data.context.overview)}</div>
      </section>
    {/if}
    {#if data.context.constraints?.trim()}
      <section class="card sec">
        <h3><Icon name="shield" size={15} /> 约束</h3>
        <div class="md">{@html md(data.context.constraints)}</div>
      </section>
    {/if}

    <section class="card sec">
      <div class="row">
        <h3>条目</h3>
        {#if hiddenCount}
          <label class="toggle"><input type="checkbox" bind:checked={showDone} /> 显示已完成（{hiddenCount}）</label>
        {/if}
      </div>
      {#if !groups.length}
        <div class="faint small">暂无条目</div>
      {/if}
      {#each groups as g (g.kind)}
        <div class="group">
          <div class="glabel">{g.label} <span class="faint">{g.total}</span></div>
          {#each g.items as e (e.id)}
            <div class="entry" class:done={isDone(e.status)}>
              <div class="emeta">
                {#if e.status}<span class="badge" class:ok={isDone(e.status)} class:run={!isDone(e.status)}>{e.status}</span>{/if}
                <span class="faint small">{shortTime(e.updated_at)}</span>
              </div>
              <div class="md">{@html md(e.content)}</div>
            </div>
          {/each}
        </div>
      {/each}
    </section>

    {#if data.commits.length}
      <section class="card sec">
        <h3><Icon name="clock" size={15} /> 最近提交</h3>
        <ul class="commits">
          {#each data.commits as c (c.seq)}
            <li>
              <div class="cmeta">
                <span class="mono faint">#{c.seq}</span>
                <span class="badge">{c.agent}</span>
                <span class="faint small">{shortTime(c.time)}</span>
                {#if fileCount(c.files)}<span class="faint small">{fileCount(c.files)} 个文件</span>{/if}
              </div>
              <div class="csum">{c.summary}</div>
            </li>
          {/each}
        </ul>
      </section>
    {/if}
  {/if}
</div>

<style>
  .pane { height: 100%; padding: 16px; display: flex; flex-direction: column; gap: 12px; max-width: 920px; margin: 0 auto; width: 100%; }
  .head { display: flex; align-items: center; gap: 10px; }
  .title { flex: 1; min-width: 0; }
  .title h2 { margin: 0; font-size: 19px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .small { font-size: 12px; }
  .sec { padding: 14px 16px; }
  .sec h3 { margin: 0 0 8px; font-size: 14px; color: var(--text-dim); display: flex; align-items: center; gap: 6px; }
  .row { display: flex; align-items: center; justify-content: space-between; gap: 8px; flex-wrap: wrap; }
  .row h3 { margin: 0; }
  .toggle { font-size: 13px; color: var(--text-dim); display: flex; align-items: center; gap: 6px; cursor: pointer; }
  .group { margin-top: 12px; }
  .glabel { font-size: 13px; font-weight: 600; margin-bottom: 6px; }
  .entry { border-left: 2px solid var(--border); padding: 4px 0 4px 10px; margin-bottom: 8px; }
  .entry.done { opacity: .6; }
  .emeta { display: flex; align-items: center; gap: 8px; margin-bottom: 2px; }
  .entry :global(.md > :first-child) { margin-top: 0; }
  .entry :global(.md > :last-child) { margin-bottom: 0; }
  .commits { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 10px; }
  .cmeta { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .csum { margin-top: 2px; white-space: pre-wrap; word-break: break-word; font-size: 14px; }
</style>
