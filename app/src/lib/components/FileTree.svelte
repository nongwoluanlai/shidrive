<script lang="ts">
  import { t } from "../i18n";
  import { untrack } from "svelte";
  // Right-docked file tree. Single-click selects/opens preview in the editor pane.
  import { app, currentProject, toast, openEditor, closeEditor } from "../state.svelte";
import { confirmDialog, promptDialog } from "../dialog.svelte";
  import { api } from "../ipc";
  import type { DirEntryInfo } from "../types";
  import ContextMenu from "./ContextMenu.svelte";
  import Icon from "./Icon.svelte";
  import type { MenuItem } from "./menu-item";

  const project = $derived(currentProject());
  const root = $derived(project?.root_path ?? "");

  interface Node extends DirEntryInfo {
    children?: Node[];
  }

  let tree = $state<Node[]>([]);
  let selected = $state<string | null>(null);
  let expanded = $state<Record<string, boolean>>({});
  let menu = $state<{ x: number; y: number; items: MenuItem[] } | null>(null);
  let renaming = $state<string | null>(null);
  let renameValue = $state("");

  // 根目录切换：整树复位；treeRev（回合结束等）：保留展开状态做增量同步
  let loadedRoot = "";
  $effect(() => {
    void app.treeRev;
    const r = root;
    if (!r) {
      loadedRoot = "";
      closeEditor();
      return;
    }
    // 只依赖 root / treeRev：读取 tree、expanded 的同步部分不能被这个 effect 追踪
    untrack(() => {
      if (r !== loadedRoot) {
        loadedRoot = r;
        sigs = new Map();
        void refresh();
      } else {
        void syncAll();
      }
    });
  });

  async function refresh() {
    if (!root) return;
    const r = root;
    try {
      const list = await api.fsList(r);
      if (r !== root) return;
      tree = list.map((k) => ({ ...k }));
      expanded = {};
    } catch (e) {
      toast("error", String(e));
    }
  }

  /** 新列表与旧节点合并：同路径同类型的目录沿用已加载的子节点（展开状态不丢）。 */
  function merge(old: Node[] | undefined, fresh: DirEntryInfo[]): Node[] {
    const byPath = new Map((old ?? []).map((n) => [n.path, n]));
    return fresh.map((k) => {
      const o = byPath.get(k.path);
      if (o && o.is_dir === k.is_dir) {
        o.size = k.size;
        o.mtime = k.mtime;
        o.name = k.name;
        return o;
      }
      return { ...k };
    });
  }

  function findNode(nodes: Node[], path: string): Node | null {
    for (const n of nodes) {
      if (n.path === path) return n;
      if (n.children && path.startsWith(n.path)) {
        const hit = findNode(n.children, path);
        if (hit) return hit;
      }
    }
    return null;
  }

  /** 重新读取一个目录（根或已加载的子目录），合并保留子树。 */
  async function reloadDir(path: string) {
    if (!root) return;
    if (path === root) {
      const r = root;
      const list = await api.fsList(r);
      if (r !== root) return;
      tree = merge(tree, list);
      return;
    }
    const node = findNode(tree, path);
    if (!node || !node.is_dir) return;
    try {
      node.children = merge(node.children, await api.fsList(path));
    } catch {
      node.children = [];
    }
  }

  async function loadChildren(node: Node): Promise<Node[]> {
    try {
      return (await api.fsList(node.path)).map((k) => ({ ...k }));
    } catch {
      return [];
    }
  }

  // ---------- 自动刷新：轮询「根 + 已展开目录」的签名（只读目录项，开销极低），变化才重载 ----------
  let sigs = new Map<string, string | null>();
  let polling = false;

  function watchedDirs(): string[] {
    const out = [root];
    const walk = (nodes: Node[]) => {
      for (const n of nodes) {
        if (out.length >= 200) return;
        if (n.is_dir && expanded[n.path] && n.children) {
          out.push(n.path);
          walk(n.children);
        }
      }
    };
    walk(tree);
    return out;
  }

  async function poll(force = false) {
    if (!root || polling) return;
    if (!force && document.visibilityState !== "visible") return;
    polling = true;
    const r = root;
    try {
      const dirs = watchedDirs();
      const res = await api.fsDirSigs(dirs);
      if (r !== root) return;
      const changed: string[] = [];
      dirs.forEach((d, i) => {
        const prev = sigs.get(d);
        if (prev !== undefined && prev !== res[i]) changed.push(d);
        sigs.set(d, res[i]);
      });
      // 浅层先刷新：子目录被删除时其父目录的重载会把它去掉
      changed.sort((a, b) => a.length - b.length);
      for (const d of changed) await reloadDir(d).catch(() => {});
    } catch {
      /* 后端暂不可用：下次再试 */
    } finally {
      polling = false;
    }
  }

  async function syncAll() {
    if (!root) return;
    for (const d of watchedDirs()) await reloadDir(d).catch(() => {});
    void poll(true);
  }

  $effect(() => {
    if (!root) return;
    const timer = setInterval(() => void poll(), 3000);
    const onFocus = () => void poll(true);
    window.addEventListener("focus", onFocus);
    return () => {
      clearInterval(timer);
      window.removeEventListener("focus", onFocus);
    };
  });

  async function toggle(node: Node) {
    if (!node.is_dir) {
      await openFile(node);
      return;
    }
    if (!node.children) node.children = await loadChildren(node);
    expanded[node.path] = !expanded[node.path];
  }

  // ---------- 单击预览 / 双击系统打开：单击延迟执行，双击到来时取消，互不触发 ----------
  const CLICK_DELAY = 300;
  let clickTimer: ReturnType<typeof setTimeout> | null = null;

  function cancelPendingClick() {
    if (clickTimer) clearTimeout(clickTimer);
    clickTimer = null;
  }

  function onRowClick(e: MouseEvent, n: Node) {
    if (renaming === n.path) return;
    selected = n.path;
    // 双击的第二次 click（detail ≥ 2）不再触发单击行为：目录不会「展开又收起」
    if (e.detail > 1) return;
    if (n.is_dir) {
      void toggle(n);
      return;
    }
    cancelPendingClick();
    clickTimer = setTimeout(() => {
      clickTimer = null;
      void openFile(n);
    }, CLICK_DELAY);
  }

  function onRowDblClick(e: MouseEvent, n: Node) {
    e.preventDefault();
    cancelPendingClick();
    if (!n.is_dir) openWithDefault(n);
  }

  function onRowKey(e: KeyboardEvent, n: Node) {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      selected = n.path;
      void toggle(n);
    }
  }

  async function openFile(node: Node) {
    try {
      const [content, binary] = await api.fsRead(node.path);
      openEditor(node.path, content, binary);
      selected = node.path;
      app.tab = "chat"; // 编辑器与聊天共存，打开文件时切到聊天页展示
    } catch (e) {
      toast("error", String(e));
    }
  }

  // 双击：使用系统默认应用打开文件（目录保持单击展开）
  function openWithDefault(node: Node) {
    if (node.is_dir) return;
    api.fsOpenDefault(node.path).catch((err) => toast("error", String(err)));
  }

  /** 拼接子路径：盘符根 `D:\` 不会拼出双反斜杠 */
  function joinPath(dir: string, name: string) {
    return /[\\/]$/.test(dir) ? dir + name : dir + "\\" + name;
  }

  function parentPath(p: string) {
    const trimmed = p.replace(/[\\/]+$/, "");
    const i = Math.max(trimmed.lastIndexOf("\\"), trimmed.lastIndexOf("/"));
    if (i < 0) return p;
    const parent = trimmed.slice(0, i);
    // `D:\a` 的父目录是 `D:\`（裸 `D:` 表示当前目录，不是盘符根）
    return /^[A-Za-z]:$/.test(parent) ? parent + "\\" : parent || p;
  }

  async function reloadParent(p: string) {
    await reloadDir(p);
  }

  function errToast(err: unknown) {
    toast("error", String(err));
  }

  async function createIn(dir: string, kind: "file" | "dir") {
    const name = await promptDialog(
      kind === "file" ? { title: t("新建文件"), label: t("文件名 *") } : { title: t("新建文件夹"), label: t("文件夹名 *") },
    );
    if (name === null || !name.trim()) return;
    try {
      const path = joinPath(dir, name.trim());
      if (kind === "file") await api.fsCreateFile(path);
      else await api.fsCreateDir(path);
      if (dir !== root) {
        const n = findNode(tree, dir);
        if (n && !expanded[dir]) {
          if (!n.children) n.children = await loadChildren(n);
          expanded[dir] = true;
        }
      }
      await reloadParent(dir);
    } catch (err) {
      errToast(err);
    }
  }

  // 空白处右键：针对项目根目录的操作
  function blankMenu(e: MouseEvent) {
    e.preventDefault();
    e.stopPropagation();
    if (!root) return;
    const items: MenuItem[] = [
      { label: t("📄 新建文件"), run: () => void createIn(root, "file") },
      { label: t("📁 新建文件夹"), run: () => void createIn(root, "dir") },
      { label: t("📋 粘贴到根目录"), run: () => api.fsPasteFromClipboard(root).then(() => reloadParent(root)).then(() => toast("ok", t("已粘贴"))).catch(errToast) },
      "sep",
      { label: t("🖥 在 cmd 打开"), run: () => api.fsOpenCmd(root).catch(errToast) },
      { label: t("🖥 在 PowerShell 打开"), run: () => api.fsOpenTerminal(root).catch(errToast) },
      { label: t("📂 在资源管理器打开"), run: () => api.fsOpenExplorer(root).catch(errToast) },
      { label: t("⌨ 在 VS Code 中打开"), run: () => api.fsOpenVscode(root).catch(errToast) },
      "sep",
      { label: t("🔍 全局搜索 (Alt+F)"), run: () => (app.searchOpen = true) },
      { label: t("📋 复制根目录路径"), run: () => navigator.clipboard.writeText(root).then(() => toast("ok", t("路径已复制"))).catch(errToast) },
      { label: t("🔄 刷新"), run: () => void syncAll() },
      { label: t("⤒ 全部折叠"), run: () => (expanded = {}) },
    ];
    menu = { x: e.clientX, y: e.clientY, items };
  }

  function nodeMenu(e: MouseEvent, node: Node) {
    e.preventDefault();
    e.stopPropagation();
    const dir = node.is_dir ? node.path : parentPath(node.path);
    const items: MenuItem[] = [
      { label: t("📋 复制路径"), run: () => navigator.clipboard.writeText(node.path).then(() => toast("ok", t("路径已复制"))).catch((err) => toast("error", String(err))) },
      { label: node.is_dir ? t("📂 打开所在位置") : t("📂 在所在位置显示"), run: () => api.fsOpenExplorer(dir).catch((err) => toast("error", String(err))) },
      {
        label: t("✏️ 重命名"),
        run: () => {
          renaming = node.path;
          renameValue = node.name;
        },
      },
      { label: t("⧉ 复制"), run: () => api.fsCopyToClipboard([node.path]).then(() => toast("ok", t("已复制到系统剪贴板"))).catch((err) => toast("error", String(err))) },
      { label: t("📋 粘贴到该文件夹"), run: () => api.fsPasteFromClipboard(dir).then(() => reloadParent(dir)).then(() => toast("ok", t("已粘贴"))).catch((err) => toast("error", String(err))) },
      "sep",
      { label: t("🖥 在 cmd 打开"), run: () => api.fsOpenCmd(dir).catch((err) => toast("error", String(err))) },
      { label: t("🖥 在 PowerShell 打开"), run: () => api.fsOpenTerminal(dir).catch((err) => toast("error", String(err))) },
      { label: t("📄 新建文件"), run: () => void createIn(dir, "file") },
      { label: t("📁 新建文件夹"), run: () => void createIn(dir, "dir") },
      "sep",
      {
        label: t("🗑 移入回收站"),
        danger: true,
        run: () => {
          void (async () => {
            if (!(await confirmDialog({ title: t("删除"), message: t("将 {p0} 移入回收站？", { p0: node.name }), danger: true, confirmText: t("移入回收站") }))) return;
            try {
              await api.fsDelete(node.path);
              toast("ok", t("已删除"));
              if (app.editor?.path === node.path || app.editor?.path.startsWith(node.path + "\\")) closeEditor();
              await reloadParent(parentPath(node.path));
            } catch (err) {
              toast("error", String(err));
            }
          })();
        },
      },
    ];
    menu = { x: e.clientX, y: e.clientY, items };
  }

  async function commitRename() {
    if (!renaming) return;
    const from = renaming;
    const parent = parentPath(from);
    const to = joinPath(parent, renameValue.trim());
    renaming = null;
    if (!renameValue.trim() || to === from) return;
    try {
      await api.fsRename(from, to);
      await reloadParent(parent);
      toast("ok", t("已重命名"));
    } catch (e) {
      toast("error", String(e));
    }
  }
</script>

<div class="tree-pane">
  <div class="bar">
    <Icon name="folder" size={14} />
    <span class="title" title={root}>{project?.name ?? t("文件")}</span>
    <span class="spacer"></span>
    <button class="btn ghost sm" title={t("全局搜索：文件名 / 内容 (Alt+F)")} onclick={() => (app.searchOpen = true)}><Icon name="search" size={13} /></button>
    <button class="btn ghost sm" title={t("资源管理器")} onclick={() => api.fsOpenExplorer(root).catch((e) => toast("error", String(e)))}>↗</button>
    <button class="btn ghost sm" title={t("在 VS Code 中打开")} onclick={() => api.fsOpenVscode(root).catch((e) => toast("error", String(e)))}><Icon name="code" size={13} /></button>
  </div>
  <div class="tree" role="tree" tabindex="-1" oncontextmenu={blankMenu}>
    {#each tree as node (node.path)}
      {#snippet nodeRow(n: Node, depth: number)}
        <div
          class="row"
          class:selected={selected === n.path || (app.editor?.path ?? "").startsWith(n.path + "\\")}
          style="padding-left:{8 + depth * 14}px"
          role="button"
          tabindex="0"
          onclick={(e) => onRowClick(e, n)}
          ondblclick={(e) => onRowDblClick(e, n)}
          onkeydown={(e) => onRowKey(e, n)}
          oncontextmenu={(e) => nodeMenu(e, n)}
        >
          {#if renaming === n.path}
            <input
              class="rename"
              bind:value={renameValue}
              onclick={(e) => e.stopPropagation()}
              onkeydown={(e) => {
                if (e.key === "Enter") void commitRename();
                if (e.key === "Escape") renaming = null;
              }}
              onblur={commitRename}
            />
          {:else}
            <span class="icon">{n.is_dir ? (expanded[n.path] ? "📂" : "📁") : "📄"}</span>
            <span class="name" title={n.path}>{n.name}</span>
          {/if}
        </div>
        {#if n.is_dir && expanded[n.path] && n.children}
          {#each n.children as child (child.path)}
            {@render nodeRow(child, depth + 1)}
          {/each}
        {/if}
      {/snippet}
      {@render nodeRow(node, 0)}
    {/each}
  </div>
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={() => (menu = null)} />
{/if}

<style>
  .tree-pane {
    display: flex;
    flex-direction: column;
    min-height: 0;
    width: 100%;
    background: var(--bg-panel);
  }
  .bar {
    display: flex;
    align-items: center;
    gap: 3px;
    padding: 7px 8px;
    border-bottom: 1px solid var(--border-soft);
    color: var(--text-dim);
  }
  .title {
    font-weight: 600;
    font-size: 0.88em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    margin-right: auto;
  }
  .tree {
    overflow: auto;
    flex: 1;
    padding: 6px 4px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 3.5px 6px;
    border-radius: 6px;
    cursor: pointer;
    color: var(--text-dim);
    white-space: nowrap;
    font-size: 0.88em;
  }
  .row:hover {
    background: var(--bg-elev);
    color: var(--text);
  }
  .row.selected {
    background: var(--bg-elev2);
    color: var(--text);
  }
  .icon {
    font-size: 0.9em;
  }
  .chev {
    width: 10px;
    flex: none;
    color: var(--text-faint);
    font-size: 0.8em;
    text-align: center;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .rename {
    padding: 1px 6px;
    font-size: 0.9em;
    width: 100%;
  }
</style>
