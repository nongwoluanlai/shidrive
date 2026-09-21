# 官方 agent-client-protocol（Rust）SDK 接入评估

结论先说：**值得接入，但不建议本轮直接替换**。建议作为 v0.3.x 的重构主线，
分两步走（先类型层、后连接层）。以下是研究与评估细节。

## 现状

`app/src-tauri/src/acp.rs`（约 800 行）手写了：

- newline-delimited JSON-RPC over stdio（读循环 + 出站通道）
- 请求/响应匹配（PendingMap + oneshot）
- session/update 通知处理、历史回放捕获（capture/begin_load/finish_load）
- 权限请求、文件读写请求的路由与响应
- 连接生命周期（spawn/shutdown/drain/abort I/O）

## 官方 SDK（agent-client-protocol 2.2.0，Zed 出品）提供

- `schema`：全部协议消息类型（锁定 schema 1.9.1），类型齐全且随协议更新
- `Builder` / `Client::builder().connect_with(transport, ...)`：连接构造
- `Stdio` / `Lines` / `ByteStreams` 传输抽象（runtime-agnostic，需外部驱动 future）
- `HandleDispatchFrom` 等宏：请求/通知分发
- 会话生命周期辅助：`SessionBuilder` / `RestoreSessionBuilder`（v1 稳定）

## 接入收益

1. **协议一致性**：schema 类型代替手写 `json!`，避免字段拼写/层级错误
   （此前 session/resume 兼容、capability 解析等处均可受益）；
2. **跟随协议演进**：官方更新即获得新能力（如 v2 会话/fork）；
3. **减少自维护面**：JSON-RPC 帧处理、id 规范（number/string）交给 SDK。

## 接入成本与风险

1. **运行时模型差异**：SDK 是 runtime-agnostic，要求外部驱动连接 future；
   现有实现深度绑定 tokio（spawn 读循环 + oneshot + select 超时），
   需要把驱动 future 挂进 tauri::async_runtime 并重排生命周期；
2. **历史回放捕获**：capture/begin_load/finish_load 与更新分发交织，
   需要在 SDK 的分发回调里重新实现等价语义（含 replay 抑制窗口）；
3. **权限/文件请求**：现为手写路由 + Responder 语义，SDK 用 trait 回调，
   迁移时需映射到现有 PermissionDialog 事件；
4. **回归面大**：codex/zcode/deepseek 三类适配器行为差异（session/load vs
   resume、provider env 注入）都要在新骨架下重验。

## 建议的分步计划（v0.3.x）

1. **第一步（低风险）**：仅引入 `schema` 类型用于出站请求/入站通知的构造与
   解析（保持自研传输与分发），即时获得字段级类型安全；
2. **第二步**：用 `Stdio`/`Lines` 传输 + `Builder` 替换自研读写循环，
   保留现有 capture/权限逻辑作为分发回调；
3. **第三步**：会话生命周期迁到 `SessionBuilder`/`RestoreSessionBuilder`，
   删除手写 session/load/resume 封装。

## 本轮已采纳的相关建议

- RPC ID 不再强转 i64：pending 表键改为 `serde_json::Value`，
  兼容字符串 id 的适配器（acp.rs `parse_rpc_id`）；
- prompt 锁从 (context, agent) 粒度改为会话粒度（临时会话与绑定会话互不阻塞）；
- 连接生命周期：初始化失败即 shutdown、断开时 drain pending 并 abort I/O、
  请求 Drop 清理 pending 表（上一轮与本轮累计完成）。
