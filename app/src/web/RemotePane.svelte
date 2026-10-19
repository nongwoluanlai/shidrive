<script lang="ts">
  // 网页端「外部编程接入」：与电脑端 设置 → 外部编程接入 等价（服务启停、接入方式、授权、OAuth 客户端）。
  // cloudflared 的安装 / 路径指定涉及本机文件，仍需在电脑端操作。
  import { onMount } from "svelte";
  import { api, type RemoteGrant, type RemoteState, type Context } from "./api";
  import { ui, toast, errText } from "./store.svelte";
  import Icon from "./Icon.svelte";

  let st = $state<RemoteState | null>(null);
  let loading = $state(false);
  let busy = $state(false);
  let lastToken = $state("");
  let customUrl = $state("");
  let port = $state(51688);
  let timerHours = $state(6);
  let adding = $state(false);
  let addProject = $state("");
  let addContext = $state("");
  let addContexts = $state<Context[]>([]);
  let addWrite = $state(false);
  let addExec = $state(false);
  let logOpen = $state(false);
  let tick = $state(0);
  let synced = false;

  const status = $derived(st?.status ?? null);
  const projects = $derived(ui.projects.filter((p) => !p.id.startsWith("00000000")));
  const publicUrl = $derived(
    status?.running
      ? (st?.exposure === "custom" && customUrl.trim() ? customUrl.trim().replace(/\/+$/, "") : status.public_url ?? "")
      : "",
  );

  async function load(quiet = false) {
    if (!quiet) loading = true;
    try {
      st = await api.remote.state();
      tick = 0;
      if (!synced) {
        synced = true;
        customUrl = st.customUrl;
        port = st.port;
        timerHours = st.timerHours;
      }
    } catch (e) {
      if (!quiet) toast("error", errText(e));
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    void load();
    const poll = setInterval(() => { if (document.visibilityState === "visible") void load(true); }, 4000);
    const sec = setInterval(() => (tick += 1), 1000);
    return () => { clearInterval(poll); clearInterval(sec); };
  });

  async function act(fn: () => Promise<unknown>, ok?: string) {
    busy = true;
    try {
      await fn();
      if (ok) toast("ok", ok);
      await load(true);
      return true;
    } catch (e) {
      toast("error", errText(e));
      return false;
    } finally {
      busy = false;
    }
  }

  async function copyText(text: string, ok: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast("ok", ok);
      return;
    } catch { /* 非 HTTPS 时退回旧接口 */ }
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.setAttribute("readonly", "");
    ta.style.cssText = "position:fixed;left:-9999px;top:0";
    document.body.appendChild(ta);
    ta.select();
    let done = false;
    try { done = document.execCommand("copy"); } catch { done = false; }
    ta.remove();
    toast(done ? "ok" : "warn", done ? ok : "当前浏览器不允许复制（需 HTTPS）");
  }

  function unavailable() {
    toast("warn", status?.running ? "接入地址尚未就绪（隧道获取中或未配置），请稍后再试" : "服务未启动，暂无接入地址");
  }

  function fmtDur(total: number) {
    const s = Math.max(0, total - Math.min(tick, total));
    return `${Math.floor(s / 3600)}:${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
  }

  function start() {
    if (st?.exposure === "quick_tunnel" && !st.cloudflared.installed) {
      toast("warn", "未找到 cloudflared：请先在电脑端「外部编程接入」中快捷安装或指定已有程序");
      return;
    }
    void act(() => api.remote.start(), "外部编程 MCP 服务已启动");
  }
  function stop() {
    if (!confirm("停止外部编程服务？已连接的外部 AI 将立即断开。")) return;
    void act(() => api.remote.stop(), "外部编程 MCP 服务已停止");
  }
  function setExposure(v: "custom" | "quick_tunnel") {
    if (st) st.exposure = v;
    void act(() => api.remote.exposure(v));
  }
  function saveCustomUrl() {
    customUrl = customUrl.trim().replace(/\/+$/, "");
    void act(() => api.remote.options({ customUrl }));
  }
  function savePort() {
    const n = Number(port);
    if (!Number.isInteger(n) || n < 1 || n > 65535) { toast("warn", "端口需为 1-65535 的整数"); port = st?.port ?? 51688; return; }
    void act(() => api.remote.options({ port: n }), "端口已保存，重启服务后生效");
  }
  function setExec(v: boolean) {
    if (v && !confirm("允许外部 AI 执行命令？\n目录限制不等于命令沙箱：开启后程序以当前系统用户权限运行（仍需授权行同时允许）。")) {
      if (st) st.execAllowed = false;
      return;
    }
    void act(() => api.remote.options({ execAllowed: v }), "全局执行开关已保存");
  }
  function applyTimer(h: number) {
    const n = Number(h);
    if (!Number.isFinite(n) || n < 0) { toast("warn", "请输入有效的小时数（0 为取消定时）"); return; }
    void act(() => api.remote.timer(n), n > 0 ? `定时停止已设置：${n} 小时后自动停止` : "定时停止已取消");
  }

  async function loadAddContexts(pid: string) {
    addContext = "";
    addContexts = pid ? await api.contexts(pid).catch(() => []) : [];
  }
  function openAdd() {
    adding = !adding;
    if (!adding) return;
    addProject = ui.projectId && projects.some((p) => p.id === ui.projectId) ? ui.projectId : projects[0]?.id ?? "";
    addWrite = false;
    addExec = false;
    void loadAddContexts(addProject);
  }
  async function create() {
    if (!addProject) { toast("warn", "请先选择项目"); return; }
    busy = true;
    try {
      const r = await api.remote.create({ projectId: addProject, contextId: addContext, fsWrite: addWrite, execAllowed: addExec });
      lastToken = r.token;
      adding = false;
      toast("ok", "授权已创建，Token 仅显示一次，请立即复制");
      await load(true);
    } catch (e) {
      toast("error", errText(e));
    } finally {
      busy = false;
    }
  }

  async function grantUrl(g: RemoteGrant): Promise<{ url: string; token: string } | null> {
    const base = publicUrl ? `${publicUrl}/mcp` : "";
    if (!base) { unavailable(); return null; }
    try {
      const { token } = await api.remote.token(g.id);
      if (!token) { toast("error", "凭据生成失败，请重试"); return null; }
      return { url: `${base}?passcode=${token}`, token };
    } catch (e) {
      toast("error", errText(e));
      return null;
    }
  }
  async function copyAddress(g: RemoteGrant) {
    const r = await grantUrl(g);
    if (r) await copyText(r.url, "接入地址已复制");
  }
  async function copyPrompt(g: RemoteGrant) {
    const r = await grantUrl(g);
    if (!r) return;
    const lines = [
      `MCP Server URL：${r.url}`,
      "",
      "这是 ShiDrive Coding MCP，你可以借助它完成已授权项目的远端开发任务。",
      `项目: ${g.project_name}`,
      `开放目录: ${g.project_root}`,
      `文件权限: ${g.fs_write ? "读写" : "只读"}`,
      `命令执行: ${g.exec_allowed ? "允许" : "禁用"}`,
      "",
      "认证凭据（服务重启或「继续开放」时自动刷新，届时请重新复制提示词）：",
      r.token,
      "请求认证二选一：Authorization: Bearer <token> 请求头，或 URL 追加 ?passcode=<token>。",
      "",
      "请先完成 MCP 初始化并读取工具列表，再在授权范围内开展工作。",
      "执行删除、覆盖、发布等高风险操作前，请先征得我的确认。",
      "本链接包含临时访问凭据，请勿写入项目文件、提交记录或日志。",
      "注意：服务或隧道重启后接入地址会变化，届时请向我索取新地址。",
    ];
    if (g.context_enabled && g.context_name) {
      lines.push(
        "",
        `此接入已开放共享上下文，绑定上下文：${g.context_name}`,
        "开始任务前，调用 context_get 获取最新目标、约束、待办与版本。",
        "完成一个阶段后，记录进展、关键决策、未完成事项及涉及文件。",
        "提交时使用读取到的 base_version，只更新实际修改的部分。",
        "如果版本冲突，请重新读取、合并后提交，不要强行覆盖。",
        "共享上下文用于工作交接，不代替代码版本管理。",
      );
    }
    await copyText(lines.join("\n"), "接入提示词已复制");
  }
  function remove(g: RemoteGrant) {
    if (!confirm(`删除「${g.project_name}」的授权记录？使用该凭据的外部连接将失效。`)) return;
    void act(() => api.remote.remove(g.id), "授权记录已删除");
  }
  function decide(txnId: string, approve: boolean) {
    void act(() => api.remote.oauthDecide(txnId, approve), approve ? "已批准授权" : "已拒绝授权");
  }
  function revokeOauth(id: string) {
    if (!confirm("吊销该 OAuth 客户端？其访问令牌将立即失效。")) return;
    void act(() => api.remote.oauthRevoke(id), "OAuth 令牌已吊销");
  }
</script>

<div class="pane scroll">
  <header class="head">
    <div class="title">
      <h2>外部编程接入</h2>
      <span class="faint small">把项目目录 / 共享上下文以 MCP 开放给外部 AI 编程工具</span>
    </div>
    <button class="icon-btn" title="刷新" onclick={() => load()} disabled={loading}>
      {#if loading}<span class="spin"></span>{:else}<Icon name="refresh" />{/if}
    </button>
  </header>

  {#if !st}
    <div class="empty">{#if loading}<span class="spin"></span>加载中…{:else}暂无数据{/if}</div>
  {:else}
    <section class="card sec">
      <div class="row">
        <div class="stat">
          <span class="dot" class:on={status?.running}></span>
          <b>{status?.running ? "运行中" : "未启动"}</b>
          <span class="faint">· 隧道 {status?.tunnel_running ? "已连接" : "未连接"} · 授权 {status?.grants_active ?? 0}</span>
        </div>
        {#if status?.running}
          <button class="btn sm" disabled={busy} onclick={stop}>停止服务</button>
        {:else}
          <button class="btn sm primary" disabled={busy} onclick={start}>启动服务</button>
        {/if}
      </div>
      <div class="row addr">
        <span class="faint">接入地址</span>
        {#if publicUrl}
          <code class="mono url">{publicUrl}/mcp</code>
        {:else if st.exposure === "quick_tunnel" && status?.running && status?.tunnel_running}
          <span class="faint pulse">正在获取隧道地址…</span>
        {:else}
          <code class="mono url">—</code>
        {/if}
        <button class="btn ghost sm" disabled={!publicUrl} onclick={() => copyText(`${publicUrl}/mcp`, "接入地址已复制")}><Icon name="copy" size={14} />复制</button>
      </div>
      <div class="row timer">
        <span class="faint">⏱ 定时停止</span>
        <input class="input num" type="number" min="0.1" step="0.5" bind:value={timerHours} />
        <span class="faint">小时</span>
        <button class="btn sm" disabled={!status?.running || busy} onclick={() => applyTimer(timerHours)}>设置并重新计时</button>
        <button class="btn ghost sm" disabled={(status?.timer_stop_in_secs ?? -1) < 0 || busy} onclick={() => applyTimer(0)}>取消定时</button>
        {#if (status?.timer_stop_in_secs ?? -1) >= 0}<span class="badge run mono">剩余 {fmtDur(status!.timer_stop_in_secs)}</span>{/if}
      </div>
    </section>

    {#if lastToken}
      <section class="card sec token">
        <div class="warn-t">⚠ 新授权 Token（仅此一次显示，请立即复制）</div>
        <code class="mono">{lastToken}</code>
        <div class="row end">
          <button class="btn sm" onclick={() => copyText(lastToken, "Token 已复制")}>复制</button>
          <button class="btn ghost sm" onclick={() => (lastToken = "")}>关闭</button>
        </div>
      </section>
    {/if}

    {#if st.oauthPending.length}
      <section class="card sec pending">
        <h3>待授权的 OAuth 请求</h3>
        {#each st.oauthPending as p (p.txnId)}
          <div class="item">
            <div class="grow">
              <b>{p.clientName || p.clientId}</b>
              <div class="faint small mono">{p.scopes.join(" ")} · {p.redirectUri}</div>
            </div>
            <button class="btn sm primary" disabled={busy} onclick={() => decide(p.txnId, true)}>批准</button>
            <button class="btn sm" disabled={busy} onclick={() => decide(p.txnId, false)}>拒绝</button>
          </div>
        {/each}
      </section>
    {/if}

    <section class="card sec">
      <h3>接入方式</h3>
      <div class="seg">
        <button class:on={st.exposure === "quick_tunnel"} onclick={() => setExposure("quick_tunnel")}>Cloudflare 临时隧道</button>
        <button class:on={st.exposure === "custom"} onclick={() => setExposure("custom")}>自定义地址 / frp / 反代</button>
      </div>
      {#if st.exposure === "custom"}
        <input class="input" placeholder="https://your-domain.com（已有 frp / 反向代理）" bind:value={customUrl} onchange={saveCustomUrl} />
      {:else}
        <p class="note">
          cloudflared：{#if st.cloudflared.installed}<span class="badge ok">已检测{st.cloudflared.version ? ` · ${st.cloudflared.version}` : ""}</span>{:else}<span class="badge warn">未安装</span> 请在电脑端快捷安装或指定已有程序{/if}
        </p>
        <p class="note">临时隧道地址在服务重启后会变化，重新复制提示词即可获取最新信息。</p>
      {/if}
      <div class="row start">
        <span class="faint">服务端口</span>
        <input class="input num" type="number" min="1" max="65535" bind:value={port} onchange={savePort} />
        <span class="faint small">修改后重启服务生效</span>
      </div>
      <label class="check">
        <input type="checkbox" checked={st.execAllowed} onchange={(e) => setExec((e.currentTarget as HTMLInputElement).checked)} />
        允许执行命令（全局开关；最终权限 = 全局 AND 授权行）
      </label>
    </section>

    <section class="card sec">
      <div class="row">
        <h3>开放目录与上下文</h3>
        <button class="btn sm" onclick={openAdd}>{adding ? "收起" : "添加授权"}</button>
      </div>
      {#if adding}
        <div class="form">
          <label class="faint small" for="rm-proj">项目</label>
          <select id="rm-proj" class="select" bind:value={addProject} onchange={() => loadAddContexts(addProject)}>
            {#each projects as p (p.id)}<option value={p.id}>{p.name} · {p.root_path}</option>{/each}
          </select>
          <label class="faint small" for="rm-ctx">共享上下文</label>
          <select id="rm-ctx" class="select" bind:value={addContext}>
            <option value="">（不开放）</option>
            {#each addContexts as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
          </select>
          <label class="check"><input type="checkbox" bind:checked={addWrite} /> 文件读写（默认只读）</label>
          <label class="check"><input type="checkbox" bind:checked={addExec} /> 允许命令执行</label>
          <div class="row end">
            <button class="btn sm primary" disabled={busy || !addProject} onclick={create}>创建授权</button>
            <button class="btn sm" onclick={() => (adding = false)}>取消</button>
          </div>
        </div>
      {/if}
      {#each st.grants as g (g.id)}
        <div class="grant" class:off={!!g.revoked_at || !!g.paused_at}>
          <div class="gtop">
            <b class="ellipsis">{g.project_name}</b>
            <span class="badge {g.revoked_at ? 'err' : g.paused_at ? 'warn' : 'ok'}">{g.revoked_at ? "已撤销" : g.paused_at ? "已暂停" : "有效"}</span>
          </div>
          <div class="faint small mono ellipsis" title={g.project_root}>{g.project_root}</div>
          <div class="small muted">
            上下文：{g.context_enabled ? g.context_name || "已开放" : "未开放"} · {g.fs_write ? "读写" : "只读"}{g.exec_allowed ? " · 可执行" : ""}
          </div>
          <div class="gops">
            {#if !g.revoked_at && !g.paused_at}
              <button class="btn ghost sm" onclick={() => copyPrompt(g)}>复制提示词</button>
              <button class="btn ghost sm" onclick={() => copyAddress(g)}>复制仅地址</button>
              <button class="btn ghost sm" disabled={busy} onclick={() => act(() => api.remote.pause(g.id), "已暂停开放，外部连接将被拒绝")}>暂停开放</button>
            {:else if g.paused_at}
              <button class="btn ghost sm" disabled={busy} onclick={() => act(() => api.remote.resume(g.id), "已继续开放，凭据已刷新，请重新复制提示词")}>继续开放</button>
            {/if}
            <button class="btn ghost sm danger-t" disabled={busy} onclick={() => remove(g)}>删除</button>
          </div>
        </div>
      {:else}
        <div class="faint small none">暂无授权。添加授权后复制接入提示词发给外部 AI。</div>
      {/each}
    </section>

    <section class="card sec">
      <h3>OAuth 客户端</h3>
      {#each st.oauthTokens.filter((t) => !t.revoked_at) as tk (tk.id)}
        <div class="item">
          <div class="grow">
            <b>{tk.client_name || tk.client_id}</b>
            <div class="faint small mono">{tk.scopes} · 最近使用 {tk.last_used_at ?? "—"}</div>
          </div>
          <button class="btn ghost sm danger-t" disabled={busy} onclick={() => revokeOauth(tk.id)}>吊销</button>
        </div>
      {:else}
        <div class="faint small none">暂无 OAuth 客户端</div>
      {/each}
    </section>

    <section class="card sec">
      <div class="row">
        <h3>运行日志</h3>
        <button class="btn ghost sm" onclick={() => (logOpen = !logOpen)}>{logOpen ? "隐藏日志" : "查看日志"}</button>
      </div>
      {#if logOpen}
        <pre class="log mono">{(status?.logs ?? []).join("\n") || "暂无日志"}</pre>
      {/if}
    </section>
  {/if}
</div>

<style>
  .pane { height: 100%; padding: 16px 16px calc(24px + var(--safe-b)); display: flex; flex-direction: column; gap: 12px; max-width: 920px; margin: 0 auto; width: 100%; }
  .head { display: flex; align-items: center; gap: 10px; }
  .title { flex: 1; min-width: 0; }
  .title h2 { margin: 0; font-size: 19px; }
  .small { font-size: 12px; }
  .sec { padding: 14px 16px; display: flex; flex-direction: column; gap: 10px; }
  .sec h3 { margin: 0; font-size: 14px; color: var(--text-dim); }
  .row { display: flex; align-items: center; justify-content: space-between; gap: 8px; flex-wrap: wrap; }
  .row.start, .row.timer, .row.addr { justify-content: flex-start; }
  .row.end { justify-content: flex-end; }
  .stat { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .dot { width: 9px; height: 9px; border-radius: 50%; background: var(--text-faint); }
  .dot.on { background: var(--ok); box-shadow: 0 0 0 3px color-mix(in srgb, var(--ok) 25%, transparent); }
  .url { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 13px; }
  .num { width: 96px; min-height: 30px; padding: 4px 8px; }
  .seg { display: flex; gap: 6px; flex-wrap: wrap; }
  .seg button { padding: 6px 12px; border-radius: 999px; border: 1px solid var(--border); background: var(--bg-elev); font-size: 13px; }
  .seg button.on { border-color: var(--accent); color: var(--accent); background: var(--accent-soft); }
  .note { font-size: 13px; color: var(--text-dim); margin: 0; }
  .check { display: flex; align-items: center; gap: 8px; font-size: 13.5px; cursor: pointer; }
  .form { display: flex; flex-direction: column; gap: 8px; padding: 12px; border-radius: 10px; background: var(--bg-elev); }
  .grant { border-top: 1px solid var(--border-soft); padding-top: 10px; display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .grant.off { opacity: .7; }
  .gtop { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .ellipsis { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .gops { display: flex; flex-wrap: wrap; gap: 4px; margin-top: 4px; }
  .danger-t { color: var(--danger); }
  .item { display: flex; align-items: center; gap: 8px; border-top: 1px solid var(--border-soft); padding-top: 8px; }
  .item:first-of-type { border-top: none; padding-top: 0; }
  .grow { flex: 1; min-width: 0; }
  .token { border-color: var(--warn); }
  .token code { word-break: break-all; user-select: all; font-size: 13px; }
  .warn-t { color: var(--warn); font-size: 13.5px; }
  .pending { border-color: var(--accent); }
  .none { padding: 6px 0; }
  .log { margin: 0; max-height: 320px; overflow: auto; padding: 10px; border-radius: 8px; background: var(--code-bg); font-size: 12px; white-space: pre-wrap; word-break: break-all; user-select: text; }
</style>
