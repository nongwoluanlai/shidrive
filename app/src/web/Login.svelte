<script lang="ts">
  import { api } from "./api";
  import { ui, loadAll, errText } from "./store.svelte";

  let code = $state("");
  let busy = $state(false);
  let error = $state("");
  let show = $state(false);

  async function submit(e: Event) {
    e.preventDefault();
    if (!code.trim() || busy) return;
    busy = true;
    error = "";
    try {
      await api.login(code.trim());
      code = "";
      ui.authed = true;
      await loadAll();
    } catch (err) {
      error = errText(err);
      ui.authed = false;
    } finally {
      busy = false;
    }
  }
</script>

<div class="login">
  <form class="panel" onsubmit={submit}>
    <div class="logo">驾</div>
    <h1>使驾 ShiDrive</h1>
    <p class="sub">网站接入 · 请输入访客码</p>
    <div class="field">
      <input
        class="input code"
        type={show ? "text" : "password"}
        autocomplete="current-password"
        autocapitalize="characters"
        spellcheck="false"
        placeholder="访客码"
        bind:value={code}
        disabled={busy}
      />
      <button type="button" class="eye" onclick={() => (show = !show)} aria-label="显示/隐藏">{show ? "隐藏" : "显示"}</button>
    </div>
    {#if error}<div class="err">{error}</div>{/if}
    <button class="btn primary go" type="submit" disabled={busy || !code.trim()}>
      {#if busy}<span class="spin"></span>{/if}进入
    </button>
    <p class="note">访客码在电脑端「设置 → 网站接入」中查看。登录状态保存 7 天，电脑端可随时一键注销。</p>
  </form>
</div>

<style>
  .login {
    height: 100%; display: grid; place-items: center; padding: 20px;
    background: radial-gradient(1200px 600px at 10% -10%, var(--accent-soft), transparent 60%), var(--bg);
  }
  .panel {
    width: min(380px, 100%); background: var(--bg-panel); border: 1px solid var(--border-soft); border-radius: 18px;
    padding: 32px 26px 24px; box-shadow: var(--shadow); display: flex; flex-direction: column; align-items: stretch; gap: 12px; text-align: center;
  }
  .logo {
    width: 56px; height: 56px; border-radius: 16px; margin: 0 auto 4px; display: grid; place-items: center;
    font-size: 26px; font-weight: 700; color: #fff; background: linear-gradient(135deg, var(--accent), #8a6dff);
  }
  h1 { font-size: 22px; }
  .sub { color: var(--text-dim); margin-top: -6px; }
  .field { position: relative; margin-top: 8px; }
  .code { height: 48px; font-size: 18px; letter-spacing: .18em; text-align: center; padding-right: 56px; padding-left: 56px; }
  .eye { position: absolute; right: 8px; top: 50%; transform: translateY(-50%); font-size: 12.5px; color: var(--text-faint); padding: 6px; }
  .go { height: 46px; font-size: 16px; }
  .err { color: var(--danger); font-size: 14px; }
  .note { font-size: 12.5px; color: var(--text-faint); line-height: 1.6; }
</style>
