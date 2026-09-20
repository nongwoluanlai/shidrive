<script lang="ts">
  import { md } from "../markdown";
  import type { DisplayItem } from "../state.svelte";
  import { api } from "../ipc";
  import { toast } from "../state.svelte";

  let { item }: { item: DisplayItem } = $props();

  const isTool = $derived(item.kind === "tools" && !!item.tools?.length);
  let expandedTools = $state<Record<string, boolean>>({});
  // 工具多时整组默认折叠（像思考过程一样）
  let groupOpen = $state(false);
  const anyLive = $derived((item.tools ?? []).some((t) => t.status === "in_progress" || t.status === "pending"));
  // 进行中时自动展开，回合结束保持用户手动状态（初始跟随 anyLive）
  $effect(() => {
    if (anyLive) groupOpen = true;
  });

  const groupSummary = $derived.by(() => {
    const tools = item.tools ?? [];
    const done = tools.filter((t) => t.status === "completed").length;
    const failed = tools.filter((t) => t.status === "failed").length;
    const live = tools.filter((t) => t.status === "in_progress" || t.status === "pending").length;
    const parts: string[] = [`${tools.length} 次调用`];
    if (live) parts.push(`${live} 进行中`);
    if (done) parts.push(`${done} 已完成`);
    if (failed) parts.push(`${failed} 失败`);
    return parts.join(" · ");
  });

  const toolStatusClass = (s?: string) =>
    s === "completed" ? "ok" : s === "failed" ? "danger" : s === "in_progress" ? "accent" : "warn";
  const toolStatusText = (s?: string) =>
    ({ pending: "等待中", in_progress: "执行中", completed: "已完成", failed: "失败" }[s ?? "pending"] ?? s ?? "等待中");
  const toolIcon = (kind?: string) =>
    ({ read: "📖", edit: "✏️", delete: "🗑", move: "📂", search: "🔍", execute: "▶", think: "🤔", fetch: "🌐", plan: "🗺", other: "🔧" }[kind ?? "other"] ??
    "🔧");

  function toolDetail(t: { rawInput?: unknown; rawOutput?: unknown }): string {
    const parts: string[] = [];
    if (t.rawInput !== undefined) parts.push("输入：\n" + JSON.stringify(t.rawInput, null, 2));
    if (t.rawOutput !== undefined) {
      const out = typeof t.rawOutput === "string" ? t.rawOutput : JSON.stringify(t.rawOutput, null, 2);
      parts.push("输出：\n" + (out.length > 4000 ? out.slice(0, 4000) + "\n…(截断)" : out));
    }
    return parts.join("\n\n");
  }

  function planEntries(rawOutput: unknown): { content: string; status: string; priority: string }[] {
    if (Array.isArray(rawOutput)) return rawOutput as any;
    return [];
  }

  async function openLocation(path: string) {
    try {
      const full = path.match(/^([a-zA-Z]:\\[^:]*|\/.*)$/)?.[1] ?? path;
      await api.fsOpenExplorer(full);
    } catch (e) {
      toast("error", String(e));
    }
  }

  // Markdown 渲染节流：流式时文本每个 chunk 都在变，全量重渲是 O(n²)。
  // 渲染最多 180ms 一次；小文本（首条/短消息）仍即时渲染。
  const MD_THROTTLE = 180;
  let html = $state("");
  let lastMdAt = 0;
  let mdTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const src = item.kind === "assistant" || item.kind === "user" || item.kind === "thought" ? item.text : "";
    if (Date.now() - lastMdAt >= MD_THROTTLE) {
      lastMdAt = Date.now();
      html = md(src);
    } else {
      // trailing render: always reschedule with the latest text so the
      // final chunk is never lost when the stream ends mid-window
      if (mdTimer) clearTimeout(mdTimer);
      mdTimer = setTimeout(() => {
        mdTimer = null;
        lastMdAt = Date.now();
        html = md(src);
      }, MD_THROTTLE);
    }
  });
  const thoughtHtml = $derived(item.kind === "thought" ? html : "");
</script>

{#if item.kind === "user"}
  <div class="row user">
    <div class="bubble user-bubble content">{@html html}</div>
  </div>
{:else if item.kind === "assistant"}
  <div class="row">
    <div class="avatar ai">AI</div>
    <div class="bubble ai-bubble content" class:streaming={item.streaming}>{@html html}</div>
  </div>
{:else if item.kind === "thought"}
  <div class="row">
    <div class="avatar thought">💭</div>
    <div class="bubble thought-bubble content">
      <details open={item.streaming}>
        <summary>思考过程 {item.streaming ? "…" : ""}</summary>
        <div class="inner">{@html thoughtHtml}</div>
      </details>
    </div>
  </div>
{:else if item.kind === "error"}
  <div class="row">
    <div class="bubble error-bubble">{item.text}</div>
  </div>
{:else if isTool}
  <div class="tools">
    <div class="tools-group" role="button" tabindex="0" onclick={() => (groupOpen = !groupOpen)} onkeydown={(e) => e.key === "Enter" && (groupOpen = !groupOpen)}>
      <span class="tg-icon">🔧</span>
      <span class="tg-title">工具调用</span>
      <span class="tg-summary">{groupSummary}</span>
      <span class="tg-arrow">{groupOpen ? "▾" : "▸"}</span>
    </div>
    {#if groupOpen}
      {#each item.tools ?? [] as t (t.toolCallId)}
      {#if t.toolCallId === "__plan__"}
        <div class="tool plan">
          <div class="thead"><span>🗺 计划</span></div>
          <ul class="plan-list">
            {#each planEntries(t.rawOutput) as e}
              <li class={e.status}>
                <span class="pmark">{e.status === "completed" ? "✓" : e.status === "in_progress" ? "▸" : "○"}</span>
                {e.content}
              </li>
            {/each}
          </ul>
        </div>
      {:else}
        <div class="tool">
          <div class="thead" role="button" tabindex="0" onclick={() => (expandedTools[t.toolCallId] = !expandedTools[t.toolCallId])}>
            <span>{toolIcon(t.kind)} {t.title ?? t.kind ?? "工具调用"}</span>
            <span class="meta">
              {#if t.locations?.length}
                {#each t.locations.slice(0, 2) as loc (loc.path + (loc.line ?? ""))}
                  <a
                    href="#"
                    title={loc.path}
                    onclick={(e) => {
                      e.preventDefault();
                      void openLocation(loc.path);
                    }}>📍{loc.path.split(/[\\/]/).pop()}</a
                  >
                {/each}
              {/if}
              <span class="badge {toolStatusClass(t.status)}">{toolStatusText(t.status)}</span>
              <span class="tw">{expandedTools[t.toolCallId] ? "▾" : "▸"}</span>
            </span>
          </div>
          {#if expandedTools[t.toolCallId]}
            <pre class="tdetail">{toolDetail(t) || "（无详情）"}</pre>
          {/if}
        </div>
      {/if}
    {/each}
    {/if}
  </div>
{/if}

<style>
  .row {
    display: flex;
    gap: 10px;
    padding: 6px 20px;
    align-items: flex-start;
  }
  .row.user {
    justify-content: flex-end;
  }
  .avatar {
    width: 28px;
    height: 28px;
    border-radius: 8px;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 12px;
    font-weight: 700;
    flex: none;
    margin-top: 2px;
    background: var(--bg-elev2);
    color: var(--text-dim);
  }
  .avatar.ai {
    background: var(--accent);
    color: var(--accent-contrast);
  }
  .bubble {
    max-width: min(78%, 860px);
    padding: 9px 14px;
    border-radius: var(--radius);
    overflow-wrap: break-word;
    min-width: 0;
  }
  .user-bubble {
    background: var(--accent);
    color: var(--accent-contrast);
    border-bottom-right-radius: 4px;
    user-select: text;
  }
  .ai-bubble {
    background: var(--bg-panel);
    border: 1px solid var(--border-soft);
    border-top-left-radius: 4px;
    user-select: text;
  }
  .ai-bubble.streaming::after {
    content: "▍";
    color: var(--accent);
    animation: pulse 1s infinite;
  }
  .thought-bubble {
    background: transparent;
    border: 1px dashed var(--border);
    color: var(--text-dim);
    padding: 4px 12px;
  }
  .thought-bubble summary {
    cursor: pointer;
    font-size: 0.86em;
  }
  .thought-bubble .inner {
    font-size: 0.88em;
    margin-top: 6px;
  }
  .error-bubble {
    background: color-mix(in srgb, var(--danger) 14%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
    color: var(--danger);
  }

  /* markdown content */
  .content :global(p) {
    margin: 0.45em 0;
  }
  .content :global(p:first-child) {
    margin-top: 0;
  }
  .content :global(p:last-child) {
    margin-bottom: 0;
  }
  .content :global(h1),
  .content :global(h2),
  .content :global(h3),
  .content :global(h4) {
    margin: 0.7em 0 0.3em;
    line-height: 1.3;
  }
  .content :global(h1) {
    font-size: 1.18em;
  }
  .content :global(h2) {
    font-size: 1.1em;
  }
  .content :global(h3),
  .content :global(h4) {
    font-size: 1.02em;
  }
  /* lists: hanging indent with an accent marker, breathing room between items */
  .content :global(ul),
  .content :global(ol) {
    padding-left: 1.5em;
    margin: 0.45em 0;
  }
  .content :global(li) {
    margin: 0.22em 0;
    line-height: 1.65;
    padding-left: 0.15em;
  }
  .content :global(li::marker) {
    color: var(--text-faint);
  }
  .content :global(ol) {
    list-style: decimal;
  }
  .content :global(ol > li::marker) {
    font-variant-numeric: tabular-nums;
    font-weight: 600;
  }
  .content :global(li > ul),
  .content :global(li > ol) {
    margin: 0.18em 0;
  }
  .content :global(li > p) {
    margin: 0.15em 0;
  }
  .content :global(li.task-list-item) {
    list-style: none;
    margin-left: -1.2em;
  }
  .content :global(input[type="checkbox"]) {
    margin-right: 0.4em;
    vertical-align: -1px;
  }
  .content :global(img) {
    max-width: 100%;
    border-radius: 8px;
  }
  .content :global(code) {
    font-family: var(--mono);
    font-size: 0.88em;
    background: var(--code-bg);
    padding: 1px 5px;
    border-radius: 5px;
  }
  .content :global(pre) {
    background: var(--code-bg);
    border: 1px solid var(--border-soft);
    border-radius: 8px;
    padding: 10px 12px;
    overflow-x: auto;
    margin: 0.5em 0;
  }
  .content :global(pre code) {
    background: none;
    padding: 0;
  }
  .content :global(a) {
    color: var(--accent);
  }
  .content :global(blockquote) {
    border-left: 3px solid var(--accent);
    padding-left: 10px;
    color: var(--text-dim);
    margin: 0.4em 0;
  }
  .content :global(table) {
    border-collapse: collapse;
    margin: 0.5em 0;
  }
  .content :global(th),
  .content :global(td) {
    border: 1px solid var(--border);
    padding: 4px 10px;
  }
  .content :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 0.8em 0;
  }
  /* user bubbles sit on the accent color: keep inline code readable */
  .user-bubble :global(code) {
    background: rgba(255, 255, 255, 0.18);
  }
  .user-bubble :global(a) {
    color: inherit;
    text-decoration: underline;
  }
  .user-bubble :global(li::marker),
  .user-bubble :global(ol > li::marker) {
    color: color-mix(in srgb, var(--accent-contrast) 70%, transparent);
  }

  /* tools */
  .tools {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 6px 20px 6px 58px;
  }
  .tools-group {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    border: 1px solid var(--border-soft);
    background: var(--bg-panel);
    border-radius: 8px;
    font-size: 0.86em;
    color: var(--text-dim);
    cursor: pointer;
    user-select: none;
  }
  .tools-group:hover {
    background: var(--bg-elev);
    color: var(--text);
  }
  .tg-title {
    font-weight: 600;
    color: var(--text);
  }
  .tg-summary {
    flex: 1;
  }
  .tg-arrow {
    color: var(--text-faint);
  }
  .tool {
    border: 1px solid var(--border-soft);
    background: var(--bg-panel);
    border-radius: 8px;
    overflow: hidden;
  }
  .thead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 6px 12px;
    font-size: 0.88em;
    cursor: pointer;
    color: var(--text-dim);
  }
  .thead:hover {
    background: var(--bg-elev);
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .meta a {
    color: var(--text-faint);
    font-size: 0.92em;
    text-decoration: none;
  }
  .meta a:hover {
    color: var(--accent);
  }
  .tw {
    color: var(--text-faint);
  }
  .tdetail {
    font-family: var(--mono);
    font-size: 0.8em;
    padding: 10px 12px;
    border-top: 1px solid var(--border-soft);
    background: var(--code-bg);
    overflow-x: auto;
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 300px;
    overflow-y: auto;
    user-select: text;
  }
  .plan-list {
    list-style: none;
    padding: 4px 14px 8px;
    font-size: 0.9em;
  }
  .plan-list li {
    display: flex;
    gap: 8px;
    padding: 2px 0;
    color: var(--text-dim);
  }
  .plan-list li.completed {
    color: var(--text-faint);
    text-decoration: line-through;
  }
  .plan-list li.in_progress {
    color: var(--accent);
  }
</style>
