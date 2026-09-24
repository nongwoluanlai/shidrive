# 使驾 ShiDrive

[English](README_EN.md) | 中文

> **One Context. Any Harness.** —— 让 Harness 并驾齐驱。

使驾（ShiDrive）是一款基于 **Tauri 2** 的本地 AI 开发工作台：把项目、工作上下文、AI Agent 会话与工作流放在同一驾驶席上统一管理。它不是又一个 AI 编码工具，而是让你"驾驶"多个 ACP Agent（Codex / ZCode / Claude / Cline / Cursor / Qoder …）完成日常开发工作——同一份上下文，驱动任意 Harness。

## 功能一览
- **项目与上下文**：项目分组、多工作上下文；上下文可绑定多个 Agent 会话，切换即恢复。
<img width="2250" height="1303" alt="image" src="https://github.com/user-attachments/assets/686f9065-44f2-4aa2-af07-db07f9938223" />
- **AI 会话（ACP）**：新建 / 绑定历史会话、流式回复、工具调用时间线、权限请求对话框、模型与模式切换、Ctrl+F 正文搜索、会话标题编辑；会话状态（对话中 / 已完成 / 已中断）集中管理。
- **多 Agent 注册表**：内置 Codex、ZCode、Claude、Cline、Codebuddy、Cursor、MiniMax、OpenCode、Qoder、Grok 十种 ACP 工具。启用顺序即会话页标签顺序；npm 类适配器在设置页一键安装（支持 HTTP 代理、npmmirror 镜像回退、`--omit=optional` 跳过大体积平台二进制）。
<img width="892" height="793" alt="image" src="https://github.com/user-attachments/assets/b9bd3ffe-df63-4654-9694-c6974f1cf61b" />
- **图形化工作流**：画布拖拽 / 自由连线 / 双击编辑；开始、注释、气泡提醒、环境变量、命令（cmd/powershell/python）、Agent 调度、延时等节点；并行分支、定时与手动触发、运行日志、复制粘贴、自动保存。
<img width="1272" height="1298" alt="image" src="https://github.com/user-attachments/assets/812ee760-deea-4384-92f8-3cab691a1eed" />

- **共享上下文 MCP**：本地 HTTP MCP 服务（默认 `127.0.0.1:8345`），把项目上下文以 git 式提交模型开放给 Agent——`context_get/update/history/search` + 工作流六工具，带版本冲突检测与字节限额。
- **Coding MCP（独立模式）**：`shidrive.exe --coding-mcp` 以独立进程暴露文件与命令工具（路径越界防护、Token/Basic 认证、执行开关）。
- **外部编程接入**：Passcode / OAuth 双接入；OAuth 请求由桌面端全局审批，复用「开放目录与上下文」中的当前权限，支持多项目访问、令牌吊销和标准 MCP 自动发现；详见 [OAuth 合并说明](docs/oauth-global-permissions.md)。
- **文件树与编辑器**：目录树、系统剪贴板互操作、终端/资源管理器打开、内置编辑器。
- **桌面集成**：自定义标题栏与对话框、深浅色主题；关闭窗口最小化到托盘常驻（右键打开/退出）、开机自启可选。
- **免环境依赖**：Node 运行时（≥22）自动检测，或一键下载便携版到用户数据目录（不影响系统环境）；适配器按需安装，全程可配代理。
- **自定义主题**：可以参考图片
<img width="2560" height="1390" alt="image" src="https://github.com/user-attachments/assets/1221c69e-a69b-4fab-ba0d-126868d88cc0" />
<img width="2560" height="1390" alt="image" src="https://github.com/user-attachments/assets/15fb9b11-24a4-479e-a09a-c152f67fbc04" />

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

## 运行时与适配器（免环境依赖）

- **Node 运行时**：自动检测系统 Node（≥22 才自动采用）；不满足时可在设置中一键下载便携版
  node22（约 35MB）到用户数据目录，或自行指定路径——不影响系统 Node 环境。
  随包分发时把 node22 放在 `shidrive.exe` 旁的 `.tools\node22\` 即可被自动发现。
- **适配器**：在「设置 → Agent 管理」展开对应工具点「安装适配器」，自动 `npm install` 到
  用户数据目录 `%APPDATA%\com.shidrive.desktop\tools\acp`（`--omit=optional` 跳过大体积
  平台二进制；默认源失败自动改用 npmmirror 镜像；代理仅作用于使驾自身的下载）。
- **各工具 CLI**（codex.exe / zcode.cjs / claude 等）按标准路径自动发现，按需登录。
- `--debug` 启动写文件日志到 `%APPDATA%\com.shidrive.desktop\logs\shidrive.log`。

## 参考与致谢

- [Zed](https://github.com/zed-industries/zed) —— ACP 连接层（`agent_servers`/`acp.rs`）与
  "适配器按需安装到数据目录"的托管形式参考。
- [Agent Client Protocol](https://agentclientprotocol.com) /
  [agentclientprotocol/registry](https://github.com/agentclientprotocol/registry) —— 协议规范
  与 Agent 注册表；适配器清单参考其 registry 数据。
- 适配器包：[@agentclientprotocol/codex-acp](https://www.npmjs.com/package/@agentclientprotocol/codex-acp)、
  [zcode-acp-server](https://www.npmjs.com/package/zcode-acp-server)、
  [@zed-industries/claude-code-acp](https://www.npmjs.com/package/@zed-industries/claude-code-acp)。
- [Tauri](https://tauri.app) / [Svelte](https://svelte.dev)。

## 状态

个人项目，活跃开发中。接口与数据结构仍可能调整，欢迎提 issue 讨论。

## License

[MIT](LICENSE)
