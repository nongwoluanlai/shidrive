<script lang="ts">
  import { t } from "../i18n";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";

  const req = $derived(app.permissions[0] ?? null);

  const kindIcon = (kind?: string) =>
    ({ read: "📖", edit: "✏️", delete: "🗑", move: "📂", search: "🔍", execute: "▶", think: "🤔", fetch: "🌐", other: "🔧" }[kind ?? "other"] ?? "🔧");

  const inputText = $derived(
    (() => {
      const raw = req?.params?.toolCall?.rawInput;
      if (raw === undefined) return "";
      return typeof raw === "string" ? raw : JSON.stringify(raw, null, 2);
    })(),
  );

  async function respond(optionId: string) {
    if (!req) return;
    try {
      // answer first; shift afterwards so the request stays visible if the IPC throws
      await api.acpRespondPermission(req.requestId, optionId);
    } catch (e) {
      toast("error", t("权限应答失败: {p0}", { p0: String(e) }));
    }
    app.permissions.shift();
  }

  function optionClass(kind: string) {
    if (kind.startsWith("allow")) return "primary";
    if (kind.startsWith("reject")) return "danger";
    return "";
  }
</script>

{#if req}
  <div class="modal-backdrop">
    <div class="modal perm">
      <header>
        <span>{kindIcon(req.params?.toolCall?.kind)} {t("权限请求 —")} {app.agents.find((a) => a.id === req.agentType)?.name ?? req.agentType}</span>
      </header>
      <div class="body">
        <p class="title">{req.params?.toolCall?.title ?? t("Agent 请求执行操作")}</p>
        {#if inputText}
          <pre>{inputText.length > 2000 ? inputText.slice(0, 2000) + t("\n…(截断)") : inputText}</pre>
        {/if}
      </div>
      <footer>
        {#each req.params?.options ?? [] as opt (opt.optionId)}
          <button class="btn {optionClass(opt.kind)}" onclick={() => respond(opt.optionId)}>{opt.name}</button>
        {/each}
      </footer>
    </div>
  </div>
{/if}

<style>
  .modal.perm {
    min-width: 480px;
  }
  .title {
    font-weight: 600;
    margin-bottom: 10px;
    user-select: text;
  }
  pre {
    background: var(--code-bg);
    border: 1px solid var(--border-soft);
    border-radius: 8px;
    padding: 10px 12px;
    font-family: var(--mono);
    font-size: 0.82em;
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 260px;
    overflow: auto;
    user-select: text;
  }
</style>
