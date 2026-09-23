<script lang="ts">
  // ACP elicitation（unstable 草案，MCP 形状）：agent 请求用户结构化输入。
  // 支持：enum 选项按钮（单字段时可一键作答）、字符串/多行文本、布尔、数字；
  // 应答 { action: "accept" | "decline" | "cancel", content?: {...} }
  import { t } from "../i18n";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";

  interface Field {
    name: string;
    kind: "enum" | "string" | "boolean" | "number";
    options: { value: string; label: string }[];
    description: string;
    required: boolean;
    multiline: boolean;
  }

  interface ElicitParams {
    message: string;
    fields: Field[];
    hasSchema: boolean;
  }

  const req = $derived(app.elicitations[0] ?? null);

  function parseParams(raw: Record<string, unknown> | undefined): ElicitParams {
    const p = (raw ?? {}) as Record<string, unknown>;
    const message = (p.message ?? p.prompt ?? "") as string;
    const schema = (p.requestedSchema ?? p.schema ?? null) as
      | { properties?: Record<string, any>; required?: string[] }
      | null;
    const out: ElicitParams = { message: typeof message === "string" ? message : "", fields: [], hasSchema: false };
    const required = new Set<string>(Array.isArray(schema?.required) ? (schema?.required as string[]) : []);
    if (schema && schema.properties && typeof schema.properties === "object") {
      out.hasSchema = true;
      for (const [name, prop] of Object.entries(schema.properties)) {
        const ty = String(prop?.type ?? "string");
        if (ty === "boolean") {
          out.fields.push({ name, kind: "boolean", options: [], description: prop?.description ?? "", required: required.has(name), multiline: false });
        } else if (Array.isArray(prop?.enum)) {
          const labels: unknown = prop?.enumNames;
          out.fields.push({
            name,
            kind: "enum",
            options: (prop.enum as unknown[]).map((v, i) => ({
              value: String(v),
              label: Array.isArray(labels) && labels[i] != null ? String(labels[i]) : String(v),
            })),
            description: prop?.description ?? "",
            required: required.has(name),
            multiline: false,
          });
        } else if (ty === "integer" || ty === "number") {
          out.fields.push({ name, kind: "number", options: [], description: prop?.description ?? "", required: required.has(name), multiline: false });
        } else {
          const multiline = prop?.format === "multiline" || prop?.["x-multiline"] === true;
          out.fields.push({ name, kind: "string", options: [], description: prop?.description ?? "", required: required.has(name), multiline });
        }
      }
    } else if (Array.isArray(p.choices) && p.choices.length) {
      // 宽容：非标准形状（直接给 choices 数组）
      out.hasSchema = true;
      out.fields.push({
        name: "choice",
        kind: "enum",
        options: (p.choices as unknown[]).map((v, i) =>
          typeof v === "string" ? { value: v, label: v } : { value: String((v as any)?.id ?? i), label: String((v as any)?.label ?? v) },
        ),
        description: "",
        required: true,
        multiline: false,
      });
    }
    return out;
  }

  const parsed = $derived(req ? parseParams(req.params) : null);
  let values = $state<Record<string, unknown>>({});

  // req 变化时重置输入
  $effect(() => {
    if (req?.requestId) values = {};
  });

  function buildContent(fields: Field[]): Record<string, unknown> {
    const content: Record<string, unknown> = {};
    for (const f of fields) {
      const v = values[f.name];
      if (f.kind === "boolean") {
        content[f.name] = v === true;
      } else if (f.kind === "number") {
        const n = typeof v === "string" ? Number(v) : v;
        if (n !== undefined && n !== null && !Number.isNaN(Number(n))) content[f.name] = Number(n);
      } else {
        const sv = typeof v === "string" ? v.trim() : "";
        if (sv !== "") content[f.name] = sv;
      }
    }
    return content;
  }

  async function respond(action: "accept" | "decline" | "cancel", fields?: Field[]) {
    if (!req) return;
    let payload: Record<string, unknown> = { action };
    if (action === "accept" && fields) {
      const missing = fields.filter((f) => f.required && (values[f.name] === undefined || String(values[f.name] ?? "").trim() === ""));
      if (missing.length) {
        toast("warn", t("请填写必填项：{n}", { n: missing.map((f) => f.name).join(", ") }));
        return;
      }
      payload = { action: "accept", content: buildContent(fields) };
    }
    try {
      await api.acpRespondElicitation(req.requestId, payload);
    } catch (e) {
      toast("error", t("输入应答失败: {p0}", { p0: String(e) }));
    }
    app.elicitations.shift();
  }

  // 单 enum 字段：点选项 = 填值并直接提交
  function quickPick(f: Field, value: string) {
    values[f.name] = value;
    if (parsed && parsed.fields.length === 1) void respond("accept", parsed.fields);
  }

  function agentName(id: string) {
    return app.agents.find((a) => a.id === id)?.name ?? id;
  }

  function onInputKeydown(e: KeyboardEvent, fields: Field[]) {
    if (e.key === "Enter" && !(e.target instanceof HTMLTextAreaElement)) {
      e.preventDefault();
      void respond("accept", fields);
    }
  }
</script>

{#if req && parsed}
  <div class="modal-backdrop">
    <div class="modal elic">
      <header>
        <span>💬 {t("输入请求 —")} {agentName(req.agentType)}</span>
      </header>
      <div class="body">
        <p class="msg">{parsed.message || t("Agent 请求你提供以下信息")}</p>
        {#each parsed.fields as f (f.name)}
          <div class="field">
            <label class="lbl">
              {f.name}{f.required ? " *" : ""}
              {#if f.description}<span class="desc">{f.description}</span>{/if}
            </label>
            {#if f.kind === "enum"}
              <div class="opts">
                {#each f.options as opt (opt.value)}
                  <button class="btn sm" class:primary={values[f.name] === opt.value} onclick={() => quickPick(f, opt.value)}>{opt.label}</button>
                {/each}
              </div>
            {:else if f.kind === "boolean"}
              <label class="check"><input type="checkbox" bind:checked={values[f.name] as boolean} /> {f.description || f.name}</label>
            {:else if f.kind === "number"}
              <input type="number" bind:value={values[f.name] as number} placeholder={f.description || f.name} onkeydown={(e) => onInputKeydown(e, parsed.fields)} />
            {:else if f.multiline}
              <textarea rows="3" bind:value={values[f.name] as string} placeholder={f.description || f.name}></textarea>
            {:else}
              <input bind:value={values[f.name] as string} placeholder={f.description || f.name} onkeydown={(e) => onInputKeydown(e, parsed.fields)} />
            {/if}
          </div>
        {/each}
        {#if !parsed.hasSchema}
          <div class="field">
            <label class="lbl">{t("自由输入（可选）")}</label>
            <textarea rows="3" bind:value={values.__free as string} placeholder={t("想对 Agent 说的内容…")}></textarea>
          </div>
        {/if}
      </div>
      <footer>
        <button class="btn primary" onclick={() => void respond("accept", parsed.fields)}>{t("提交")}</button>
        <button class="btn" onclick={() => void respond("decline")}>{t("不提供（decline）")}</button>
        <button class="btn danger ghost" onclick={() => void respond("cancel")}>{t("取消（cancel）")}</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .modal.elic {
    min-width: 500px;
  }
  .msg {
    font-weight: 600;
    margin-bottom: 12px;
    white-space: pre-wrap;
    user-select: text;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 12px;
  }
  .lbl {
    font-size: 0.86em;
    color: var(--text-dim);
    display: flex;
    gap: 8px;
    align-items: baseline;
  }
  .desc {
    color: var(--text-faint);
    font-size: 0.92em;
    user-select: text;
  }
  .opts {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .check {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 0.9em;
  }
  input,
  textarea {
    width: 100%;
    box-sizing: border-box;
  }
</style>
