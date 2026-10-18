<script lang="ts">
  // 设置 → 网站接入：内置网页版服务（访客码登录），供其它设备经反代 / 隧道访问
  import { onMount } from "svelte";
  import { toast } from "../state.svelte";
  import { api, type WebStatus } from "../ipc";
  import { t } from "../i18n";
  import { confirmDialog } from "../dialog.svelte";

  let status = $state<WebStatus | null>(null);
  let busy = $state(false);
  let publicUrl = $state("");
  let port = $state(5165);
  let code = $state("");
  let showCode = $state(false);
  let logOpen = $state(false);
  let loaded = false;

  const codeDirty = $derived(!!status && code !== status.guest_code);
  const links = $derived.by(() => {
    const out: { label: string; url: string }[] = [];
    if (status?.public_url) out.push({ label: t("开放地址"), url: status.public_url });
    for (const u of status?.local_urls ?? []) out.push({ label: u.includes("127.0.0.1") ? t("本机") : t("局域网"), url: u });
    return out;
  });

  async function refresh() {
    try {
      const s = await api.webStatus();
      status = s;
      if (!loaded) {
        loaded = true;
        publicUrl = s.public_url;
        port = s.port;
        code = s.guest_code;
      }
    } catch {
      status = null;
    }
  }

  onMount(() => {
    void refresh();
    const id = setInterval(() => void refresh(), 3000);
    return () => clearInterval(id);
  });

  async function apply(p: Promise<WebStatus>, ok?: string) {
    busy = true;
    try {
      status = await p;
      if (ok) toast("ok", ok);
      return true;
    } catch (e) {
      toast("error", String(e));
      return false;
    } finally {
      busy = false;
    }
  }

  async function toggle(on: boolean) {
    if (on) {
      if (code.trim().length < 4) { toast("warn", t("请先设置访客码（至少 4 位）")); return; }
      if (codeDirty && !(await saveCode(true))) return;
      await apply(api.webStart(), t("网站接入已开启"));
    } else {
      await apply(api.webStop(), t("网站接入已关闭"));
    }
  }

  async function savePublicUrl() {
    if (!status || publicUrl.trim() === status.public_url) return;
    if (await apply(api.webConfigSet({ publicUrl: publicUrl.trim() }), t("开放地址已保存"))) publicUrl = status!.public_url;
  }

  async function savePort() {
    const n = Number(port);
    if (!Number.isInteger(n) || n < 1 || n > 65535) { toast("warn", t("端口需为 1-65535 的整数")); port = status?.port ?? 5165; return; }
    if (n === status?.port) return;
    await apply(api.webConfigSet({ port: n }), status?.running ? t("端口已保存，服务已按新端口重启") : t("端口已保存"));
  }

  async function setLan(v: boolean) {
    await apply(api.webConfigSet({ lan: v }), v ? t("已允许局域网直接访问") : t("已仅限本机监听"));
  }

  async function saveCode(silent = false): Promise<boolean> {
    const c = code.trim();
    if (c.length < 4) { toast("warn", t("访客码至少 4 位")); return false; }
    const ok = await apply(api.webConfigSet({ guestCode: c }), silent ? undefined : t("访客码已保存，已登录的访客需重新登录"));
    if (ok) code = status!.guest_code;
    return ok;
  }

  async function genCode() {
    try { code = await api.webGenerateCode(); showCode = true; } catch (e) { toast("error", String(e)); }
  }

  async function revokeAll() {
    const ok = await confirmDialog({ title: t("注销所有访客"), message: t("所有已登录的网页访客将被立即登出，需要重新输入访客码。确定继续？"), danger: true, confirmText: t("注销") });
    if (!ok) return;
    try {
      const n = await api.webRevokeAll();
      toast("ok", t("已注销 {n} 个访客会话", { n: String(n) }));
      await refresh();
    } catch (e) {
      toast("error", String(e));
    }
  }

  function copy(text: string, msg: string) {
    void navigator.clipboard.writeText(text).then(() => toast("ok", msg), () => toast("error", t("复制失败")));
  }

  function ago(s: number): string {
    if (s < 60) return t("{n} 秒前", { n: String(s) });
    if (s < 3600) return t("{n} 分钟前", { n: String(Math.floor(s / 60)) });
    if (s < 86400) return t("{n} 小时前", { n: String(Math.floor(s / 3600)) });
    return t("{n} 天前", { n: String(Math.floor(s / 86400)) });
  }
  const shortUa = (ua: string) => {
    const os = /iPhone|iPad/.test(ua) ? "iOS" : /Android/.test(ua) ? "Android" : /Windows/.test(ua) ? "Windows" : /Mac OS/.test(ua) ? "macOS" : /Linux/.test(ua) ? "Linux" : "";
    const br = /Edg\//.test(ua) ? "Edge" : /Chrome\//.test(ua) ? "Chrome" : /Firefox\//.test(ua) ? "Firefox" : /Safari\//.test(ua) ? "Safari" : "";
    return [br, os].filter(Boolean).join(" · ") || ua.slice(0, 40) || "—";
  };
</script>

<section class="web-access">
  <div class="status-row">
    <span class="dot {status?.running ? 'on' : 'off'}"></span>
    <span>{t("服务")}：{status?.running ? t("运行中") : t("未启动")}</span>
    {#if status?.running}
      <span class="sep">·</span><span class="mono">{status.listen}</span>
      <span class="sep">·</span><span>{t("在线访客")}：{status.sessions.length}</span>
    {/if}
    <span class="grow"></span>
    <label class="switch" title={t("开启后其它设备可通过网页访问本机的使驾")}>
      <input type="checkbox" checked={!!status?.running} disabled={busy || !status} onchange={(e) => void toggle(e.currentTarget.checked)} />
      <span class="slider"></span>
    </label>
  </div>

  <p class="note">{t("便于从其它设备在网站进行访问，需要自行给设备接入反代隧道地址。")}</p>

  {#if status && !status.assets_ready}
    <div class="banner warn">⚠ {t("当前运行的程序未包含网页资源（开发模式或旧版本）。请重新打包后使用网站接入。")}</div>
  {/if}
  {#if status?.last_error}
    <div class="banner err">✕ {status.last_error}</div>
  {/if}

  <div class="sec">
    <h3>{t("访问配置")}</h3>
    <div class="row">
      <span class="lbl w">{t("开放地址")}</span>
      <input class="grow" placeholder={t("https://your-domain.com 或 1.2.3.4:5165（反代 / 隧道对外地址）")} bind:value={publicUrl} onchange={savePublicUrl} />
    </div>
    <div class="row">
      <span class="lbl w">{t("本地端口")}</span>
      <input class="port-input" type="number" min="1" max="65535" bind:value={port} onchange={savePort} />
      <span class="pend">{t("反代 / 隧道请转发到 http://127.0.0.1:{port}", { port: String(status?.port ?? port) })}</span>
    </div>
    <div class="row">
      <span class="lbl w"></span>
      <label class="check"><input type="checkbox" checked={!!status?.lan} disabled={busy || !status} onchange={(e) => void setLan(e.currentTarget.checked)} /> {t("允许局域网直接访问（监听 0.0.0.0；默认仅本机）")}</label>
    </div>
    <div class="row">
      <span class="lbl w">{t("访客码")}<span class="req">*</span></span>
      <input class="code-input mono" type={showCode ? "text" : "password"} autocomplete="new-password" placeholder={t("至少 4 位，网页登录时输入")} bind:value={code} onkeydown={(e) => { if (e.key === "Enter") void saveCode(); }} />
      <button class="btn ghost sm" onclick={() => (showCode = !showCode)}>{showCode ? t("隐藏") : t("显示")}</button>
      <button class="btn ghost sm" onclick={genCode}>{t("随机生成")}</button>
      <button class="btn ghost sm" disabled={!code} onclick={() => copy(code, t("访客码已复制"))}>{t("复制")}</button>
      <button class="btn sm primary" disabled={busy || !codeDirty} onclick={() => void saveCode()}>{t("保存")}</button>
    </div>
    <p class="note">{t("修改访客码会注销所有已登录的访客。连续输错会被暂时限制登录。")}</p>
  </div>

  <div class="sec">
    <h3>{t("访问地址")}</h3>
    {#if status?.running && links.length}
      {#each links as l (l.url)}
        <div class="row">
          <span class="lbl w">{l.label}</span>
          <code class="grow">{l.url}</code>
          <button class="btn ghost sm" onclick={() => copy(l.url, t("访问地址已复制"))}>{t("复制")}</button>
        </div>
      {/each}
    {:else}
      <div class="dim">{status?.running ? t("暂无可用地址") : t("服务未启动")}</div>
    {/if}
  </div>

  <div class="sec">
    <div class="sec-head-row">
      <h3>{t("在线访客")}{status?.clients ? `（${t("实时连接")} ${status.clients}）` : ""}</h3>
      <button class="btn sm" disabled={!status?.sessions.length} onclick={revokeAll}>{t("注销所有访客")}</button>
    </div>
    <div class="tbl-wrap">
      <table class="tbl">
        <thead><tr><th>IP</th><th>{t("设备")}</th><th>{t("登录")}</th><th>{t("最近活动")}</th></tr></thead>
        <tbody>
          {#each status?.sessions ?? [] as s, i (i)}
            <tr>
              <td class="mono">{s.ip}</td>
              <td title={s.ua}>{shortUa(s.ua)}</td>
              <td>{ago(s.loginSecsAgo)}</td>
              <td>{ago(s.activeSecsAgo)}</td>
            </tr>
          {:else}
            <tr><td colspan="4" class="none">{t("暂无访客登录")}</td></tr>
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
  .web-access { display: flex; flex-direction: column; gap: 14px; }
  .status-row { display: flex; align-items: center; gap: 8px; font-size: .92em; }
  .grow { flex: 1; min-width: 0; }
  .dot { width: 10px; height: 10px; border-radius: 50%; display: inline-block; }
  .dot.on { background: #34c77b; box-shadow: 0 0 6px #34c77b; }
  .dot.off { background: var(--text-faint); }
  .sep { color: var(--text-faint); }
  .sec h3 { font-size: .9em; color: var(--text-dim); margin-bottom: 8px; }
  .row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-bottom: 6px; }
  .check { display: inline-flex; align-items: center; gap: 6px; font-size: .88em; color: var(--text-dim); }
  .lbl { font-size: .86em; color: var(--text-dim); }
  .lbl.w { width: 5.5em; flex: none; }
  .req { color: #e5686f; margin-left: 2px; }
  code { font-family: var(--mono); font-size: .86em; user-select: text; word-break: break-all; }
  .note { font-size: .82em; color: var(--text-faint); line-height: 1.5; margin: 0; }
  .pend { font-size: .82em; color: var(--text-faint); }
  .dim { color: var(--text-faint); font-size: .86em; }
  .mono { font-family: var(--mono); font-size: .92em; }
  .sec-head-row { display: flex; align-items: center; justify-content: space-between; margin-bottom: 4px; }
  .sec-head-row h3 { margin-bottom: 0; }
  .port-input { width: 90px; padding: 4px 8px; font-size: .86em; }
  .code-input { width: 200px; }
  .tbl-wrap { overflow: auto; max-height: 240px; }
  .tbl { width: 100%; border-collapse: collapse; font-size: .86em; }
  .tbl th, .tbl td { text-align: left; padding: 6px 8px; border-bottom: 1px solid var(--border-soft); }
  .tbl thead th { color: var(--text-dim); white-space: nowrap; position: sticky; top: 0; background: var(--bg-panel); }
  .none { color: var(--text-faint); text-align: center; padding: 14px; }
  .logs { max-height: 220px; overflow: auto; background: var(--code-bg); border-radius: 6px; padding: 8px; font-family: var(--mono); font-size: .8em; user-select: text; }
  .banner { border-radius: 8px; padding: 8px 10px; font-size: .86em; }
  .banner.warn { border: 1px solid rgba(240, 170, 40, .4); background: rgba(240, 170, 40, .08); color: #b57e17; }
  .banner.err { border: 1px solid rgba(229, 104, 111, .4); background: rgba(229, 104, 111, .08); color: #d5494f; word-break: break-word; }
  .switch { position: relative; display: inline-block; width: 38px; height: 22px; flex: none; }
  .switch input { opacity: 0; width: 0; height: 0; }
  .slider { position: absolute; inset: 0; cursor: pointer; border-radius: 22px; background: var(--border); transition: background .2s; }
  .slider::before { content: ""; position: absolute; width: 16px; height: 16px; left: 3px; top: 3px; border-radius: 50%; background: #fff; transition: transform .2s; }
  .switch input:checked + .slider { background: #34c77b; }
  .switch input:checked + .slider::before { transform: translateX(16px); }
  .switch input:disabled + .slider { opacity: .5; cursor: default; }
</style>
