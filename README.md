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
- **全局助手**：独立于项目会话的长期对话伙伴，左下角（或皮肤人物旁）气泡；大脑可选已安装的 ACP Agent 或 OpenAI 兼容 API；记住你的偏好（长期记忆），人设与说话风格跟随皮肤角色；你不忙时她会偶尔主动关怀（深夜、久坐、饭点、未完成待办…，可设最短间隔，每天最多 6 次）；标题栏附带轻量**待办**面板。
- **语音**：双击 Ctrl / 唤醒词（默认「Christina」，离线检测）唤起，支持多轮语音对话；识别用本地 SenseVoice（离线）或百度短语音识别；朗读用本地 MeloTTS / Kokoro、系统语音或**角色语音包**（如牧濑红莉栖 · Amadeus 日语，字幕仍显示界面语言）。语音运行环境与模型都在设置里按需下载，不装进系统。
- **皮肤**：内置「命运石之门」「地狱乐」两套，更多皮肤（含动态人物）从皮肤商店获取；皮肤包（`skin.json` + 图片，zip）可拖入导入，支持人物立绘、动态特效、时间线插画、不透明度调节，并可声明绑定的角色语音包。制作方法见 [皮肤开发指南](docs/skin-guide.md)。
<img width="2560" height="1390" alt="image" src="https://github.com/user-attachments/assets/1221c69e-a69b-4fab-ba0d-126868d88cc0" />
<img width="2560" height="1390" alt="image" src="https://github.com/user-attachments/assets/15fb9b11-24a4-479e-a09a-c152f67fbc04" />

- **全局搜索**：文件树里 `Alt+F` 打开，文件名快速打开 + 内容搜索。
- **网站接入**：内置轻量 Web 服务（默认端口 5165），配合自备反代隧道 / 局域网，在其它设备的浏览器里选择项目与上下文、和 Agent 对话、运行工作流、浏览文件；访客码登录、错误次数限制。
- **桌面集成**：自定义标题栏与对话框、深浅色主题；使驾自有的通知弹窗（不抢焦点）；关闭窗口最小化到托盘常驻（右键打开/退出）、开机自启可选；单实例运行——重复启动（任务栏再点一次）只唤出已打开的主窗口。
- **免环境依赖**：Node 运行时（≥22）自动检测，或一键下载便携版到用户数据目录（不影响系统环境）；适配器、语音运行环境与模型都按需安装，全程可配代理 / 下载加速。

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
  ├── remote_mcp.rs 外部编程接入（Passcode / OAuth，remote_oauth.rs）
  ├── assistant.rs  全局助手：长期会话、记忆、主动关怀
  ├── voice.rs      语音：运行环境 / 模型下载、识别、TTS、唤醒词、角色语音包
  ├── skins.rs      皮肤包导入与校验
  ├── search.rs     全局搜索（Alt+F）
  ├── web.rs        网站接入（内置 Web 服务）
  ├── toast.rs      自有通知弹窗
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

## 资源与更新（皮肤 / 语音）

为了让安装包保持轻量，**大体积资源不进安装包、也不进 git 仓库**，按需获取：

| 资源 | 怎么获取 | 怎么更新 |
| --- | --- | --- |
| 应用本体 + 内置皮肤 | [Releases](https://github.com/nongwoluanlai/shidrive/releases) 的安装包 / 便携版 zip | 下载新版本覆盖即可，用户数据（`%APPDATA%\com.shidrive.desktop`）保留 |
| 社区皮肤 | 官网 [皮肤商店](https://shidrive.nwll.top/skinstore) 下载 zip → 「设置 → 外观主题」导入（或直接拖入） | 在皮肤列表删除旧版后导入新版 zip（同 id 不会被静默覆盖）；也欢迎在商店上传分享 |
| 语音运行环境与通用模型（SenseVoice / MeloTTS / Kokoro / 唤醒词） | 「设置 → 全局助手 → 语音」一键下载（来自 sherpa-onnx 官方发布，可填 GitHub 下载加速前缀） | 随应用版本更新；删除后重新下载即可 |
| 角色语音包（如牧濑红莉栖 · Amadeus） | 官网 [资源下载](https://shidrive.nwll.top/resources) 或 Releases 的 `voice-packs` 资源 → 「语音 → 导入语音包（.zip）」 | 导入新版 zip 时选择覆盖 |

- 语音包格式：zip 内含 `model.onnx`、`tokens.json`、`voice.json`（`id` / 名称 / 语言 / 引擎 / 适用皮肤 `skins` / 角色名 / 许可证 / 来源）。切到 `skins` 中的皮肤时自动使用对应语音包。
- 资源清单：官网提供 `https://shidrive.nwll.top/api/resources.json`（皮肤 + 语音包的版本、大小、sha256、下载地址），便于校验与后续的应用内更新检查。
- 第三方模型 / 角色语音遵循各自许可证（Amadeus 语音包为 CC-BY-4.0，来源见包内 `voice.json` 与 README）。

## 参考与致谢

- [Zed](https://github.com/zed-industries/zed) —— ACP 连接层（`agent_servers`/`acp.rs`）与
  "适配器按需安装到数据目录"的托管形式参考。
- [Agent Client Protocol](https://agentclientprotocol.com) /
  [agentclientprotocol/registry](https://github.com/agentclientprotocol/registry) —— 协议规范
  与 Agent 注册表；适配器清单参考其 registry 数据。
- 适配器包：[@agentclientprotocol/codex-acp](https://www.npmjs.com/package/@agentclientprotocol/codex-acp)、
  [zcode-acp-server](https://www.npmjs.com/package/zcode-acp-server)、
  [@zed-industries/claude-code-acp](https://www.npmjs.com/package/@zed-industries/claude-code-acp)。
- [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) —— 本地语音识别 / TTS / 唤醒词；模型来自其官方发布（SenseVoice、MeloTTS、Kokoro、GigaSpeech KWS）。
- [mio/amadeus](https://huggingface.co/mio/amadeus) —— 牧濑红莉栖日语 VITS 模型（CC-BY-4.0），导出为 ONNX 用作角色语音包。
- [Tauri](https://tauri.app) / [Svelte](https://svelte.dev)。

## 状态

个人项目，活跃开发中。接口与数据结构仍可能调整，欢迎提 issue 讨论。

## License

[MIT](LICENSE)
