# 使驾 ShiDrive

> AI 是发动机，Agent 是车辆，使驾是驾驶席。

使驾（ShiDrive）是一款基于 **Tauri 2** 的本地 AI 开发工作台：把项目、工作上下文、AI Agent 会话与工作流放在同一驾驶席上统一管理。它不是又一个 AI 编码工具，而是让你"驾驶"多个 ACP Agent（Codex / ZCode / Claude / Cline / Cursor / Qoder …）完成日常开发工作。

## 功能一览

- **项目与上下文**：项目分组、多工作上下文；上下文可绑定多个 Agent 会话，切换即恢复。
- **AI 会话（ACP）**：新建 / 绑定历史会话、流式回复、工具调用时间线、权限请求对话框、模型与模式切换、Ctrl+F 正文搜索、会话标题编辑；会话状态（对话中 / 已完成 / 已中断）集中管理。
- **多 Agent 注册表**：内置 Codex、ZCode、Claude、Cline、Codebuddy、Cursor、MiniMax、OpenCode、Qoder、Grok 十种 ACP 工具。启用顺序即会话页标签顺序；npm 类适配器在设置页一键安装（支持 HTTP 代理、npmmirror 镜像回退、`--omit=optional` 跳过大体积平台二进制）。
- **图形化工作流**：画布拖拽 / 自由连线 / 双击编辑；开始、注释、气泡提醒、环境变量、命令（cmd/powershell/python）、Agent 调度、延时等节点；并行分支、定时与手动触发、运行日志、复制粘贴、自动保存。
- **共享上下文 MCP**：本地 HTTP MCP 服务（默认 `127.0.0.1:8345`），把项目上下文以 git 式提交模型开放给 Agent——`context_get/update/history/search` + 工作流六工具，带版本冲突检测与字节限额。
- **Coding MCP（独立模式）**：`shidrive.exe --coding-mcp` 以独立进程暴露文件与命令工具（路径越界防护、Token/Basic 认证、执行开关）。
- **文件树与编辑器**：目录树、系统剪贴板互操作、终端/资源管理器打开、内置编辑器。
- **自定义主题**：深浅色、强调色、圆角、字号；自定义标题栏与对话框，无系统弹窗。

## 架构

```
前端 Svelte 5 (runes) + Vite
  │  invoke / event
后端 Rust (Tauri 2)
  ├── acp.rs        ACP 客户端：stdio JSON-RPC、权限/文件请求、回合转录
  ├── agents.rs     Agent 注册表：探测 / 适配器安装 / 启用顺序
  ├── manager.rs    会话生命周期：启动、绑定、历史重放、配置项
  ├── mcp.rs        共享上下文 MCP 服务（/skills、/mcp）
  ├── coding_mcp.rs 独立 Coding MCP（--coding-mcp）
  ├── engine.rs     工作流引擎：图执行 / 定时 / 模板变量
  └── db.rs         SQLite（项目 / 上下文 / 绑定 / 工作流 / 提交）
```

## 构建与运行

```powershell
cd app
pnpm install
pnpm check            # svelte-check
pnpm tauri dev        # 开发运行
pnpm tauri build      # NSIS 安装包
pnpm tauri build --no-bundle   # 仅产出 target/release/shidrive.exe
```

要求：Rust 1.88+、Node ≥ 20（构建）；Windows 10/11（WebView2）。详细说明见 [app/README.md](app/README.md)。

## 发布结构（免环境依赖）

```
ShiDrive/
├── shidrive.exe
└── .tools/
    └── node22/        # Node ≥22 便携版（含 npm），适配器安装与启动使用
```

- 各 ACP 工具的 CLI（codex.exe / zcode.cjs / claude 等）按标准路径自动发现。
- 适配器在「设置 → Agent 管理」中按需安装到用户数据目录，可用代理加速。
- `--debug` 启动写文件日志到 `%APPDATA%\com.shidrive.desktop\logs\shidrive.log`。

## 状态

个人项目，活跃开发中。接口与数据结构仍可能调整，欢迎提 issue 讨论。

## License

[MIT](LICENSE)
