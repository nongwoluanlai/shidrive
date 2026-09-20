# 使驾 ShiDrive

> **AI 是发动机，Agent 是车辆，使驾是驾驶席。** / Drive Your AI.

统一管理 **Project、Context、AI Agent Session 与 Workflow** 的本地 AI 开发工作台。
Tauri 2 + Svelte 5 + Rust + SQLite 实现，不依赖云端。

```
Project（下拉切换）
  └── Context（不绑定某一种 AI）
       ├── Codex Session      （ACP：@agentclientprotocol/codex-acp，可新建 / 绑定历史）
       ├── ZCode Session      （ACP：zcode-acp-server，可新建 / 绑定历史）
       ├── 概述 / 待办 / 进展 / 决策 / 注意 / 约束（共享上下文工作区）
       └── Workflow（图形化画布，手动 / 定时）
共享上下文：HTTP MCP 服务（默认 127.0.0.1:8345），git 式提交历史
```

## 功能总览

- **自定义标题栏**：应用图标 + 会话管理 / 运行中任务 / 常用提示词 / 设置 + 窗口控制。
- **侧栏**：项目下拉切换；上半部分上下文列表、下半部分本项目工作流；左下角「上下文」
  按钮跟随当前项目打开上下文编辑；Codex / ZCode 连接状态一键切换。
- **会话管理**（右上角）：跨项目查看全部会话绑定，支持前往、解绑、一键复制
  「共享上下文接入提示词」。
- **聊天**：流式 Markdown、思考过程折叠、工具调用卡片、计划清单、权限弹窗、停止、
  会话模式与模型切换；**新建会话 / 绑定历史会话**（读取适配器 session/list）/
  重新连接；一键复制接入提示词。
- **上下文**：概述与约束自动保存；待办 / 进展 / 决策 / 注意条目管理；**Agent 通过 MCP
  提交的共享上下文更新会实时同步到界面**。
- **共享上下文（git 式）**：
  - 使驾内置 MCP 服务：`GET /skills?sharedsession=<contextId>` 返回技能文档；
    `POST /mcp` 提供 `context_get` / `context_get_version` / `context_update` /
    `context_history` / `context_search` 五个工具；
  - `context_update` 携带 `base_version`，版本落后时按 git 冲突拒绝并指引先读后合；
  - 每次提交记录摘要 + 涉及文件 + 全量快照，长内容限字并提示精简，历史可搜索；
  - 新建 ACP 会话时自动注入该 MCP 服务（适配器不支持时自动降级）。
- **文件**：右侧常驻文件树（新建 / 重命名 / 删除进回收站 / 打开终端与资源管理器）；
  文件预览编辑器可收起：聊天页对半展开，其他页面完全展开，自动保存。
- **工作流**：轻量 SVG 节点画布（拖拽排列、按执行顺序连线）；节点类型：
  命令（cmd / powershell / python）、Agent（指定上下文与会话，留空会话 ID 即临时会话）、
  等待、环境变量（`{{env.X}}` / `{{date:格式}}` 模板，命令输出写入
  `<名称>_stdout / _stderr / _exit`）；触发支持手动与定时（间隔 / 每天 / 每周 / 单次）；
  运行历史与实时日志；右上角「运行中任务」可随时停止。
- **常用提示词**：跨会话维护，一键插入聊天输入框或复制。
- **自定义主题**：深色 / 浅色 + 强调色 + 圆角 + 字号，持久化。

## 目录结构

```
app/
├── src/                      # Svelte 5 前端
│   ├── App.svelte            # 标题栏 + 侧栏 + 主区 + 右侧文件坞布局
│   ├── lib/
│   │   ├── state.svelte.ts   # 全局响应式状态（runes）+ 共享上下文渲染
│   │   ├── events.ts         # 后端事件接线（acp://*、wf://*、sc://*）
│   │   ├── ipc.ts / theme.ts / markdown.ts
│   │   └── components/       # TitleBar / Sidebar / ChatView / ContextView /
│   │                         # WorkflowsView(画布) / FileTree / FileEditor /
│   │                         # SessionManagerModal / TasksPanel / PromptsPanel /
│   │                         # HistoryBindModal / SettingsModal …
└── src-tauri/                # Rust 后端
    └── src/
        ├── acp.rs            # ACP 客户端：stdio JSON-RPC、权限/文件请求、回合转录
        ├── manager.rs        # 连接复用、会话新建/恢复/绑定/列表、MCP 注入
        ├── mcp.rs            # HTTP MCP 服务 + git 式共享上下文引擎 + 技能文档
        ├── engine.rs         # 工作流引擎 v2 + 定时调度器
        ├── db.rs             # SQLite（…/sc_commits 提交历史）
        ├── setup.rs          # 工具路径发现（全部可在设置覆盖，无硬编码）
        └── commands.rs
```

## 环境要求

- Rust 1.88+、Node ≥ 20 构建；**运行 ZCode 桥接需要 Node ≥ 22**（未随包内置以控制体积；
  在 `.tools/node22` 放置便携版即可被自动发现，或在设置中指定路径）。
- Windows 10/11（WebView2）。
- 适配器（Zed 式按需安装，无需手动准备）：在「设置 → Agent 管理」展开对应工具点
  「安装适配器」，自动 `npm install` 到用户数据目录 `%APPDATA%\com.shidrive.desktop\tools\acp`
  （`--omit=optional` 跳过大体积平台二进制；默认源失败自动改用 npmmirror 镜像；
  可在「环境与路径」配置仅用于此处的 HTTP 代理）。
  `.tools/acp` 下的手工安装仍然优先被识别。
- **发布结构（免环境依赖）**：`shidrive.exe` 旁放 `.tools\node22\`（Node ≥22 便携版，
  含自带 npm）即可在无 Node 的机器上安装适配器；Node 也可在设置中手动指定。
- Codex CLI / ZCode 桌面端按标准路径自动发现；ZCode CLI 路径可在「Agent 管理」的
  环境变量里用 `ZCODE_BIN` 指定。

## 开发与构建

```powershell
cd app
pnpm install
pnpm check            # svelte-check
pnpm tauri dev        # 开发运行
pnpm tauri build      # NSIS 安装包（推荐；CLI 自动携带 custom-protocol feature）
pnpm tauri build --no-bundle   # 仅产出 target/release/shidrive.exe，便于便携部署
```

发布提示：release exe 必须携带 `custom-protocol` feature 才会内嵌 `dist` 前端资源；
直接 `cargo build --release` 不带该 feature 时，exe 会去连 devUrl（localhost:1420）
导致其它机器上白屏。`pnpm tauri build` 已自动处理，无需手动传参。

开发提示：若长时间运行 `pnpm dev` 后界面出现"图标/文案是旧版"，是 vite 模块缓存失同步，
重启 `pnpm dev` 并强刷页面（Page.reload）即可。

## 验证共享上下文

```powershell
# 技能文档（Agent 会按文档调用 MCP 工具）
curl "http://127.0.0.1:8345/skills?sharedsession=<contextId>"
# 健康检查
curl http://127.0.0.1:8345/health
```

在聊天页点击「复制接入提示词」，把类似下面的消息发给 Agent 即可完成同步：

```
http://127.0.0.1:8345/skills?sharedsession=<contextId> ，请阅读这个技能，确认会话同步到共享上下文中
```
