<script lang="ts">
  import { app } from "../state.svelte";
  import { dynamicSkin } from "../skins.svelte";
  import { effectiveMotion, skinMotion } from "../skin-fx/motion.svelte";
  import { emitSkinEvent, onSkinEvent } from "../skin-fx/bus";
  import { INTERACTIVE, SceneRenderer } from "../skin-fx/scene";
  import { see, seeMask, inSeeThrough, watchSeeThrough } from "../skin-fx/seethrough.svelte";

  // 动态看板：皮肤声明了 scene 时用 WebGL 渲染分层人物（待机动画 + 事件表情）；
  // 动效关闭、WebGL 不可用或渲染失败时回退为静态图，界面其余部分不受影响。
  let canvas = $state<HTMLCanvasElement | null>(null);
  /** 渲染失败的皮肤 id：同一皮肤不再重试，直到切换皮肤 */
  let failedId = $state("");
  let renderer: SceneRenderer | null = null;

  const level = $derived(skinMotion.level === "full" && skinMotion.reduced ? "lite" : skinMotion.level);
  const live = $derived(!!dynamicSkin.scene && !!dynamicSkin.urls && level !== "off" && failedId !== dynamicSkin.id);

  const loadImage = (src: string) => new Promise<HTMLImageElement>((resolve, reject) => {
    const img = new Image();
    img.onload = () => resolve(img);
    img.onerror = () => reject(new Error("scene image failed to load"));
    img.src = src;
  });

  $effect(() => {
    const spec = dynamicSkin.scene, urls = dynamicSkin.urls, id = dynamicSkin.id, el = canvas;
    if (!live || !spec || !urls || !el) return;
    let cancelled = false;
    let unsubscribe = () => {};
    const fail = (reason: unknown) => { console.warn("[skin] dynamic board disabled:", reason); if (!cancelled) failedId = id; };
    Promise.all([loadImage(urls.body), loadImage(urls.mask), Promise.all(urls.layers.map(loadImage))])
      .then(([body, mask, layers]) => {
        if (cancelled) return;
        renderer = new SceneRenderer(el, spec, { body, mask, layers }, effectiveMotion, fail);
        const r = renderer;
        unsubscribe = onSkinEvent((event) => r.trigger(event));
        r.trigger("skin.enter");
      })
      .catch(fail);
    return () => {
      cancelled = true;
      unsubscribe();
      renderer?.dispose();
      renderer = null;
    };
  });

  // 动效档位变化时补画一帧（按需渲染模式下画面否则会停在旧形变上）
  $effect(() => { void level; renderer?.invalidate(); });

  // ---------- 聊天输入框聚焦时：与聊天区重叠的部分变透明 ----------
  let stageEl = $state<HTMLDivElement | null>(null);
  $effect(() => {
    if (!app.skin) return;
    return watchSeeThrough();
  });
  const maskStyle = $derived.by(() => {
    const h = see.hole, el = stageEl;
    if (!h || !el) return "";
    return `${seeMask(h, el.clientWidth, el.clientHeight)};--see:${see.active ? 0.1 : 1}`;
  });

  // ---------- 静态人物的点击判定（动态看板由 SceneRenderer 自己判定） ----------
  // 不透明像素才算「戳人物」：取一张缩小的 alpha 图，按 contain + 左下对齐换算坐标。
  let alpha: { src: string; w: number; h: number; iw: number; ih: number; a: Uint8ClampedArray } | null = null;
  let alphaLoading = "";
  function staticTarget(): { el: Element; src: string } | null {
    const img = stageEl?.querySelector(".skin-scene-static") as HTMLImageElement | null;
    if (img?.src) return { el: img, src: img.currentSrc || img.src };
    const div = stageEl?.querySelector(".skin-character");
    if (!div) return null;
    const m = /url\(["']?(.*?)["']?\)/.exec(getComputedStyle(div).backgroundImage);
    return m ? { el: div, src: m[1] } : null;
  }
  function loadAlpha(src: string) {
    if (alphaLoading === src) return;
    alphaLoading = src;
    const img = new Image();
    img.onload = () => {
      try {
        const k = Math.min(1, 256 / Math.max(img.naturalWidth, img.naturalHeight));
        const w = Math.max(1, Math.round(img.naturalWidth * k)), h = Math.max(1, Math.round(img.naturalHeight * k));
        const c = document.createElement("canvas");
        c.width = w; c.height = h;
        const ctx = c.getContext("2d", { willReadFrequently: true })!;
        ctx.drawImage(img, 0, 0, w, h);
        const d = ctx.getImageData(0, 0, w, h).data;
        const a = new Uint8ClampedArray(w * h);
        for (let i = 0; i < a.length; i++) a[i] = d[i * 4 + 3];
        alpha = { src, w, h, iw: img.naturalWidth, ih: img.naturalHeight, a };
      } catch { alpha = null; }
    };
    img.src = src;
  }
  function staticHit(x: number, y: number): boolean {
    const t = staticTarget();
    if (!t) return false;
    if (alpha?.src !== t.src) { loadAlpha(t.src); return false; }
    const r = t.el.getBoundingClientRect();
    const k = Math.min(r.width / alpha.iw, r.height / alpha.ih);
    const w = alpha.iw * k, h = alpha.ih * k;
    const px = (x - r.left) / w, py = (y - (r.bottom - h)) / h;
    if (px < 0 || py < 0 || px >= 1 || py >= 1) return false;
    return alpha.a[Math.floor(py * alpha.h) * alpha.w + Math.floor(px * alpha.w)] > 96;
  }
  $effect(() => {
    if (!app.skin || live) return;
    const t = setTimeout(() => { const st = staticTarget(); if (st) loadAlpha(st.src); }, 300);
    const onDown = (e: PointerEvent) => {
      if (e.button !== 0 || (e.target instanceof Element && e.target.closest(INTERACTIVE))) return;
      if (inSeeThrough(e.clientX, e.clientY)) return;
      if (staticHit(e.clientX, e.clientY)) emitSkinEvent("character.click");
    };
    addEventListener("pointerdown", onDown, { passive: true, capture: true });
    return () => { clearTimeout(t); removeEventListener("pointerdown", onDown, { capture: true }); };
  });
</script>

{#if app.skin}
  <div class="skin-character-stage" class:see-through={see.active && !!see.hole} aria-hidden="true" bind:this={stageEl} style={maskStyle}>
    {#if live}
      <canvas class="skin-scene" bind:this={canvas}></canvas>
    {:else if dynamicSkin.scene && dynamicSkin.urls}
      <img class="skin-scene-static" src={dynamicSkin.urls.body} alt="" />
    {:else}
      <div class="skin-character" role="presentation"></div>
    {/if}
  </div>
{/if}

<style>
  .skin-scene { position: absolute; inset: 0; width: 100%; height: 100%; display: block; }
  .skin-scene-static { position: absolute; left: 6px; bottom: 0; max-width: 95%; height: 98%; object-fit: contain; object-position: left bottom; filter: drop-shadow(3px 2px 7px #0006); }
</style>
