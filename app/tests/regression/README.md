# 会话修复与官方 ACP SDK 合入回归测试

适用基线：ShiDrive v0.3.14，`2ae51a7eafe129b9a6549286b5a95c69fe223945`。

这不是原审查中的“断言错误行为存在”的复现脚本；这里断言的是修复后的正确行为。测试不访问模型、不使用登录凭证，也不执行 Agent 生成的命令。

## 前端（21 个用例）

先安装 app 的锁定依赖，然后运行：

```sh
cd app
npx --yes pnpm@10 install --frozen-lockfile --ignore-scripts
cd tests/regression
npm ci --ignore-scripts
npm test
```

需要 Node.js 20+。`compile-frontend.mjs` 从当前源码编译 Svelte 5 组件和状态模块；默认 app 路径是当前测试目录的 `../..`，也可设置 `SHIDRIVE_APP` 指向其他完整 app 目录。请在本目录执行命令。

- 使用真实的 `state.svelte.ts`、`events.ts`、ChatView、两个请求弹窗、历史绑定/会话管理弹窗和 MessageItem。
- 编译时仅注入用于测试的函数访问器，不改业务函数体。
- Happy DOM 提供 DOM；IPC、Tauri 事件传输、确认框、Markdown 渲染器及不涉及本次逻辑的视觉子组件使用替身。
- 用例覆盖 UUID/旧快照重复 ID 修复、草稿 effect、异步新建/恢复/绑定/解绑、快照 SID/写入顺序、加载所有者、双击/错误/取消弹窗、事件 SID/回合/连接隔离、异步 finally、首条消息保留、上下文配置队列、标准工具 content 展示、图片跨上下文与迟到 FileReader。
- `default-full-access-unchanged` 专门断言原自动 `danger-full-access` / `never` 默认策略仍然生效，这是用户明确排除的修改项。
- 每个用例使用独立 Node 进程，避免模块级状态和持久化定时器污染后续用例。

单独运行一个用例：

```sh
node compile-frontend.mjs
node --conditions=browser frontend-tests.mjs session-event-routing
```

## Rust / 本地 ACP peers（43 个源码单元测试 + 21 个原专项 + 20 个 SDK 专项 + 12 个生命周期专项）

```sh
cd app/tests/regression/backend
cargo test --locked
```

需要 Rust 1.88+ 和 Python 3（本次实测 Rust 1.98.1；未用 1.88 单独验证最低版本）。peer 默认使用 `python3`（Windows 为 `python`）；可以通过 `PYTHON` 环境变量指定解释器路径。

`src/lib.rs` 直接引用当前生产源码中的 `acp.rs`、`manager.rs`、`agents.rs`、`db.rs`、`engine.rs`、`models.rs`、`schedule.rs`，不是其重写版本。只替换 Tauri GUI/事件句柄、工具安装路径发现、Node 定位、MCP 端口和 Windows job-object 挂接。

`build.rs` 从当前 `commands.rs` 提取本次变更的 4 个 IPC 函数体，编译检查它们与 manager 的调用匹配；只替换 State 类型并移除桌面宏，不代表完整 Tauri 命令注册或桌面构建验证。

生产 `acp.rs` 实际依赖并调用官方 `agent-client-protocol = 2.2.0`，使用稳定协议 v1；不是测试替身，也未启用 unstable/v2 特性。`acp_compat.rs` 由同一个生产模块引用。桌面主工程与测试的 SDK/schema 版本及 features 相同，测试 lockfile 内所有依赖版本均存在于桌面主工程 lockfile。测试 peer 另启用 Tokio 的 `io-std` 功能。

`mock_adapter.py` 是经 stdio 通信的本地 JSON-RPC peer。它拒绝同 SID 的重叠 prompt/load，模拟空历史、延迟取消确认、恢复配置、失败创建/恢复、权限与 elicitation 请求。

专项覆盖：

- 五个 npm 启动配方对照；手动命令空参数继承默认、非空参数完整覆盖；自动命令追加自定义参数。
- 同 SID 跨上下文及 UI/工作流串行；不同 SID 并行；取消后的锁等待确认；历史加载等待同 SID 的 prompt。
- 空历史不重启连接；被取消的 replay 清理；活跃绑定不能新建/解绑；失败保留旧绑定和快照。
- 会话级模型/模式缓存与 load 返回值；旧 SID 的配置请求被拒绝。
- 绑定变更/解绑与快照删除；迟到旧 SID 快照拒绝；过时后台历史刷新拒绝。
- 断开同时释放权限和输入请求；单 SID 取消不影响其他 SID；错误 buffer 所有者不能删除当前回合。
- 工作流事件与 UI 分离；真实 Engine 将成功输出写入运行日志。

SDK 专项 (`backend/tests/sdk_migration.rs`) 另覆盖：

- 标准 v1 初始化、文本/图片 prompt、关闭会话；不接受未支持的协议版本。
- 32 个乱序并发响应正确关联；超时/丢弃请求由 SDK 取消；迟到响应不污染后续请求。
- SDK 文件读写、行范围、标准 `content` 和旧 `contents`；未知方法与不完整请求返回协议错误。
- 等待权限不阻塞其他 RPC；字符串/数字 ID 区分；对端 `$/cancel_request` 只清理对应弹窗。
- 三种 elicitation 旧别名；分页/数组/蛇形字段历史列表；load → resume；空配置回包不清空菜单。
- EOF、非法 UTF-8、超过 32 MiB 的单行输入清理 driver/弹窗/等待者；最后一行无换行符仍接收最终响应与流式内容。
- 最后一个宿主引用释放、初始化中断、关闭后进程回收。`/proc` 进程回收断言仅在 Linux 启用；初始化中断用例也仅 Linux 编译。
- `backend/src/bin/sdk_peer.rs` 使用官方 SDK 的 **Agent 角色**。两项双端互通测试完成权限同意/取消、文件写读与流式回复，不依赖 Python 端手写协议的正确性。该 peer 不是已登录的真实 AI Agent。

Linux 下共 **117 项**：前端 21 + 源码单元 43 + 原专项 21 + SDK 专项 20 + 生命周期专项 12。原手写 PendingMap 的内部结构测试已移除，由实际 SDK 超时/取消/迟到响应测试替代；此前 21 项功能专项全部保留。三个后端专项组另按 4 线程重复 3 轮。

`registry-recipes.json` 是官方 ACP registry 的固定配方夹具，提交为 `9875e7702461a68b8252f929a35fb05c26ad86a2`。测试不联网重新解析 registry，因而可重现；它验证启动配置，不验证任何实际 CLI 的认证或跨版本表现。

## 边界

本地验证环境为 Linux、Node 20.20.2、Rust 1.98.1。依赖由各自 lockfile 固定。未验证 Windows WebView2、NSIS/便携包、真实登录后的 Agent、Windows 进程树回收或原生浏览器长列表性能。测试替身不应被当成这些集成环境的替代证明。

生成的 `compiled/`、`node_modules/`、`backend/target/` 已忽略，不应提交或加入补丁包。


## SDK 复核修复 R2：生命周期专项

`backend/tests/sdk_lifecycle.rs` 是正向修复断言，不是先前“断言缺陷存在”的审查探针：

- 用受控阻塞池复现排队条件，断开后保留用户新编辑，不允许旧写入补跑。
- 最后宿主引用释放、EOF、非法 UTF-8 同样阻止排队文件任务；没有新的连接引用环。
- 对端 `$/cancel_request` 取消文件读写；等待文件任务时其他 RPC 仍正常。
- 取消 SID 后新回合使用新的文件任务代次，旧任务不复活；其他 SID 不受影响。
- 每连接最多 32 个待完成文件任务、合计 64 MiB 的写入内容/路径计费；超额返回错误而非无限排队。这不是整个进程内存预算。
- 同连接文件 IO 串行执行；已进入 OS 的 IO 即使取消，仍保留执行槽直到真正退出。后者用 Linux FIFO 控制操作完成时机，仅 Linux 编译此用例。
- 真实 AgentManager 的聊天取消与工作流 future 中断都关闭该 SID 的弹窗准入；取消确认前/后迟到请求均得到取消结果，新回合正常开放。
- 三个 elicitation 别名均覆盖；没有 SID 的旧扩展明确为连接级，不猜测会话归属。
- 过期所有者不能取消新回合；失败的 begin 不能重开已取消门禁；并发准入/取消事件不会颠倒。

`acp_file_tasks.rs` 另有 5 个直接生产源码单元测试，覆盖关停、代次隔离、任务数量/字节预算及 SID 隔离。

**取消边界：** 尚未被阻塞闭包最后检查接纳的任务会跳过 IO；已接纳并进入同步文件操作的任务可能完成，不保证撤销内核调用或恢复已改文件。保持原 `std::fs::write` 路径/权限/链接语义，没有未经验证地替换成跨平台临时文件 rename。所有实际运行结果来自 Linux，不把它当成 Windows 进程/文件系统实机验收。
