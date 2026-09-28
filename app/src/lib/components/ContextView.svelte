<script lang="ts">
  import { t, localeTag } from "../i18n";
  import { onMount, tick, untrack } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { app, currentContext, refreshContexts, toast } from "../state.svelte";
  import { api } from "../ipc";
  import { promptDialog } from "../dialog.svelte";
  import type { ContextEntry, ScCommit } from "../types";

  const ctx = $derived(currentContext());
  // 原始值派生：列表刷新换了对象引用但 id 未变时，下面按 id 触发的 effect 不会重跑
  const ctxId = $derived(ctx?.id ?? null);
  let entries = $state<ContextEntry[]>([]);
  let overview = $state("");
  let constraints = $state("");
  // 概述/约束的本地修改尚未落库时为 true：此时不允许外部提交（Agent 经 MCP）覆盖正在编辑的文本
  let dirty = $state(false);
  let editSeq = 0;
  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  let pendingSave: { id: string; overview: string; constraints: string; seq: number } | null = null;

  const inputs = $state<Record<string, string>>({ todo: "", progress: "", decision: "", note: "" });
  let commits = $state<ScCommit[]>([]);
  let openCommit = $state<number | null>(null);
  /** 导出时拉取的提交上限：视为「全部」（后端不裁剪提交历史） */
  const EXPORT_ALL_LIMIT = 100000;
  // 各分区列表的滚动容器：新增条目追加在末尾，添加后把对应列表滚到底
  const listEls: Record<string, HTMLUListElement | undefined> = $state({});

  async function refreshCommits(id: string) {
    try {
      const list = await api.contextCommits(id);
      if (ctx?.id === id) commits = list; // 切换后才返回的旧结果直接丢弃
    } catch (e) {
      if (ctx?.id === id) toast("error", t("加载提交历史失败: ") + String(e));
    }
  }

  function parseFiles(raw: string): string[] {
    try {
      const v = JSON.parse(raw);
      return Array.isArray(v) ? v.map(itemText).filter(Boolean) : [];
    } catch {
      return [];
    }
  }

  // 快照里的条目是 {content, status} 对象（旧数据/外部写入可能是纯字符串）；
  // 直接 String(obj) 会得到 "[object Object]"——导出 MD 的异常就出在这里。
  function itemText(v: unknown): string {
    if (v === null || v === undefined) return "";
    if (typeof v === "string") return v;
    if (typeof v === "number" || typeof v === "boolean") return String(v);
    if (typeof v === "object") {
      const o = v as Record<string, unknown>;
      for (const k of ["content", "text", "title", "summary", "name", "path"]) {
        if (typeof o[k] === "string") return o[k] as string;
      }
      try {
        return JSON.stringify(v);
      } catch {
        return "";
      }
    }
    return String(v);
  }

  interface SnapEntry {
    content: string;
    status: string;
  }
  /** 把快照中的任意列表规整为 {content, status}[]（界面展示与导出共用）。 */
  function snapList(v: unknown): SnapEntry[] {
    if (!Array.isArray(v)) return [];
    return v
      .map((it) => ({
        content: itemText(it),
        status: it && typeof it === "object" && typeof (it as Record<string, unknown>).status === "string" ? ((it as Record<string, unknown>).status as string) : "",
      }))
      .filter((e) => e.content.trim() !== "");
  }

  // 导出全部提交版本为 Markdown（写到系统"下载"目录）
  async function exportMarkdown() {
    if (!ctx) return;
    const ctxNow = ctx;
    // 导出「全部」版本：界面只加载最近 50 条，这里单独拉取完整历史（后端不裁剪提交）
    let all: ScCommit[];
    try {
      all = await api.contextCommits(ctxNow.id, EXPORT_ALL_LIMIT);
    } catch (e) {
      toast("error", t("导出失败: ") + String(e));
      return;
    }
    if (ctx?.id !== ctxNow.id) return;
    const esc = (s: string) => s.replaceAll("\r\n", "\n");
    // 列表项内的换行：续行缩进两格，保持在同一个列表项里
    const li = (prefix: string, text: string) => `${prefix}${esc(text).replaceAll("\n", "\n  ")}`;
    const lines: string[] = [];
    lines.push(t("# 共享上下文：{p0}", { p0: ctxNow.name }));
    lines.push("");
    lines.push(t("- 项目：{p0}", { p0: app.projects.find((p) => p.id === ctxNow.project_id)?.name ?? ctxNow.project_id }));
    lines.push(t("- 导出时间：{p0}", { p0: new Date().toLocaleString(localeTag()) }));
    const sorted = [...all].sort((a, b) => a.seq - b.seq);
    lines.push(
      t("- 提交版本：v{p0} ~ v{p1}（共 {p2} 条）", {
        p0: sorted[0]?.seq ?? 0,
        p1: sorted[sorted.length - 1]?.seq ?? 0,
        p2: sorted.length,
      }),
    );
    lines.push("");
    for (const c of sorted) {
      lines.push(`---`);
      lines.push("");
      lines.push(`## v${c.seq} · ${c.created_at}`);
      const who = [c.agent_type, c.session_id ? t("会话 {p0}", { p0: c.session_id.slice(0, 8) }) : ""].filter(Boolean).join(" · ");
      if (who) lines.push(`> ${who}`);
      lines.push("");
      if (c.summary) lines.push(t("**摘要**：{p0}", { p0: esc(c.summary) }), "");
      const files = parseFiles(c.files);
      if (files.length) lines.push(t("**涉及文件**："), "", ...files.map((f) => li("- ", f)), "");
      const snap = parseSnap(c.snapshot || "{}");
      const text = (label: string, v: unknown) => {
        const s = itemText(v).trim();
        if (s) lines.push(`**${label}**：`, "", esc(s), "");
      };
      const list = (label: string, v: unknown, todo = false) => {
        const rows = snapList(v);
        if (!rows.length) return;
        lines.push(`**${label}**：`, "");
        for (const r of rows) lines.push(li(todo ? (r.status === "done" ? "- [x] " : "- [ ] ") : "- ", r.content));
        lines.push("");
      };
      text(t("概述"), snap.overview);
      list(t("待办"), snap.todos ?? snap.entries, true);
      list(t("进展"), snap.progress);
      list(t("注意"), snap.notes);
      text(t("约束"), snap.constraints);
    }
    const name = `context-${ctxNow.name.replace(/[\\/:*?"<>|]/g, "_")}-${new Date().toISOString().slice(0, 10)}.md`;
    try {
      const dir = await api.fsDesktopDir().catch(() => "");
      const initial = dir ? `${dir}\\${name}` : name;
      const target = await promptDialog({
        title: t("导出 MD"),
        label: t("保存路径（含文件名，可改）"),
        initial,
      });
      if (target === null) return;
      const path = target.trim();
      if (!path) { toast("warn", t("路径不能为空")); return; }
      await api.fsWrite(path, lines.join("\n"));
      toast("ok", t("已导出到 {p0}", { p0: path }));
    } catch (e) {
      toast("error", t("导出失败: ") + String(e));
    }
  }

  /** 快照字段一律按 unknown 处理：历史数据/外部写入的形状不保证与当前结构一致 */
  type Snap = Record<string, unknown>;
  function parseSnap(raw: string): Snap {
    try {
      const v = JSON.parse(raw);
      return v && typeof v === "object" && !Array.isArray(v) ? (v as Snap) : {};
    } catch {
      return {};
    }
  }

  // 切换上下文：只依赖 id（列表刷新产生的新对象不会重置正在编辑的文本）。
  // 先把上一个上下文的待保存修改立即落库，再清空旧内容并加载新内容。
  $effect(() => {
    const id = ctxId;
    untrack(() => {
      void flushPending();
      openCommit = null;
      entries = [];
      commits = [];
      dirty = false;
      editSeq++;
      if (!id) {
        overview = "";
        constraints = "";
        return;
      }
      const c = app.contexts.find((x) => x.id === id);
      overview = c?.overview ?? "";
      constraints = c?.constraints ?? "";
      void refresh(id);
      void refreshCommits(id);
    });
  });

  /** 从后端重新读取当前上下文的概述/约束（仅在没有未保存修改时覆盖文本框）。 */
  async function reloadHead(id: string) {
    await refreshContexts();
    const c = app.contexts.find((x) => x.id === id);
    if (!c || ctx?.id !== id || dirty) return;
    overview = c.overview;
    constraints = c.constraints;
  }

  let unlistenCommit: (() => void) | null = null;
  onMount(() => {
    // 任何一方（Agent 经 MCP / 本界面）提交后实时刷新条目、概述与提交历史；只处理当前上下文的事件
    void listen<{ contextId: string; seq: number; summary: string; agent: string }>("sc://commit", (e) => {
      const id = e.payload.contextId;
      if (!id || ctx?.id !== id) return;
      void refresh(id);
      void refreshCommits(id);
      if (e.payload.agent !== "user") void reloadHead(id);
    }).then((f) => (unlistenCommit = f));
    return () => {
      unlistenCommit?.();
      void flushPending();
    };
  });

  async function refresh(id: string) {
    try {
      const list = await api.entriesList(id);
      if (ctx?.id === id) entries = list;
    } catch (e) {
      if (ctx?.id === id) toast("error", String(e));
    }
  }

  function queueSave() {
    const target = ctx;
    if (!target) return;
    dirty = true;
    const seq = ++editSeq;
    // snapshot at queue time: the timer must never write this context's
    // edits into whichever context is selected when it fires
    pendingSave = { id: target.id, overview, constraints, seq };
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      void flushPending();
    }, 600);
  }

  /** 立即写入待保存的概述/约束（切换上下文、离开页面时调用，不再等待防抖）。 */
  async function flushPending() {
    const snap = pendingSave;
    pendingSave = null;
    if (saveTimer) {
      clearTimeout(saveTimer);
      saveTimer = null;
    }
    if (!snap) return;
    // 名称以当前列表为准：期间可能已在侧栏重命名，不能用排队时的旧名字覆盖回去
    const row = app.contexts.find((x) => x.id === snap.id);
    if (!row) return; // 上下文已被删除
    try {
      await api.contextsUpdate(snap.id, row.name, snap.overview, snap.constraints);
      // 让全局列表与后端一致（侧栏重命名等操作会读取这里的概述/约束）
      const live = app.contexts.find((x) => x.id === snap.id);
      if (live) {
        live.overview = snap.overview;
        live.constraints = snap.constraints;
      }
      if (ctx?.id === snap.id && editSeq === snap.seq) dirty = false;
    } catch (e) {
      toast("error", t("保存失败：{error}", { error: String(e) }));
    }
  }

  async function add(kind: keyof typeof inputs) {
    const id = ctx?.id;
    if (!id) return;
    const content = inputs[kind].trim();
    if (!content) return;
    try {
      await api.entryAdd(id, kind, content);
      if (ctx?.id === id) inputs[kind] = "";
      await refresh(id);
      await tick();
      const el = listEls[kind];
      if (el) el.scrollTop = el.scrollHeight;
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function toggle(e: ContextEntry) {
    const id = e.context_id;
    try {
      await api.entryUpdate(e.id, e.content, e.status === "done" ? "open" : "done");
      await refresh(id);
    } catch (err) {
      toast("error", String(err));
    }
  }

  async function remove(e: ContextEntry) {
    const id = e.context_id;
    try {
      await api.entryDelete(e.id);
      await refresh(id);
    } catch (err) {
      toast("error", String(err));
    }
  }

  const of = (kind: string) => entries.filter((e) => e.kind === kind);
  const todoDone = $derived(entries.filter((e) => e.kind === "todo" && e.status === "done").length);
</script>

<div class="ctxview">
  {#if ctx}
    <div class="col">
      <section class="card">
        <h3>{t("🧭 概述 Overview")}</h3>
        <textarea
          rows="4"
          bind:value={overview}
          oninput={queueSave}
          placeholder={t("这个上下文要做什么？当前目标、范围、背景…（自动保存）")}
        ></textarea>
      </section>

      <section class="card">
        <h3>{t("✅ 待办 Todo")}{#if of("todo").length}<span class="count">{todoDone}/{of("todo").length}</span>{/if}</h3>
        <div class="addrow">
          <input
            placeholder={t("添加待办，回车确认")}
            bind:value={inputs.todo}
            onkeydown={(e) => e.key === "Enter" && add("todo")}
          />
          <button class="btn sm" onclick={() => add("todo")}>{t("添加")}</button>
        </div>
        <ul class="todos scroll" bind:this={listEls.todo}>
          {#each of("todo") as e (e.id)}
            <li class:done={e.status === "done"}>
              <input type="checkbox" checked={e.status === "done"} onchange={() => toggle(e)} />
              <span class="content">{e.content}</span>
              <button class="btn ghost sm" onclick={() => remove(e)}>✕</button>
            </li>
          {:else}
            <li class="none">{t("暂无待办")}</li>
          {/each}
        </ul>
      </section>

      <section class="card">
        <h3>{t("📈 进展 Progress")}{#if of("progress").length}<span class="count">{of("progress").length}</span>{/if}</h3>
        <div class="addrow">
          <input placeholder={t("记录阶段性进展，回车确认")} bind:value={inputs.progress} onkeydown={(e) => e.key === "Enter" && add("progress")} />
          <button class="btn sm" onclick={() => add("progress")}>{t("添加")}</button>
        </div>
        <ul class="scroll" bind:this={listEls.progress}>
          {#each of("progress") as e (e.id)}
            <li><span class="content">{e.content}</span><span class="time">{e.created_at.slice(5, 16)}</span><button class="btn ghost sm" onclick={() => remove(e)}>✕</button></li>
          {:else}
            <li class="none">{t("暂无进展记录")}</li>
          {/each}
        </ul>
      </section>

      <div class="pair">
        <section class="card">
          <h3>{t("⚖️ 决策 Decisions")}{#if of("decision").length}<span class="count">{of("decision").length}</span>{/if}</h3>
          <div class="addrow">
            <input placeholder={t("记录重要决策")} bind:value={inputs.decision} onkeydown={(e) => e.key === "Enter" && add("decision")} />
            <button class="btn sm" onclick={() => add("decision")}>{t("添加")}</button>
          </div>
          <ul class="scroll" bind:this={listEls.decision}>
            {#each of("decision") as e (e.id)}
              <li><span class="content">{e.content}</span><button class="btn ghost sm" onclick={() => remove(e)}>✕</button></li>
            {:else}
              <li class="none">{t("暂无决策记录")}</li>
            {/each}
          </ul>
        </section>
        <section class="card">
          <h3>{t("📝 注意 Notes")}{#if of("note").length}<span class="count">{of("note").length}</span>{/if}</h3>
          <div class="addrow">
            <input placeholder={t("记录注意事项")} bind:value={inputs.note} onkeydown={(e) => e.key === "Enter" && add("note")} />
            <button class="btn sm" onclick={() => add("note")}>{t("添加")}</button>
          </div>
          <ul class="scroll" bind:this={listEls.note}>
            {#each of("note") as e (e.id)}
              <li><span class="content">{e.content}</span><button class="btn ghost sm" onclick={() => remove(e)}>✕</button></li>
            {:else}
              <li class="none">{t("暂无注意事项")}</li>
            {/each}
          </ul>
        </section>
      </div>

      <section class="card commits">
        <h3>{t("🕘 提交历史")} {#if commits.length}<span class="count">{commits.length >= 50 ? t("最近 {n} 条", { n: commits.length }) : commits.length}</span>{/if}<span class="ver">{t("当前版本 v{version}", { version: commits[0]?.seq ?? 0 })}</span><span class="spacer"></span><button class="btn ghost sm" title={t("导出全部版本为 Markdown（保存到桌面）")} onclick={exportMarkdown}>{t("导出 MD")}</button><button class="btn ghost sm" title={t("刷新")} onclick={() => { if (ctx) void refreshCommits(ctx.id); }}>⟳</button></h3>
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
                {@const sOverview = itemText(snap.overview).trim()}
                {@const sTodos = snapList(snap.todos ?? snap.entries)}
                {@const sProgress = snapList(snap.progress)}
                {@const sNotes = snapList(snap.notes)}
                {@const sConstraints = itemText(snap.constraints).trim()}
                <div class="cbody">
                  {#if parseFiles(c.files).length}
                    <div class="cfiles">{t("涉及文件：")}{#each parseFiles(c.files) as f, i (f + i)}{#if i > 0}、{/if}<code>{f}</code>{/each}</div>
                  {/if}
                  <!-- 按索引作 key：同一快照里可能有内容相同的条目（按内容作 key 会触发 each_key_duplicate 渲染错误） -->
                  {#if sOverview}<div class="csec"><b>{t("概述")}</b><div class="ctext">{sOverview}</div></div>{/if}
                  {#if sTodos.length}<div class="csec"><b>{t("待办")}</b><ul>{#each sTodos as it, i (i)}<li class:done={it.status === "done"}>{it.content}{it.status === "done" ? " ✓" : ""}</li>{/each}</ul></div>{/if}
                  {#if sProgress.length}<div class="csec"><b>{t("进展")}</b><ul>{#each sProgress as it, i (i)}<li>{it.content}</li>{/each}</ul></div>{/if}
                  {#if sNotes.length}<div class="csec"><b>{t("注意")}</b><ul>{#each sNotes as it, i (i)}<li>{it.content}</li>{/each}</ul></div>{/if}
                  {#if sConstraints}<div class="csec"><b>{t("约束")}</b><div class="ctext">{sConstraints}</div></div>{/if}
                </div>
              {/if}
            </div>
          {:else}
            <p class="none">{t("还没有提交记录（Agent 通过 MCP 提交后会出现在这里）")}</p>
          {/each}
        </div>
      </section>

      <section class="card">
        <h3>{t("⛔ 约束 Constraints")}</h3>
        <textarea
          rows="3"
          bind:value={constraints}
          oninput={queueSave}
          placeholder={t("项目约束：技术栈限定、不能动的部分、性能要求…（自动保存）")}
        ></textarea>
      </section>
    </div>
  {:else}
    <div class="empty">{t("选择一个上下文")}</div>
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
  .card h3 .count {
    margin-left: 8px;
    padding: 0 7px;
    border-radius: 999px;
    background: var(--bg-elev);
    color: var(--text-faint);
    font-size: 0.8em;
    font-weight: 500;
  }
  /* 各分区列表条目多时在卡片内滚动，而不是整页一股脑摊开 */
  ul.scroll {
    max-height: 264px;
    overflow-y: auto;
    overscroll-behavior: contain;
    scrollbar-gutter: stable;
  }
  .pair ul.scroll {
    max-height: 220px;
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
    overscroll-behavior: contain;
    scrollbar-gutter: stable;
  }
  .commit {
    /* 必须 flex:none：列表是限高的纵向 flex 容器，而 overflow:hidden 让条目的最小高度
       变成 0，条目一多就被压缩——这就是「提交历史压着叠起来」的根因 */
    flex: none;
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
    /* 单个版本的快照内容很长时在自身内部滚动，不把整个列表撑满 */
    max-height: 300px;
    overflow-y: auto;
    overscroll-behavior: contain;
  }
  .csec li {
    white-space: pre-wrap;
  }
  .csec li.done {
    color: var(--text-faint);
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
