<script lang="ts">
  // 网页端用的少量线性图标（24 网格，stroke 绘制）
  let { name, size = 18 }: { name: string; size?: number } = $props();
  const P: Record<string, string> = {
    chat: "M4 5h16v11H8l-4 4z",
    folder: "M3 6.5A1.5 1.5 0 0 1 4.5 5H9l2 2h8.5A1.5 1.5 0 0 1 21 8.5v9a1.5 1.5 0 0 1-1.5 1.5h-15A1.5 1.5 0 0 1 3 17.5z",
    file: "M6 3h8l4 4v14H6zM14 3v4h4",
    flow: "M5 4h5v5H5zM14 15h5v5h-5zM7.5 9v3.5a2 2 0 0 0 2 2h7v.5",
    book: "M5 4h10a3 3 0 0 1 3 3v13H8a3 3 0 0 1-3-3zM5 17a3 3 0 0 1 3-3h10",
    menu: "M4 7h16M4 12h16M4 17h16",
    x: "M6 6l12 12M18 6L6 18",
    send: "M4 12l16-8-6 16-2.5-6.5z",
    stop: "M7 7h10v10H7z",
    plus: "M12 5v14M5 12h14",
    refresh: "M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7",
    back: "M15 5l-7 7 7 7",
    chevron: "M9 6l6 6-6 6",
    down: "M6 9l6 6 6-6",
    sun: "M12 7a5 5 0 1 0 0 10 5 5 0 0 0 0-10zM12 1v2M12 21v2M4.2 4.2l1.4 1.4M18.4 18.4l1.4 1.4M1 12h2M21 12h2M4.2 19.8l1.4-1.4M18.4 5.6l1.4-1.4",
    moon: "M20 14.5A8 8 0 0 1 9.5 4 8 8 0 1 0 20 14.5z",
    logout: "M15 4h4v16h-4M10 8l-4 4 4 4M6 12h10",
    play: "M8 5l11 7-11 7z",
    search: "M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4",
    copy: "M9 9h11v11H9zM5 15V4h11",
    download: "M12 4v11M7 10l5 5 5-5M5 20h14",
    plug: "M9 3v5M15 3v5M6 8h12v3a6 6 0 0 1-12 0zM12 17v4",
    monitor: "M3 4h18v12H3zM8 20h8M12 16v4",
    tool: "M14.7 6.3a4 4 0 0 0-5.4 5.4L4 17l3 3 5.3-5.3a4 4 0 0 0 5.4-5.4l-2.5 2.5-2.5-2.5z",
    shield: "M12 3l8 3v6c0 4.5-3.4 8.2-8 9-4.6-.8-8-4.5-8-9V6z",
    brain: "M9 4a3 3 0 0 0-3 3 3 3 0 0 0-2 5 3 3 0 0 0 2 5 3 3 0 0 0 6 0V4a3 3 0 0 0-3 0zM15 4a3 3 0 0 1 3 3 3 3 0 0 1 2 5 3 3 0 0 1-2 5 3 3 0 0 1-6 0",
    clock: "M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7v5l3 2",
    dot: "M12 10a2 2 0 1 0 0 4 2 2 0 0 0 0-4z",
    check: "M5 12l5 5 9-10",
    alert: "M12 3l10 18H2zM12 10v5M12 18v.5",
    eye: "M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6z",
    code: "M8 7l-5 5 5 5M16 7l5 5-5 5",
    image: "M4 5h16v14H4zM4 16l5-5 4 4 3-3 4 4M15 9h.01",
  };
</script>

<svg class="ic" width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
  <path d={P[name] ?? P.dot} />
</svg>

<style>
  .ic { flex: none; display: block; }
</style>
