<script lang="ts">
  import { app } from "../state.svelte";
  import { dynamicSkin } from "../skins.svelte";
  import { effectiveMotion, skinMotion } from "../skin-fx/motion.svelte";
  import { onSkinEvent } from "../skin-fx/bus";
  import { SceneRenderer } from "../skin-fx/scene";

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
</script>

{#if app.skin}
  <div class="skin-character-stage" aria-hidden="true">
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
