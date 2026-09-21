<script lang="ts">
  import { onMount } from "svelte";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import { t } from "../i18n";
  import { builtinSkins, listSkins, skinOpacity, setSkinOpacity, type SkinManifest } from "../skins.svelte";

  let custom = $state<SkinManifest[]>([]);
  let lastSkin = $state("steins-gate");
  let busy = $state(false);
  // Serialize writes so quick toggles cannot persist a stale selection.
  let saving: Promise<unknown> = Promise.resolve();
  async function choose(id: string) {
    app.skin = id;
    if (id) lastSkin = id;
    saving = saving.catch(() => {}).then(async () => {
      await api.settingsSet("ui.skin", id);
      if (id) await api.settingsSet("ui.skin.last", id);
    }).catch((error) => toast("error", String(error)));
    await saving;
  }
  async function refresh() { custom = await listSkins(); }
  onMount(() => {
    void Promise.all([refresh(), api.settingsGet("ui.skin.last")]).then(([, last]) => {
      if (app.skin) lastSkin = app.skin;
      else if (last && (builtinSkins.some((s) => s.id === last) || custom.some((s) => s.id === last))) lastSkin = last;
    }).catch((error) => toast("error", String(error)));
  });
  async function importSkin() {
    const path = await import("../dialog.svelte").then((m) => m.promptDialog({ title: t("导入皮肤包"), label: t("皮肤 zip 路径"), initial: "" }));
    if (!path?.trim()) return;
    busy = true;
    try {
      const manifest = await api.skinImport(path.trim());
      await refresh();
      await choose(String(manifest.id));
      toast("ok", t("皮肤已导入并启用"));
    } catch (error) { toast("error", String(error)); }
    finally { busy = false; }
  }
</script>

<section class="skin-settings">
  <h3>{t("皮肤插件")}</h3>
  <label class="skin-toggle"><input type="checkbox" checked={!!app.skin} onchange={(e) => choose(e.currentTarget.checked ? lastSkin : "")} />{t("启用皮肤")}</label>
  <p class="skin-note">{t("皮肤提供独立配色；关闭后恢复上方基础主题。人物悬浮于左下角顶层，不遮挡操作。")}</p>
  <div class="skin-cards">
    {#each builtinSkins as skin (skin.id)}
      <button class="skin-card" class:on={app.skin === skin.id} aria-pressed={app.skin === skin.id} onclick={() => choose(skin.id)}>
        <img src={skin.preview} alt="" loading="lazy" />
        <strong>{t(skin.name)}</strong><span>{t(skin.description)}</span>
      </button>
    {/each}
    {#each custom as skin (skin.id)}
      <button class="skin-card custom" class:on={app.skin === skin.id} aria-pressed={app.skin === skin.id} onclick={() => choose(skin.id)}>
        <span class="skin-swatch" aria-hidden="true"></span><strong>{skin.name}</strong><span>{t("自定义皮肤")}</span>
      </button>
    {/each}
  </div>
  <div class="skin-actions">
    <button class="btn sm" disabled={busy} onclick={importSkin}>{busy ? t("正在导入…") : t("导入皮肤包（zip）")}</button>
    <a href="https://github.com/nongwoluanlai/shidrive/blob/main/docs/skin-guide.md" target="_blank" rel="noopener noreferrer">{t("皮肤开发指南")}</a>
  </div>
  {#if app.skin}
    <label class="skin-opacity-row">
      <span>{t("皮肤不透明度（背景随此变清晰/模糊，人物不受影响）")}</span>
      <input type="range" min="30" max="100" value={Math.round(skinOpacity.value * 100)} oninput={(e) => setSkinOpacity(Number((e.currentTarget as HTMLInputElement).value) / 100)} />
      <b>{Math.round(skinOpacity.value * 100)}%</b>
    </label>
  {/if}
</section>

<style>
  h3 { font-size: .9em; margin-bottom: 8px; color: var(--text-dim); }
  .skin-toggle { display: flex; align-items: center; gap: 8px; font-size: .92em; }
  input { accent-color: var(--accent); }
  .skin-note { font-size: .84em; color: var(--text-dim); margin: 7px 0 12px; }
  .skin-cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); gap: 10px; }
  .skin-card { display: flex; flex-direction: column; text-align: left; gap: 4px; padding: 8px; border: 2px solid var(--border); border-radius: var(--radius); background: var(--bg-elev); min-width: 0; }
  .skin-card img, .skin-swatch { width: 100%; height: 78px; object-fit: cover; border-radius: 4px; margin-bottom: 3px; }
  .skin-swatch { background: linear-gradient(135deg, var(--bg-panel), var(--accent-soft, var(--bg-elev2))); }
  .skin-card strong { font-size: .92em; color: var(--text); overflow-wrap: anywhere; }
  .skin-card span:not(.skin-swatch) { font-size: .78em; color: var(--text-dim); }
  .skin-card.on { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .skin-card:hover { border-color: var(--text-dim); }
  .skin-card:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  .skin-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; margin-top: 12px; }
  .skin-opacity-row { display: flex; align-items: center; gap: 8px; font-size: .86em; color: var(--text-dim); margin-top: 12px; }
  .skin-opacity-row input { flex: 1; max-width: 220px; accent-color: var(--accent); }
  .skin-opacity-row b { min-width: 38px; text-align: right; color: var(--text); }
  .skin-actions a { color: var(--accent); font-size: .85em; }
</style>
