<script lang="ts">
  // 设置 →「帮助与关于」：使用教程、联系方式（公众号可复制名字 / 扫码）、工作室展示与版本信息。
  // 图片随应用打包在 public/about/，离线可用，打开设置不产生网络请求。
  import { onMount } from "svelte";
  import { api } from "../ipc";
  import { toast } from "../state.svelte";
  import { t } from "../i18n";

  const TUTORIAL_URL = "https://shidrive.nwll.top/tutorials/start";
  const REPO_URL = "https://github.com/nongwoluanlai/shidrive";
  const WECHAT_NAME = "弄倭乱来";

  let version = $state("");
  onMount(() => {
    import("@tauri-apps/api/app")
      .then((m) => m.getVersion())
      .then((v) => { if (typeof v === "string") version = v; })
      .catch(() => { /* 非 Tauri 环境没有版本号，不显示即可 */ });
  });

  function openUrl(url: string) {
    void api.fsOpenDefault(url).catch((e) => toast("error", String(e)));
  }

  async function copyWechatName() {
    try {
      await api.clipboardWriteText(WECHAT_NAME);
    } catch {
      try { await navigator.clipboard.writeText(WECHAT_NAME); }
      catch (e) { toast("error", String(e)); return; }
    }
    toast("ok", t("已复制公众号名称「{name}」", { name: WECHAT_NAME }));
  }
</script>

<div class="about">
  <section class="sec">
    <h3>{t("使用教程")}</h3>
    <div class="guide">
      <div class="guide-text">
        <strong>{t("使驾 ShiDrive 使用教程")}</strong>
        <span class="url">{TUTORIAL_URL}</span>
      </div>
      <button class="btn sm primary" onclick={() => openUrl(TUTORIAL_URL)}>{t("打开使用教程 ↗")}</button>
    </div>
  </section>

  <section class="sec">
    <h3>{t("联系方式")}</h3>
    <p class="note">{t("产品动态、使用技巧与交流，都在公众号「弄倭乱来」。")}</p>
    <div class="cards">
      <div class="card">
        <div class="qr"><img src="/about/wechat-qr.png" alt={t("公众号二维码：弄倭乱来")} draggable="false" /></div>
        <span class="label">{t("微信公众号")}</span>
        <div class="name-row">
          <span class="name" title={WECHAT_NAME}>{WECHAT_NAME}</span>
          <button class="btn sm" onclick={copyWechatName}>{t("复制名字")}</button>
        </div>
        <span class="hint">{t("微信扫码关注，或复制名字后在微信中搜索")}</span>
      </div>
      <div class="card">
        <div class="studio"><img src="/about/studio.jpg" alt={t("不乱来工作室")} draggable="false" /></div>
        <span class="label">{t("工作室")}</span>
        <span class="name">{t("不乱来工作室")}</span>
        <span class="hint">{t("做点小工具，让重复劳动少一点。")}</span>
      </div>
    </div>
  </section>

  <section class="sec">
    <h3>{t("关于")}</h3>
    <div class="meta">
      <span>使驾 ShiDrive{version ? ` v${version}` : ""}</span>
      <button class="linklike" onclick={() => openUrl(REPO_URL)}>GitHub ↗</button>
    </div>
  </section>
</div>

<style>
  /* 与 SettingsModal 其它栏目一致（其 .sec 样式是组件作用域，这里对齐一份） */
  .about { display: flex; flex-direction: column; gap: 18px; }
  .sec h3 { font-size: 0.9em; color: var(--text-dim); margin: 0 0 8px; }
  .guide {
    display: flex; align-items: center; justify-content: space-between; gap: 12px;
    padding: 12px 14px; border: 1px solid var(--border-soft); border-radius: var(--radius); background: var(--bg-elev);
  }
  .guide-text { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .guide-text strong { font-size: .95em; color: var(--text); }
  .url { font-size: .8em; color: var(--text-faint); font-family: var(--mono); overflow-wrap: anywhere; user-select: text; }
  .note { font-size: .86em; color: var(--text-dim); margin: 0 0 10px; }
  .cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); gap: 12px; }
  .card {
    display: flex; flex-direction: column; align-items: center; text-align: center; gap: 6px;
    padding: 16px 14px 14px; border: 1px solid var(--border-soft); border-radius: var(--radius); background: var(--bg-elev);
  }
  .qr, .studio { width: 150px; height: 150px; border-radius: 10px; overflow: hidden; margin-bottom: 4px; }
  /* 二维码始终垫白底 + 留白，深色主题 / 皮肤下也能扫 */
  .qr { background: #fff; padding: 8px; box-shadow: 0 0 0 1px var(--border-soft); }
  .qr img, .studio img { width: 100%; height: 100%; object-fit: contain; display: block; user-select: none; pointer-events: none; }
  .studio img { object-fit: cover; }
  .label { font-size: .78em; color: var(--text-faint); letter-spacing: .04em; }
  .name { font-size: 1.05em; font-weight: 600; color: var(--text); user-select: text; }
  .name-row { display: flex; align-items: center; gap: 8px; }
  .hint { font-size: .8em; color: var(--text-dim); }
  .meta { display: flex; align-items: center; gap: 14px; font-size: .9em; color: var(--text-dim); }
  .linklike { color: var(--accent); background: none; border: none; padding: 0; cursor: pointer; font-size: inherit; }
  .linklike:hover { text-decoration: underline; }
</style>
