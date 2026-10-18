<script lang="ts">
  import { md } from "../lib/markdown";
  import type { Item, ToolCall } from "./api";
  import Icon from "./Icon.svelte";

  let { item }: { item: Item } = $props();

  let thoughtOpen = $state(false);
  let openTools = $state<Record<string, boolean>>({});

  const html = $derived(item.kind === "assistant" ? md(item.text) : "");

  const STATUS: Record<string, { cls: string; label: string }> = {
    pending: { cls: "", label: "等待" },
    in_progress: { cls: "run", label: "进行中" },
    completed: { cls: "ok", label: "完成" },
    failed: { cls: "err", label: "失败" },
  };

  function clip(s: string, n = 6000) {
    return s.length > n ? s.slice(0, n) + `\n…（共 ${s.length} 字符，已截断）` : s;
  }
  function stringify(v: unknown): string {
    if (v == null) return "";
    if (typeof v === "string") return v;
    try { return JSON.stringify(v, null, 2); } catch { return String(v); }
  }
  function toolDetail(t: ToolCall): string {
    const parts: string[] = [];
    if (t.kind === "plan" && Array.isArray(t.rawOutput)) {
      return (t.rawOutput as any[]).map((e) => `${e.status === "completed" ? "✔" : e.status === "in_progress" ? "▶" : "○"} ${e.content}`).join("\n");
    }
    if (t.locations?.length) parts.push(t.locations.map((l) => l.path + (l.line ? `:${l.line}` : "")).join("\n"));
    const input = stringify(t.rawInput);
    if (input && input !== "{}") parts.push("输入：\n" + clip(input, 3000));
    if (Array.isArray(t.content)) {
      for (const c of t.content as any[]) {
        if (c?.type === "content") parts.push(clip(String(c.content?.text ?? stringify(c.content))));
        else if (c?.type === "diff") parts.push(`修改 ${c.path ?? ""}\n` + clip(String(c.newText ?? ""), 4000));
        else if (c?.type === "terminal") parts.push(`终端 ${c.terminalId ?? ""}`);
        else parts.push(clip(stringify(c)));
      }
    } else if (t.content) parts.push(clip(stringify(t.content)));
    const out = stringify(t.rawOutput);
    if (out && !Array.isArray(t.content)) parts.push("输出：\n" + clip(out));
    return parts.filter(Boolean).join("\n\n") || "（无详情）";
  }
  function toolIcon(kind?: string) {
    switch (kind) {
      case "read": return "eye";
      case "edit": return "code";
      case "search": return "search";
      case "execute": return "tool";
      case "plan": return "check";
      case "think": return "brain";
      default: return "tool";
    }
  }
</script>

{#if item.kind === "user"}
  <div class="row user">
    <div class="bubble">{item.text}</div>
  </div>
{:else if item.kind === "assistant"}
  <div class="row assistant">
    <div class="md">{@html html}</div>
    {#if item.streaming}<span class="caret pulse"></span>{/if}
  </div>
{:else if item.kind === "thought"}
  <div class="row thought">
    <button class="thought-head" onclick={() => (thoughtOpen = !thoughtOpen)}>
      <Icon name="brain" size={15} /><span>{item.streaming ? "思考中…" : "思考过程"}</span>
      <span class="chev" class:open={thoughtOpen || item.streaming}><Icon name="chevron" size={14} /></span>
    </button>
    {#if thoughtOpen || item.streaming}<div class="thought-body">{item.text}</div>{/if}
  </div>
{:else if item.kind === "tools"}
  <div class="row tools">
    {#each item.tools ?? [] as t (t.toolCallId)}
      {@const st = STATUS[t.status ?? ""] ?? { cls: "", label: t.status ?? "" }}
      <div class="tool" class:open={openTools[t.toolCallId]}>
        <button class="tool-head" onclick={() => (openTools[t.toolCallId] = !openTools[t.toolCallId])}>
          <Icon name={toolIcon(t.kind)} size={15} />
          <span class="tool-title">{t.title || t.kind || "工具调用"}</span>
          {#if st.label}<span class="badge {st.cls}">{#if t.status === "in_progress"}<span class="spin sm"></span>{/if}{st.label}</span>{/if}
        </button>
        {#if openTools[t.toolCallId]}<pre class="tool-body">{toolDetail(t)}</pre>{/if}
      </div>
    {/each}
  </div>
{:else}
  <div class="row error"><Icon name="alert" size={16} /><span>{item.text}</span></div>
{/if}

<style>
  .row { padding: 6px 0; max-width: 100%; }
  .user { display: flex; justify-content: flex-end; }
  .bubble {
    background: var(--user-bubble); padding: 9px 13px; border-radius: 16px 16px 4px 16px; max-width: min(85%, 720px);
    white-space: pre-wrap; word-break: break-word; user-select: text;
  }
  .assistant { user-select: text; }
  .caret { display: inline-block; width: 8px; height: 16px; background: var(--accent); border-radius: 2px; vertical-align: text-bottom; margin-left: 2px; }
  .thought-head, .tool-head {
    display: flex; align-items: center; gap: 7px; width: 100%; text-align: left; color: var(--text-dim); font-size: 13.5px; padding: 6px 8px; border-radius: 8px;
  }
  .thought-head:hover, .tool-head:hover { background: var(--bg-elev); }
  .chev { margin-left: auto; transition: transform .15s; display: inline-flex; }
  .chev.open { transform: rotate(90deg); }
  .thought-body { white-space: pre-wrap; color: var(--text-faint); font-size: 13.5px; padding: 4px 10px 6px 30px; border-left: 2px solid var(--border); margin-left: 14px; user-select: text; }
  .tools { display: flex; flex-direction: column; gap: 4px; }
  .tool { border: 1px solid var(--border-soft); border-radius: 10px; background: var(--bg-panel); overflow: hidden; }
  .tool-title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--mono); font-size: 12.5px; color: var(--text); }
  .tool-body {
    margin: 0; padding: 8px 10px; border-top: 1px solid var(--border-soft); background: var(--code-bg); font-family: var(--mono); font-size: 12px;
    white-space: pre-wrap; word-break: break-all; max-height: 320px; overflow: auto; user-select: text;
  }
  .spin.sm { width: 10px; height: 10px; border-width: 1.5px; }
  .error { display: flex; gap: 8px; align-items: flex-start; color: var(--danger); background: color-mix(in srgb, var(--danger) 10%, transparent); border-radius: 10px; padding: 8px 12px; white-space: pre-wrap; }
</style>
