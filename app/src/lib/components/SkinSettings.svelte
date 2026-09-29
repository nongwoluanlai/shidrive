<script lang="ts">
  import { onMount } from "svelte";
  import { app, toast } from "../state.svelte";
  import { api } from "../ipc";
  import { t } from "../i18n";
  import { confirmDialog } from "../dialog.svelte";
  import { builtinSkins, listSkins, skinOpacity, setSkinOpacity, type SkinManifest } from "../skins.svelte";
  import { skinMotion, setSkinMotion, MOTION_LEVELS, type MotionLevel } from "../skin-fx/motion.svelte";
  const motionLabel: Record<MotionLevel, string> = { full: "完整", lite: "轻量", off: "关闭" };

  const MAX_PACKAGE = 32 * 1024 * 1024; // 与 skins.rs 的 MAX_TOTAL 一致

  let custom = $state<SkinManifest[]>([]);
  let lastSkin = $state("steins-gate");
  let busy = $state(false);
  /** 正在往窗口里拖 zip（Tauri 拖放事件或 HTML5 dragover） */
  let dragging = $state(false);
  let fileInput = $state<HTMLInputElement | null>(null);
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
  async function refresh() { custom = await listSkins(); void loadPreviews(); }

  // 自定义皮肤预览：背景图（无背景则立绘）经 blob URL 呈现，列表变化时释放旧 URL
  let previews = $state<Record<string, string>>({});
  // refresh() 会被连续调用（挂载、每导入一个包、删除后），loadPreviews 又不被 await：
  // 只有最新一次调用的结果可以落地，否则较慢的旧调用会用旧列表覆盖新预览；
  // 组件卸载后才返回的结果也要立即释放，避免 blob 泄漏。
  let previewGen = 0;
  let unmounted = false;
  const IMAGE_MIME: Record<string, string> = { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", webp: "image/webp", gif: "image/gif" };
  async function loadPreviews() {
    const gen = ++previewGen;
    const next: Record<string, string> = {};
    await Promise.all(custom.map(async (s) => {
      const file = s.background || s.character || "";
      if (!file) return;
      try {
        const bytes = await api.skinAssetRead(s.dir, file);
        const type = IMAGE_MIME[file.split(".").pop()?.toLowerCase() ?? ""] ?? "application/octet-stream";
        next[s.id] = URL.createObjectURL(new Blob([bytes], { type }));
      } catch { /* 预览缺失不阻塞 */ }
    }));
    if (gen !== previewGen || unmounted) {
      for (const url of Object.values(next)) URL.revokeObjectURL(url);
      return;
    }
    for (const url of Object.values(previews)) URL.revokeObjectURL(url);
    previews = next;
  }

  onMount(() => {
    void Promise.all([refresh(), api.settingsGet("ui.skin.last")]).then(([, last]) => {
      if (app.skin) lastSkin = app.skin;
      else if (last && (builtinSkins.some((s) => s.id === last) || custom.some((s) => s.id === last))) lastSkin = last;
    }).catch((error) => toast("error", String(error)));

    // Tauri 默认接管了窗口的文件拖放（dragDropEnabled）：文件落到 WebView 上时 DOM 收不到 HTML5
    // drop 事件，只会收到 tauri://drag-* 事件并带上真实路径——这里在皮肤设置打开期间监听它们，
    // 落下的 .zip 直接走按路径导入。HTML5 的 dragover/drop 作为浏览器/关闭 dragDrop 时的后备。
    let unlisten: (() => void) | null = null;
    let disposed = false;
    void import("@tauri-apps/api/webview")
      .then(({ getCurrentWebview }) => getCurrentWebview().onDragDropEvent((event) => {
        const payload = event.payload;
        if (payload.type === "enter") dragging = payload.paths.some(isZipPath);
        else if (payload.type === "leave") dragging = false;
        else if (payload.type === "drop") {
          dragging = false;
          const zips = payload.paths.filter(isZipPath);
          if (zips.length) void importPaths(zips);
          else if (payload.paths.length) toast("warn", t("仅支持 .zip 皮肤包"));
        }
      }))
      .then((fn) => { if (disposed) fn(); else unlisten = fn; })
      .catch(() => { /* 非 Tauri 环境（如浏览器预览）没有拖放事件 */ });
    return () => {
      disposed = true;
      unmounted = true;
      unlisten?.();
      for (const url of Object.values(previews)) URL.revokeObjectURL(url);
    };
  });

  const isZipPath = (p: string) => /\.zip$/i.test(p);
  const isZipFile = (f: File) => isZipPath(f.name) || /zip/i.test(f.type);

  async function finishImport(manifest: Record<string, unknown>) {
    await refresh();
    await choose(String(manifest.id));
  }
  /** 拖放（Tauri 事件给的是路径） */
  async function importPaths(paths: string[]) {
    if (busy) return;
    busy = true;
    let imported = 0;
    try {
      for (const path of paths) {
        try {
          await finishImport(await api.skinImport(path));
          imported++;
        } catch (error) { toast("error", `${path.split(/[\\/]/).pop()}: ${String(error)}`); }
      }
      if (imported) toast("ok", imported === 1 ? t("皮肤已导入并启用") : t("已导入 {count} 个皮肤", { count: imported }));
    } finally { busy = false; }
  }
  /** 文件选择器 / HTML5 拖放（只有字节没有路径）：整包作为二进制请求体发给后端 */
  async function importFiles(files: FileList | File[]) {
    if (busy) return;
    const list = Array.from(files);
    if (!list.some(isZipFile)) { toast("warn", t("仅支持 .zip 皮肤包")); return; }
    busy = true;
    let imported = 0;
    try {
      for (const file of list.filter(isZipFile)) {
        if (file.size > MAX_PACKAGE) { toast("error", `${file.name}: ${t("皮肤包超过 32 MiB")}`); continue; }
        try {
          const bytes = new Uint8Array(await file.arrayBuffer());
          await finishImport(await api.skinImportBytes(bytes));
          imported++;
        } catch (error) { toast("error", `${file.name}: ${String(error)}`); }
      }
      if (imported) toast("ok", imported === 1 ? t("皮肤已导入并启用") : t("已导入 {count} 个皮肤", { count: imported }));
    } finally { busy = false; }
  }
  function onFilePicked(e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    if (input.files?.length) void importFiles(input.files);
    input.value = ""; // 同一个文件可再次选择
  }
  function onDragOver(e: DragEvent) {
    if (!e.dataTransfer?.types.includes("Files")) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    dragging = true;
  }
  function onDrop(e: DragEvent) {
    e.preventDefault();
    dragging = false;
    if (e.dataTransfer?.files.length) void importFiles(e.dataTransfer.files);
  }

  async function removeSkin(skin: SkinManifest) {
    const ok = await confirmDialog({
      title: t("删除皮肤"),
      message: t("删除皮肤「{name}」？其文件将从本机移除，重新导入即可恢复。", { name: skin.name }),
      danger: true,
      confirmText: t("删除"),
    });
    if (!ok) return;
    try {
      await api.skinDelete(skin.id);
      if (app.skin === skin.id) await choose("");
      if (lastSkin === skin.id) lastSkin = builtinSkins[0].id;
      await refresh();
      toast("ok", t("皮肤已删除"));
    } catch (error) { toast("error", String(error)); }
  }
  function cardKey(e: KeyboardEvent, id: string) {
    // 事件会从卡片内的 ✕ 按钮冒泡上来：只处理卡片本身，否则回车/空格删不掉皮肤反而会选中它
    if (e.target !== e.currentTarget) return;
    if (e.key === "Enter" || e.key === " ") { e.preventDefault(); void choose(id); }
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
      <!-- 自定义皮肤卡片带删除按钮，button 不能嵌套 button，所以卡片本身用 role=button -->
      <div class="skin-card custom" class:on={app.skin === skin.id} role="button" tabindex="0" aria-pressed={app.skin === skin.id} onclick={() => choose(skin.id)} onkeydown={(e) => cardKey(e, skin.id)}>
        {#if previews[skin.id]}
          <img class={!skin.background ? "pf-char" : ""} src={previews[skin.id]} alt="" loading="lazy" />
        {:else}
          <span class="skin-swatch" aria-hidden="true"></span>
        {/if}
        <strong>{skin.name}</strong><span>{t("自定义皮肤")}</span>
        <button class="skin-del" title={t("删除皮肤")} aria-label={t("删除皮肤")} onclick={(e) => { e.stopPropagation(); void removeSkin(skin); }}>✕</button>
      </div>
    {/each}
  </div>
  <div class="skin-drop" class:active={dragging} class:busy role="region" aria-label={t("导入皮肤包（zip）")} ondragover={onDragOver} ondragenter={onDragOver} ondragleave={() => (dragging = false)} ondrop={onDrop}>
    <span>{dragging ? t("松开即可导入皮肤包") : busy ? t("正在导入…") : t("把皮肤 zip 拖到这里，或")}</span>
    {#if !dragging}
      <button class="btn sm" disabled={busy} onclick={() => fileInput?.click()}>{t("选择 zip 文件")}</button>
    {/if}
    <input bind:this={fileInput} type="file" accept=".zip,application/zip,application/x-zip-compressed" multiple hidden onchange={onFilePicked} />
  </div>
  <div class="skin-actions">
    <button class="linklike" onclick={() => void api.fsOpenDefault("https://github.com/nongwoluanlai/shidrive/blob/main/docs/skin-guide.md").catch((e) => toast("error", String(e)))}>{t("皮肤开发指南")}</button>
    <button class="linklike" onclick={() => void api.fsOpenDefault("https://shidrive.nwll.top/skinstore").catch((e) => toast("error", String(e)))}>{t("皮肤商店 ↗")}</button>
  </div>
  {#if app.skin}
    <label class="skin-opacity-row">
      <span>{t("皮肤不透明度（背景随此变清晰/模糊，人物不受影响）")}</span>
      <input type="range" min="30" max="100" value={Math.round(skinOpacity.value * 100)} oninput={(e) => setSkinOpacity(Number((e.currentTarget as HTMLInputElement).value) / 100)} />
      <b>{Math.round(skinOpacity.value * 100)}%</b>
    </label>
    <div class="skin-opacity-row" role="radiogroup" aria-label={t("皮肤动效")}>
      <span>{t("皮肤动效（动态看板与全局特效；系统“减少动态效果”时自动降为轻量）")}</span>
      {#each MOTION_LEVELS as lv}
        <button class="skin-motion-btn" class:on={skinMotion.level === lv} role="radio" aria-checked={skinMotion.level === lv} onclick={() => setSkinMotion(lv)}>{t(motionLabel[lv])}</button>
      {/each}
    </div>
  {/if}
</section>

<style>
  h3 { font-size: .9em; margin-bottom: 8px; color: var(--text-dim); }
  .skin-toggle { display: flex; align-items: center; gap: 8px; font-size: .92em; }
  input { accent-color: var(--accent); }
  .skin-note { font-size: .84em; color: var(--text-dim); margin: 7px 0 12px; }
  .skin-cards { display: grid; grid-template-columns: repeat(auto-fit, minmax(170px, 1fr)); gap: 10px; }
  .skin-card { display: flex; flex-direction: column; text-align: left; gap: 4px; padding: 8px; border: 2px solid var(--border); border-radius: var(--radius); background: var(--bg-elev); min-width: 0; }
  .skin-card img, .skin-swatch { width: 100%; height: 78px; border-radius: 4px; margin-bottom: 3px; }
  .skin-card img { object-fit: cover; }
  .skin-card img.pf-char { object-fit: contain; background: var(--bg-panel); }
  .skin-swatch { background: linear-gradient(135deg, var(--bg-panel), var(--accent-soft, var(--bg-elev2))); }
  .skin-card strong { font-size: .92em; color: var(--text); overflow-wrap: anywhere; }
  .skin-card span:not(.skin-swatch) { font-size: .78em; color: var(--text-dim); }
  .skin-card.on { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .skin-card:hover { border-color: var(--text-dim); }
  .skin-card:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
  .skin-card.custom { position: relative; cursor: pointer; }
  .skin-del { position: absolute; top: 6px; right: 6px; width: 24px; height: 24px; border-radius: 50%; border: 1px solid var(--border); background: color-mix(in srgb, var(--bg-panel) 85%, transparent); color: var(--text-dim); font-size: .8em; line-height: 1; cursor: pointer; opacity: 0; transition: opacity .12s; }
  .skin-card.custom:hover .skin-del, .skin-card.custom:focus-within .skin-del { opacity: 1; }
  .skin-del:hover { color: var(--danger); border-color: var(--danger); }
  .skin-drop { display: flex; flex-wrap: wrap; align-items: center; justify-content: center; gap: 10px; margin-top: 12px; padding: 14px; border: 1.5px dashed var(--border); border-radius: var(--radius); color: var(--text-dim); font-size: .88em; background: color-mix(in srgb, var(--bg-elev) 60%, transparent); transition: border-color .15s, background .15s; }
  .skin-drop.active { border-color: var(--accent); border-style: solid; background: var(--accent-soft, var(--bg-elev2)); color: var(--text); }
  .skin-drop.busy { opacity: .7; }
  .skin-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 12px; margin-top: 10px; }
  .skin-opacity-row { display: flex; align-items: center; gap: 8px; font-size: .86em; color: var(--text-dim); margin-top: 12px; }
  .skin-opacity-row input { flex: 1; max-width: 220px; accent-color: var(--accent); }
  .skin-motion-btn { padding: 3px 10px; border-radius: 6px; border: 1px solid var(--border); background: transparent; color: var(--text-dim); cursor: pointer; }
  .skin-motion-btn.on { border-color: var(--accent); color: var(--text); background: color-mix(in srgb, var(--accent) 16%, transparent); }
  .skin-opacity-row b { min-width: 38px; text-align: right; color: var(--text); }
  .skin-actions .linklike { color: var(--accent); font-size: .85em; background: none; border: none; padding: 0; cursor: pointer; }
  .skin-actions .linklike:hover { text-decoration: underline; }
</style>
