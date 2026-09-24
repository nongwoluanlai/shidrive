<script lang="ts">
  // 设置 → 外部编程接入（MCP）：远端服务生命周期 + 项目授权列表 + 临时隧道
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import { t } from "../i18n";
  import type { Context, OAuthToken } from "../types";

  interface RemoteStatus {
    running: boolean;
    port: number;
    started_at: string | null;
    public_url: string | null;
    grants_active: number;
    tunnel_running: boolean;
    logs: string[];
    timer_stop_in_secs: number;
  }
  interface RemoteGrant {
    id: string;
    project_id: string;
    project_name: string;
    project_root: string;
    context_id: string | null;
    context_name: string;
    context_enabled: boolean;
    fs_write: boolean;
    exec_allowed: boolean;
    created_at: string;
    revoked_at: string | null;
    paused_at: string | null;
  }

  let status = $state<RemoteStatus | null>(null);
  let grants = $state<RemoteGrant[]>([]);
  let busy = $state(false);
  let exposure = $state<"custom" | "quick_tunnel">("quick_tunnel");
  let customUrl = $state("");
  let lastToken = $state("");
  // 服务监听端口（自定义穿透/反代需要指定端口）；持久化，默认 51688
  let port = $state(51688);
  // 全局「允许执行命令」开关（remote.exec_allowed）：此前误绑到添加表单的 addExec，
  // 且从不写库，导致后端要求的「全局 AND 授权行」永远无法同时为真
  let globalExec = $state(false);
  let adding = $state(false);
  let addProjectId = $state("");
  let addContextId = $state("");
  let formContexts = $state<Context[]>([]);
  let addFsWrite = $state(false);
  let addExec = $state(false);
  let logOpen = $state(false);
  // 定时停止：小时数（配置）+ 本地每秒倒计时（3s 轮询之外保持显示活性）
  let timerHours = $state(6);
  let timerTick = $state(0);
  // OAuth 待授权弹窗全局常驻；这里仅保留客户端列表及吊销入口。
  let oauthTokens = $state<OAuthToken[]>([]);
  let cf = $state<{ installed: boolean; path: string; version: string } | null>(null);
  let cfBusy = $state(false);

  const projects = $derived(app.projects.filter((p) => !p.id.startsWith("00000000")));
  const publicUrl = $derived(status?.running ? (exposure === "custom" && customUrl.trim() ? customUrl.trim().replace(/\/+$/, "") : status.public_url ?? "") : "");

  async function refresh() {
    status = await api.remoteMcpStatus().catch(() => null);
    timerTick = 0;
    try {
      grants = (await api.remoteGrantsList()) as unknown as RemoteGrant[];
    } catch {
      grants = [];
    }
    void refreshOauthTokens();
  }

  async function refreshOauthTokens() {
    oauthTokens = await api.remoteOauthTokensList().catch(() => []);
  }

  async function refreshCf() {
    cf = await api.remoteCloudflaredStatus().catch(() => null);
  }

  onMount(() => {
    void refresh();
    void refreshCf();
    void api.settingsGet("remote.exposure")
      .then((v) => { if (v === "custom" || v === "quick_tunnel") exposure = v; })
      .catch(() => {});
    void api.settingsGet("remote.auto_stop_hours")
      .then((v) => { const n = Number(v); if (Number.isFinite(n) && n > 0) timerHours = n; })
      .catch(() => {});
    const unTimer = listen("remote://timer-stop", () => {
      toast("warn", t("定时停止已触发，外部编程服务已停止"));
      void refresh();
    });
    const tId = setInterval(() => (timerTick += 1), 1000);
    void api.settingsGet("remote.exec_allowed")
      .then((v) => { globalExec = v === "1"; })
      .catch(() => {});
    void api.settingsGet("remote.custom_url")
      .then((v) => { if (typeof v === "string" && v) customUrl = v; })
      .catch(() => {});
    void api.settingsGet("remote.port")
      .then((v) => { const n = Number(v); if (Number.isInteger(n) && n >= 1 && n <= 65535) port = n; })
      .catch(() => {});
    const un = listen("remote://tunnel", (e) => {
      const d = (e.payload ?? {}) as { publicUrl?: string; error?: string };
      if (d.publicUrl) { void refresh(); toast("ok", t("隧道地址已获取")); }
      else if (d.error) { toast("error", t("隧道地址获取失败，请查看运行日志")); void refresh(); }
    });
    const oauthPoll = setInterval(() => void refreshOauthTokens(), 4000);
    return () => {
      void un.then((f) => f());
      void unTimer.then((f) => f());
      clearInterval(tId);
      clearInterval(oauthPoll);
    };
  });

  async function oauthRevoke(id: string) {
    try {
      await api.remoteOauthTokenRevoke(id);
      await refreshOauthTokens();
      toast("ok", t("OAuth 令牌已吊销"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  // 切换接入方式：记住选择；服务运行中即时起/停隧道
  async function onExposureChange(v: "custom" | "quick_tunnel") {
    exposure = v;
    void api.settingsSet("remote.exposure", v).catch(() => {});
    if (!status?.running) return;
    try {
      if (v === "quick_tunnel") {
        cf = await api.remoteCloudflaredStatus().catch(() => null);
        if (cf && !cf.installed) { toast("warn", t("未找到 cloudflared：请先「快捷安装」或「指定已有程序」")); return; }
        toast("info", t("正在获取隧道地址…"));
        await api.remoteTunnelStart();
      } else {
        await api.remoteTunnelStop();
      }
      await refresh();
    } catch (e) {
      toast("error", String(e));
    }
  }

  // 选用 Cloudflare 临时隧道时立即检测环境；未安装则提示安装入口
  $effect(() => {
    if (exposure === "quick_tunnel") void refreshCf();
  });

  // 全局「允许执行命令」开关：持久化到设置（最终权限 = 全局 AND 授权行）
  async function setGlobalExec(v: boolean) {
    const prev = globalExec;
    globalExec = v;
    try {
      await api.settingsSet("remote.exec_allowed", v ? "1" : "0");
      toast("ok", t("全局执行开关已保存"));
    } catch (e) {
      globalExec = prev;
      toast("error", String(e));
    }
  }

  // 自定义地址持久化：重启后无需重输
  function persistCustomUrl() {
    const v = customUrl.trim().replace(/\/+$/, "");
    customUrl = v;
    void api.settingsSet("remote.custom_url", v).catch(() => {});
  }

  // 接入地址不可用时的区分提示：服务未启动 vs 地址还没拿到
  function unavailableReason(): string {
    if (!status?.running) return t("服务未启动，暂无接入地址");
    return t("接入地址尚未就绪（隧道获取中或未配置），请稍后再试");
  }

  async function copyText(text: string, okMsg: string) {
    try {
      await navigator.clipboard.writeText(text);
      toast("ok", okMsg);
    } catch {
      toast("error", t("复制失败"));
    }
  }

  async function installCloudflared() {
    cfBusy = true;
    try {
      toast("info", t("正在下载 cloudflared（约 15MB）…"));
      const msg = await api.remoteCloudflaredInstall();
      toast("ok", msg);
      await refreshCf();
    } catch (e) {
      toast("error", String(e));
    } finally {
      cfBusy = false;
    }
  }

  async function pickCloudflared() {
    const initial = cf?.path ?? "";
    const p = await import("../dialog.svelte").then((m) =>
      m.promptDialog({ title: t("指定 cloudflared"), label: t("cloudflared.exe 的完整路径"), initial }),
    );
    if (p === null) return;
    try {
      await api.remoteCloudflaredSetPath(p.trim());
      toast("ok", t("已保存 cloudflared 路径"));
      await refreshCf();
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function startService() {
    if (exposure === "quick_tunnel") {
      cf = await api.remoteCloudflaredStatus().catch(() => null);
      if (cf && !cf.installed) {
        toast("warn", t("未找到 cloudflared：请先「快捷安装」或「指定已有程序」"));
        return;
      }
    }
    busy = true;
    try {
      await api.remoteMcpStart({ listenHost: "0.0.0.0", port, quickTunnel: exposure === "quick_tunnel" });
      await refresh();
      toast("ok", t("外部编程 MCP 服务已启动"));
    } catch (e) {
      toast("error", String(e));
    } finally {
      busy = false;
    }
  }

  // 定时停止：>0 设置并重新计时（持久化配置，下次启动沿用）；0 取消
  async function applyTimer(hours: number) {
    const h = Number(hours);
    if (!Number.isFinite(h) || h < 0) {
      toast("warn", t("请输入有效的小时数（0 为取消定时）"));
      return;
    }
    try {
      await api.remoteTimerSet(h);
      await refresh();
      toast("ok", hours > 0 ? t("定时停止已设置：{h} 小时后自动停止", { h: String(hours) }) : t("定时停止已取消"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  function fmtDur(totalSecs: number): string {
    const secs = Math.max(0, totalSecs - Math.min(timerTick, totalSecs));
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    const ss = secs % 60;
    return `${h}:${String(m).padStart(2, "0")}:${String(ss).padStart(2, "0")}`;
  }

  async function stopService() {
    busy = true;
    try {
      await api.remoteMcpStop();
      await refresh();
      toast("ok", t("外部编程 MCP 服务已停止"));
    } catch (e) {
      toast("error", String(e));
    } finally {
      busy = false;
    }
  }

  async function copyAddress() {
    const url = publicUrl ? `${publicUrl}/mcp` : "";
    if (!url) { toast("warn", unavailableReason()); return; }
    try { await navigator.clipboard.writeText(url); toast("ok", t("接入地址已复制")); } catch { toast("error", t("复制失败")); }
  }

  function startAdd() {
    adding = true;
    addProjectId = projects[0]?.id ?? "";
    addContextId = "";
    addFsWrite = false;
    addExec = false;
    void loadFormContexts(addProjectId);
  }

  // 按项目载入上下文（选择上下文 = 一并开放该共享上下文的 MCP 工具）
  async function loadFormContexts(projectId: string) {
    formContexts = projectId ? await api.contextsList(projectId).catch(() => []) : [];
    if (!formContexts.some((c) => c.id === addContextId)) addContextId = "";
  }

  function cancelAdd() {
    adding = false;
  }

  async function saveAdd() {
    const project = app.projects.find((p) => p.id === addProjectId);
    if (!project) { toast("warn", t("请先选择项目")); return; }
    busy = true;
    try {
      const ctx = formContexts.find((c) => c.id === addContextId) ?? null;
      const res = await api.remoteGrantCreate({
        projectId: project.id,
        projectName: project.name,
        projectRoot: project.root_path,
        contextId: ctx?.id ?? null,
        contextName: ctx?.name ?? "",
        contextEnabled: !!ctx,
        fsWrite: addFsWrite,
        execAllowed: addExec,
      });
      const token = (res as { token?: string }).token ?? "";
      lastToken = token;
      adding = false;
      await refresh();
      toast("ok", t("授权已创建，Token 仅显示一次，请立即复制"));
    } catch (e) {
      toast("error", String(e));
    } finally {
      busy = false;
    }
  }

  // 暂停开放：暂停期间外部连接一律 403（可逆）
  async function pauseGrant(id: string) {
    try {
      await api.remoteGrantPause(id);
      await refresh();
      toast("ok", t("已暂停开放，外部连接将被拒绝"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  // 继续开放：清除暂停标记并刷新凭据 → 提示重新复制提示词
  async function resumeGrant(id: string) {
    try {
      await api.remoteGrantResume(id);
      await refresh();
      toast("ok", t("已继续开放，凭据已刷新，请重新复制提示词"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function deleteGrant(id: string) {
    try {
      await api.remoteGrantDelete(id);
      await refresh();
      toast("ok", t("授权记录已删除"));
    } catch (e) {
      toast("error", String(e));
    }
  }

  // 复制接入提示词：取当前有效凭据（不轮换）。凭据刷新时机：
  // 服务启动（全部授权）/「暂停开放→继续开放」切换（单个授权），见暂停/继续按钮。
  async function copyPrompt(g: RemoteGrant) {
    const base = publicUrl ? `${publicUrl}/mcp` : "";
    if (!base) { toast("warn", unavailableReason()); return; }
    let token = "";
    try {
      const res = await api.remoteGrantToken(g.id);
      token = (res as { token?: string }).token ?? "";
    } catch (e) {
      toast("error", String(e));
      return;
    }
    if (!token) { toast("error", t("凭据生成失败，请重试")); return; }
    const lines = [
      `MCP Server URL：${base}?passcode=${token}`,
      "",
      t("这是 ShiDrive Coding MCP，你可以借助它完成已授权项目的远端开发任务。"),
      `${t("项目")}: ${g.project_name}`,
      `${t("开放目录")}: ${g.project_root}`,
      `${t("文件权限")}: ${g.fs_write ? t("读写") : t("只读")}`,
      `${t("命令执行")}: ${g.exec_allowed ? t("允许") : t("禁用")}`,
      "",
      t("认证凭据（服务重启或「继续开放」时自动刷新，届时请重新复制提示词）："),
      token,
      t("请求认证二选一：Authorization: Bearer <token> 请求头，或 URL 追加 ?passcode=<token>。"),
      "",
      t("请先完成 MCP 初始化并读取工具列表，再在授权范围内开展工作。"),
      t("执行删除、覆盖、发布等高风险操作前，请先征得我的确认。"),
      t("本链接包含临时访问凭据，请勿写入项目文件、提交记录或日志。"),
      t("注意：服务或隧道重启后接入地址会变化，届时请向我索取新地址。"),
    ];
    if (g.context_enabled && g.context_name) {
      lines.push("", t("此接入已开放共享上下文，绑定上下文：{name}", { name: g.context_name }));
      lines.push(t("开始任务前，调用 context_get 获取最新目标、约束、待办与版本。"));
      lines.push(t("完成一个阶段后，记录进展、关键决策、未完成事项及涉及文件。"));
      lines.push(t("提交时使用读取到的 base_version，只更新实际修改的部分。"));
      lines.push(t("如果版本冲突，请重新读取、合并后提交，不要强行覆盖。"));
      lines.push(t("共享上下文用于工作交接，不代替代码版本管理。"));
    }
    void navigator.clipboard.writeText(lines.join("\n")).then(
      () => toast("ok", t("接入提示词已复制")),
      () => toast("error", t("复制失败")),
    );
  }
</script>

<section class="remote-mcp">
  <div class="status-row">
    <span class="dot {status?.running ? 'on' : 'off'}"></span>
    <span>{t("服务")}：{status?.running ? t("运行中") : t("未启动")}</span>
    <span class="sep">·</span>
    <span>{t("隧道")}：{status?.tunnel_running ? t("已连接") : t("未连接")}</span>
    <span class="sep">·</span>
    <span>{t("授权")}：{status?.grants_active ?? 0}</span>
    <span class="grow"></span>
    {#if status?.running}
      <button class="btn sm" onclick={stopService}>{t("停止服务")}</button>
    {:else}
      <button class="btn sm primary" onclick={startService}>{t("启动服务")}</button>
    {/if}
  </div>

  <div class="row timer-row">
    <span class="lbl">⏱ {t("定时停止")}</span>
    <input
      class="timer-hours"
      type="number"
      min="0.1"
      step="0.5"
      bind:value={timerHours}
      title={t("单位：小时；服务每次启动后按此时间自动停止")}
    />
    <span class="lbl">{t("小时")}</span>
    <button class="btn sm" disabled={!status?.running} title={status?.running ? "" : t("服务未启动")} onclick={() => void applyTimer(Number(timerHours))}>{t("设置并重新计时")}</button>
    <button class="btn ghost sm" disabled={(status?.timer_stop_in_secs ?? -1) < 0} onclick={() => void applyTimer(0)}>{t("取消定时")}</button>
    {#if (status?.timer_stop_in_secs ?? -1) >= 0}
      <span class="mono timer-left" title={t("距离自动停止的剩余时间")}>{t("剩余 {t}", { t: fmtDur(status!.timer_stop_in_secs) })}</span>
    {:else}
      <span class="dim">{t("未启用（服务启动时默认按上方小时数自动计时）")}</span>
    {/if}
  </div>

  {#if lastToken}
    <div class="token-banner">
      <span class="warn-txt">⚠ {t("新授权 Token（仅此一次显示，请立即复制）")}：</span>
      <code>{lastToken}</code>
      <button class="btn sm" onclick={() => void copyText(lastToken, t("Token 已复制"))}>{t("复制")}</button>
      <button class="btn ghost sm" onclick={() => (lastToken = "")}>{t("关闭")}</button>
    </div>
  {/if}

  <div class="sec">
    <h3>{t("接入方式")}</h3>
    <div class="row">
      <label class="radio"><input type="radio" checked={exposure === "custom"} onchange={() => void onExposureChange("custom")} /> {t("自定义地址 / 已有 frp 或反代")}</label>
      <label class="radio"><input type="radio" checked={exposure === "quick_tunnel"} onchange={() => void onExposureChange("quick_tunnel")} /> {t("Cloudflare 临时隧道")}</label>
    </div>
    {#if exposure === "custom"}
      <div class="row">
        <input class="grow" placeholder={t("https://your-domain.com（已有 frp / 反向代理）")} bind:value={customUrl} onchange={persistCustomUrl} />
        <button class="btn sm" onclick={copyAddress} disabled={!publicUrl}>{t("复制接入地址")}</button>
      </div>
    {:else}
      <p class="note">{t("Cloudflare Tunnel 为免费临时地址，重启服务后刷新，重新复制提示词可获取最新的信息。如果cloudflare安装速度慢，可以试着在环境与路径栏目下配置HTTP代理进行加速")}</p>
      <div class="row cf-row">
        <span class="lbl">{t("cloudflared")}：</span>
        {#if cf?.installed}
          <span class="badge ok">{t("已检测")}{cf.version ? ` · ${cf.version}` : ""}</span>
          <span class="dim mono cf-path" title={cf.path}>{cf.path}</span>
        {:else}
          <span class="badge warn">{t("未安装")}</span>
          <button class="btn sm primary" disabled={cfBusy} onclick={installCloudflared}>{cfBusy ? t("下载中…") : t("快捷安装")}</button>
        {/if}
        <button class="btn sm" onclick={pickCloudflared}>{t("指定已有程序")}</button>
      </div>
    {/if}
    <div class="row">
      <span class="lbl">{t("接入地址")}：</span>
      {#if publicUrl}
        <code>{publicUrl}/mcp</code>
      {:else if exposure === "quick_tunnel" && status?.running && status?.tunnel_running}
        <span class="pend">{t("正在获取隧道地址…")}</span>
      {:else}
        <code>—</code>
      {/if}
      <button class="btn ghost sm" onclick={copyAddress} disabled={!publicUrl}>{t("复制")}</button>
    </div>
    <div class="row">
      <label class="lbl">{t("服务端口")}</label>
      <input
        class="port-input"
        type="number"
        min="1"
        max="65535"
        value={port}
        onchange={(e) => {
          const n = Number((e.currentTarget as HTMLInputElement).value);
          if (Number.isInteger(n) && n >= 1 && n <= 65535) {
            port = n;
            void api.settingsSet("remote.port", String(n)).catch(() => {});
            toast("info", t("端口已保存，重启服务后生效"));
          } else {
            toast("warn", t("端口需为 1-65535 的整数"));
            (e.currentTarget as HTMLInputElement).value = String(port);
          }
        }}
      />
      <span class="pend">{t("自定义穿透 / 反向代理时按需指定；修改后重启服务生效")}</span>
    </div>
  </div>

  <div class="sec">
    <div class="sec-head-row">
      <h3>{t("OAuth 客户端")}</h3>
      {#if app.oauthPending.length}
        <button class="btn sm primary" onclick={() => (app.oauthConsentOpen = true)}>{t("待授权")}（{app.oauthPending.length}）</button>
      {/if}
    </div>
    <p class="note">{t("OAuth 客户端可通过接入地址自动发现授权流程。批准后复用下方开放目录与上下文；实际能力受请求 scope、授权权限及全局执行开关共同限制。")}</p>
    <div class="tbl-wrap">
      <table class="tbl">
        <thead>
          <tr><th>{t("客户端")}</th><th>{t("权限")}</th><th>{t("最近使用")}</th><th>{t("操作")}</th></tr>
        </thead>
        <tbody>
          {#each oauthTokens as tk (tk.id)}
            <tr class:revoked={!!tk.revoked_at}>
              <td>{tk.client_name || tk.client_id}{#if tk.client_name}<br /><span class="dim mono">{tk.client_id}</span>{/if}</td>
              <td><span class="mono">{tk.scopes}</span></td>
              <td>{tk.last_used_at ?? "—"}</td>
              <td class="op">
                {#if tk.revoked_at}
                  {t("已吊销")}
                {:else}
                  <button class="btn ghost sm" onclick={() => void oauthRevoke(tk.id)}>{t("吊销")}</button>
                {/if}
              </td>
            </tr>
          {:else}
            <tr><td colspan="4" class="none">{t("暂无 OAuth 客户端")}</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  </div>

  <div class="sec">
    <h3>{t("全局选项")}</h3>
    <label class="check"><input type="checkbox" checked={globalExec} onchange={(e) => void setGlobalExec(e.currentTarget.checked)} /> {t("允许执行命令（默认关闭；最终权限 = 全局 AND 授权行）")}</label>
    <p class="note">{t("目录限制不等于命令沙箱：开启命令执行后，程序以当前系统用户权限运行。")}</p>
  </div>

  <div class="sec">
    <div class="sec-head-row">
      <h3>{t("开放目录与上下文")}</h3>
      <button class="btn sm" onclick={() => (adding = !adding)}>{adding ? t("收起") : t("添加授权")}</button>
    </div>
    {#if adding}
      <div class="add-form">
        <div class="row">
          <label class="lbl">{t("项目")}</label>
          <select
            bind:value={addProjectId}
            onchange={() => { addContextId = ""; void loadFormContexts(addProjectId); }}
          >
            {#each projects as p (p.id)}
              <option value={p.id}>{p.name} · {p.root_path}</option>
            {/each}
          </select>
        </div>
        <div class="row">
          <label class="lbl">{t("共享上下文")}</label>
          <select bind:value={addContextId}>
            <option value="">{t("（不开放）")}</option>
            {#each formContexts as c (c.id)}
              <option value={c.id}>{c.name}</option>
            {/each}
          </select>
          <span class="pend">{addContextId ? t("已绑定：远端可直接读写该上下文") : t("选择上下文后，远端可获得其 MCP 读写工具")}</span>
        </div>
        <label class="check"><input type="checkbox" bind:checked={addFsWrite} /> {t("文件读写（默认只读）")}</label>
        <label class="check"><input type="checkbox" bind:checked={addExec} /> {t("允许命令执行")}</label>
        <div class="rowbtns">
          <button class="btn sm primary" onclick={saveAdd}>{t("创建授权")}</button>
          <button class="btn sm" onclick={cancelAdd}>{t("取消")}</button>
        </div>
      </div>
    {/if}
    <div class="tbl-wrap">
      <table class="tbl">
        <thead>
          <tr><th>{t("项目 / 目录")}</th><th>{t("上下文")}</th><th>{t("权限")}</th><th>{t("状态")}</th><th>{t("操作")}</th></tr>
        </thead>
        <tbody>
          {#each grants as g (g.id)}
            <tr class:revoked={!!g.revoked_at} class:paused={!!g.paused_at && !g.revoked_at}>
              <td>{g.project_name}<br /><span class="dim mono">{g.project_root}</span></td>
              <td>{g.context_enabled ? (g.context_name || t("已开放")) : t("未开放")}</td>
              <td>
                {[g.fs_write ? t("读写") : t("只读"), g.exec_allowed ? t("可执行") : null].filter(Boolean).join(" · ") || "—"}
              </td>
              <td>{g.revoked_at ? t("已撤销") : g.paused_at ? t("已暂停") : t("有效")}</td>
              <td class="op">
                {#if !g.revoked_at && !g.paused_at}
                  <button class="btn ghost sm" title={t("复制当前有效凭据的接入提示词")} onclick={() => void copyPrompt(g)}>{t("复制提示词")}</button>
                  <button class="btn ghost sm" title={t("暂停后外部连接将被拒绝，继续开放时自动刷新凭据")} onclick={() => void pauseGrant(g.id)}>{t("暂停开放")}</button>
                {:else if g.paused_at}
                  <button class="btn ghost sm" title={t("继续开放并刷新凭据，旧凭据失效")} onclick={() => void resumeGrant(g.id)}>{t("继续开放")}</button>
                {/if}
                <button class="btn ghost sm" onclick={() => deleteGrant(g.id)}>{t("删除")}</button>
              </td>
            </tr>
          {:else}
            <tr><td colspan="5" class="none">{t("暂无授权。添加授权后复制接入提示词发给外部 AI。")}</td></tr>
          {/each}
        </tbody>
      </table>
    </div>
  </div>

  <div class="sec">
    <h3>{t("运行日志")}</h3>
    <button class="btn sm" onclick={() => (logOpen = !logOpen)}>{logOpen ? t("隐藏日志") : t("查看日志")}</button>
    {#if logOpen}
      <pre class="logs">{(status?.logs ?? []).join("\n") || t("暂无日志")}</pre>
    {/if}
  </div>
</section>

<style>
  .remote-mcp { display: flex; flex-direction: column; gap: 14px; }
  .status-row { display: flex; align-items: center; gap: 8px; font-size: .92em; }
  .status-row .grow { flex: 1; }
  .dot { width: 10px; height: 10px; border-radius: 50%; display: inline-block; }
  .dot.on { background: #34c77b; box-shadow: 0 0 6px #34c77b; }
  .dot.off { background: var(--text-faint); }
  .sep { color: var(--text-faint); }
  .sec h3 { font-size: .9em; color: var(--text-dim); margin-bottom: 8px; }
  .row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-bottom: 6px; }
  .radio, .check { display: inline-flex; align-items: center; gap: 6px; font-size: .88em; color: var(--text-dim); }
  .lbl { font-size: .86em; color: var(--text-dim); }
  .grow { flex: 1; }
  code { font-family: var(--mono); font-size: .86em; user-select: text; }
  .note { font-size: .82em; color: var(--text-faint); line-height: 1.5; }
  .sec-head-row { display: flex; align-items: center; justify-content: space-between; }
  .add-form { border: 1px solid var(--border-soft); border-radius: 8px; padding: 10px; display: flex; flex-direction: column; gap: 8px; margin-top: 8px; }
  .rowbtns { display: flex; gap: 8px; }
  .tbl-wrap { overflow: auto; max-height: 300px; }
  .tbl { width: 100%; border-collapse: collapse; font-size: .86em; }
  .tbl th, .tbl td { text-align: left; padding: 6px 8px; border-bottom: 1px solid var(--border-soft); vertical-align: top; }
  .tbl thead th { color: var(--text-dim); white-space: nowrap; position: sticky; top: 0; background: var(--bg-panel); }
  .tbl tr.revoked td { opacity: .5; }
  .tbl tr.paused td { opacity: .65; }
  .dim { color: var(--text-faint); }
  .mono { font-family: var(--mono); font-size: .92em; }
  .op { white-space: nowrap; }
  .none { color: var(--text-faint); text-align: center; padding: 14px; }
  .logs { max-height: 220px; overflow: auto; background: var(--code-bg); border-radius: 6px; padding: 8px; font-family: var(--mono); font-size: .8em; user-select: text; }
  .cf-row { align-items: baseline; }
  .port-input { width: 90px; padding: 4px 8px; font-size: .86em; }
  .cf-path { max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: .82em; }
  .badge { font-size: .82em; padding: 1px 8px; border-radius: 10px; }
  .badge.ok { background: rgba(52, 199, 123, .15); color: #2c9c63; }
  .badge.warn { background: rgba(240, 170, 40, .15); color: #b57e17; }
  .timer-row { margin-top: -4px; }
  .timer-hours { width: 5.5em; }
  .timer-left { color: #2c9c63; font-variant-numeric: tabular-nums; }
  .token-banner {
    display: flex; align-items: center; gap: 8px; flex-wrap: wrap;
    border: 1px solid rgba(240, 170, 40, .4); background: rgba(240, 170, 40, .08);
    border-radius: 8px; padding: 8px 10px; font-size: .88em;
  }
  .token-banner code { user-select: text; word-break: break-all; }
  .warn-txt { color: #b57e17; font-weight: 600; }
</style>
