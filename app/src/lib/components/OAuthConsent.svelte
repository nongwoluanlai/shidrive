<script lang="ts">
  // 全局常驻：不依赖「设置 → 外部编程接入」是否打开；错过事件也能从后端补回。
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import { t } from "../i18n";
  import type { OAuthPending } from "../types";

  interface OpenGrant {
    id: string;
    project_name: string;
    project_root: string;
    context_name: string;
    context_enabled: boolean;
    fs_write: boolean;
    exec_allowed: boolean;
    revoked_at: string | null;
    paused_at: string | null;
  }

  let openGrants = $state<OpenGrant[]>([]);
  let busyTxn = $state<string | null>(null);
  let version = 0;
  let requestSeq = 0;
  const handled = new Set<string>();
  let mounted = false;
  let firstLoad = true;

  async function refreshPending() {
    const before = version;
    const seq = ++requestSeq;
    try {
      const rows = (await api.remoteOauthPendingList()).filter((row) => !handled.has(row.txnId));
      if (!mounted || seq !== requestSeq) return;
      // 后端快照与新事件/本地裁决并发时，不要用较旧的快照覆盖较新的状态。
      app.oauthPending = before === version
        ? rows
        : [...rows, ...app.oauthPending.filter((row) => !handled.has(row.txnId) && !rows.some((r) => r.txnId === row.txnId))];
      if (firstLoad && app.oauthPending.length) app.oauthConsentOpen = true;
      if (!app.oauthPending.length) app.oauthConsentOpen = false;
      firstLoad = false;
      if (app.oauthPending.length) void refreshGrants();
    } catch {
      // 后端不可用时保留已收到的事件，下一次轮询/窗口恢复再重试。
    }
  }

  async function refreshGrants() {
    try {
      const rows = await api.remoteGrantsList() as OpenGrant[];
      if (mounted) openGrants = rows.filter((g) => !g.revoked_at && !g.paused_at);
    } catch { /* 使用最后一次成功的授权列表；批准时后端仍会重新校验。 */ }
  }

  onMount(() => {
    mounted = true;
    const subscribed = listen<OAuthPending>("remote://oauth-consent", (e) => {
      const req = e.payload;
      if (!req?.txnId || handled.has(req.txnId)) return;
      if (!app.oauthPending.some((row) => row.txnId === req.txnId)) {
        app.oauthPending = [...app.oauthPending, req];
      }
      version++;
      firstLoad = false;
      app.oauthConsentOpen = true;
      toast("info", t("收到 OAuth 授权请求"));
      void refreshGrants();
    });
    // 先建立监听，再查询快照：设置未打开 / 刚启动时的请求也能看到。
    void subscribed.then(() => refreshPending()).catch(() => {});
    const timer = setInterval(() => void refreshPending(), 4000);
    return () => {
      mounted = false;
      clearInterval(timer);
      void subscribed.then((unlisten) => unlisten()).catch(() => {});
    };
  });

  async function decide(req: OAuthPending, approve: boolean) {
    if (busyTxn) return;
    busyTxn = req.txnId;
    try {
      await api.remoteOauthDecide(req.txnId, approve);
      handled.add(req.txnId);
      version++;
      app.oauthPending = app.oauthPending.filter((row) => row.txnId !== req.txnId);
      if (!app.oauthPending.length) app.oauthConsentOpen = false;
      toast("ok", approve ? t("已批准，客户端将自动继续授权") : t("已拒绝该授权请求"));
      void refreshPending();
    } catch (e) {
      toast("error", String(e));
      void refreshPending();
    } finally {
      busyTxn = null;
    }
  }
</script>

{#if app.oauthConsentOpen && app.oauthPending.length}
  <div class="modal-backdrop consent-backdrop">
    <div class="modal consent" role="dialog" aria-modal="true" aria-label={t("OAuth 待授权请求")}>
      <header>
        <span>🔐 {t("OAuth 待授权请求")}（{app.oauthPending.length}）</span>
        <button class="btn ghost sm" title={t("稍后处理（标题栏可重新打开）")} onclick={() => (app.oauthConsentOpen = false)}>✕</button>
      </header>
      <div class="body">
        <p class="note">{t("批准后客户端可使用当前及后续开放的目录与上下文；实际能力为请求 scope 与各授权权限的交集。暂停或删除授权会立即收回该范围。")}</p>
        {#each app.oauthPending as req (req.txnId)}
          <section class="request">
            <div class="client"><b>{req.clientName}</b> <span class="mono dim">{req.clientId}</span></div>
            <div class="meta">{t("回调地址")}：<span class="mono">{req.redirectUri}</span></div>
            <div class="meta">{t("请求权限")}：<span class="mono">{req.scopes.join(" · ")}</span></div>
            <div class="grant-head">
              <b>{t("当前开放目录与上下文")}（{openGrants.length}）</b>
              <button class="btn ghost sm" onclick={refreshGrants}>{t("刷新")}</button>
            </div>
            {#if openGrants.length}
              <ul class="grants">
                {#each openGrants as g (g.id)}
                  <li>
                    <b>{g.project_name}</b> <span class="mono dim">{g.project_root}</span><br />
                    <span class="dim">{g.fs_write ? t("读写") : t("只读")}{g.exec_allowed ? ` · ${t("可执行（需全局开关）")}` : ""}{g.context_enabled ? ` · ${t("共享上下文")}：${g.context_name}` : ""}</span>
                  </li>
                {/each}
              </ul>
            {:else}
              <p class="empty-grants">{t("尚无开放目录。请先到「设置 → 外部编程接入 → 开放目录与上下文」添加授权。")}</p>
            {/if}
            <div class="actions">
              <button class="btn sm" disabled={!!busyTxn} onclick={() => void decide(req, false)}>{t("拒绝")}</button>
              <button class="btn sm primary" disabled={!!busyTxn || !openGrants.length} onclick={() => void decide(req, true)}>{t("批准使用开放权限")}</button>
            </div>
          </section>
        {/each}
      </div>
    </div>
  </div>
{/if}

<style>
  .consent-backdrop { z-index: 120; }
  .modal.consent { width: min(690px, 94vw); min-width: 0; }
  .note { color: var(--text-dim); font-size: .88em; line-height: 1.55; margin-bottom: 12px; }
  .request { border: 1px solid var(--border); border-radius: 9px; padding: 12px; margin-bottom: 10px; }
  .request:last-child { margin-bottom: 0; }
  .client { margin-bottom: 4px; overflow-wrap: anywhere; }
  .meta { color: var(--text-dim); font-size: .85em; overflow-wrap: anywhere; }
  .mono { font-family: var(--mono); font-size: .9em; user-select: text; }
  .dim { color: var(--text-faint); }
  .grant-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; margin: 12px 0 5px; font-size: .86em; }
  .grants { list-style: none; max-height: 180px; overflow: auto; border: 1px solid var(--border-soft); border-radius: 6px; }
  .grants li { padding: 5px 8px; border-bottom: 1px solid var(--border-soft); font-size: .85em; overflow-wrap: anywhere; }
  .grants li:last-child { border-bottom: 0; }
  .empty-grants { color: var(--warn); font-size: .86em; }
  .actions { display: flex; justify-content: flex-end; gap: 8px; margin-top: 12px; }
</style>
