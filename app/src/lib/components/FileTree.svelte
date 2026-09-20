<script lang="ts">
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

  $effect(() => {
    // 依赖 treeRev：回合结束后自动刷新（保留展开状态）；首次加载复位展开
    void app.treeRev;
    if (root) void refresh(app.treeRev > 0);
    else closeEditor();
  });

  async function refresh(keepExpansion = false) {
    if (!root) return;
    try {
      tree = await api.fsList(root);
      if (!keepExpansion) expanded = {};
    } catch (e) {
      toast("error", String(e));
    }
  }

  async function loadChildren(node: Node): Promise<Node[]> {
    try {
      return (await api.fsList(node.path)).map((k) => ({ ...k }));
    } catch {
      return [];
    }
  }

  async function toggle(node: Node) {
    if (!node.is_dir) {
      await openFile(node);
      return;
    }
    if (!node.children) node.children = await loadChildren(node);
    expanded[node.path] = !expanded[node.path];
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

  function parentPath(p: string) {
    const i = Math.max(p.lastIndexOf("\\"), p.lastIndexOf("/"));
    return i > 0 ? p.slice(0, i) : p;
  }

  async function reloadParent(p: string) {
    if (p === root) {
      tree = await api.fsList(root);
    } else {
      await refreshIn(tree, p);
    }
  }

  async function refreshIn(nodes: Node[], path: string): Promise<boolean> {
    for (const n of nodes) {
      if (n.path === path) {
        n.children = await loadChildren(n);
        return true;
      }
      if (n.children && (await refreshIn(n.children, path))) return true;
    }
    return false;
  }

  function nodeMenu(e: MouseEvent, node: Node) {
    e.preventDefault();
    e.stopPropagation();
    const dir = node.is_dir ? node.path : parentPath(node.path);
    const items: MenuItem[] = [
      { label: "📋 复制路径", run: () => navigator.clipboard.writeText(node.path).then(() => toast("ok", "路径已复制")).catch((err) => toast("error", String(err))) },
      { label: node.is_dir ? "📂 打开所在位置" : "📂 在所在位置显示", run: () => api.fsOpenExplorer(dir).catch((err) => toast("error", String(err))) },
      {
        label: "✏️ 重命名",
        run: () => {
          renaming = node.path;
          renameValue = node.name;
        },
      },
      { label: "⧉ 复制", run: () => api.fsCopyToClipboard([node.path]).then(() => toast("ok", "已复制到系统剪贴板")).catch((err) => toast("error", String(err))) },
      { label: "📋 粘贴到该文件夹", run: () => api.fsPasteFromClipboard(dir).then(() => reloadParent(dir)).then(() => toast("ok", "已粘贴")).catch((err) => toast("error", String(err))) },
      "sep",
      { label: "🖥 在 cmd 打开", run: () => api.fsOpenCmd(dir).catch((err) => toast("error", String(err))) },
      { label: "🖥 在 PowerShell 打开", run: () => api.fsOpenTerminal(dir).catch((err) => toast("error", String(err))) },
      {
        label: "📄 新建文件",
        run: () => {
          void (async () => {
            const name = await promptDialog({ title: "新建文件", label: "文件名 *" });
            if (name === null || !name.trim()) return;
            try {
              await api.fsCreateFile(dir + "\\" + name);
              await reloadParent(dir);
            } catch (err) {
              toast("error", String(err));
            }
          })();
        },
      },
      {
        label: "📁 新建文件夹",
        run: () => {
          void (async () => {
            const name = await promptDialog({ title: "新建文件夹", label: "文件夹名 *" });
            if (name === null || !name.trim()) return;
            try {
              await api.fsCreateDir(dir + "\\" + name);
              await reloadParent(dir);
            } catch (err) {
              toast("error", String(err));
            }
          })();
        },
      },
      "sep",
      {
        label: "🗑 移入回收站",
        danger: true,
        run: () => {
          void (async () => {
            if (!(await confirmDialog({ title: "删除", message: `将 ${node.name} 移入回收站？`, danger: true, confirmText: "移入回收站" }))) return;
            try {
              await api.fsDelete(node.path);
              toast("ok", "已删除");
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
    const to = parent + "\\" + renameValue.trim();
    renaming = null;
    if (!renameValue.trim() || to === from) return;
    try {
      await api.fsRename(from, to);
      await reloadParent(parent);
      toast("ok", "已重命名");
    } catch (e) {
      toast("error", String(e));
    }
  }
</script>

<div class="tree-pane">
  <div class="bar">
    <Icon name="folder" size={14} />
    <span class="title" title={root}>{project?.name ?? "文件"}</span>
    <span class="spacer"></span>
    <button class="btn ghost sm" title="刷新" onclick={() => refresh()}><Icon name="refresh" size={13} /></button>
    <button class="btn ghost sm" title="资源管理器" onclick={() => api.fsOpenExplorer(root).catch((e) => toast("error", String(e)))}>↗</button>
    <button class="btn ghost sm" title="在 VS Code 中打开" onclick={() => api.fsOpenVscode(root).catch((e) => toast("error", String(e)))}><Icon name="code" size={13} /></button>
  </div>
  <div class="tree">
    {#each tree as node (node.path)}
      {#snippet nodeRow(n: Node, depth: number)}
        <div
          class="row"
          class:selected={selected === n.path || (app.editor?.path ?? "").startsWith(n.path + "\\")}
          style="padding-left:{8 + depth * 14}px"
          role="button"
          tabindex="0"
          onclick={() => toggle(n)}
          ondblclick={(e) => {
            e.preventDefault();
            if (!n.is_dir) openWithDefault(n);
          }}
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
