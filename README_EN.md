# ShiDrive

[中文](README.md) | English

> **One Context. Any Harness.**

ShiDrive (使驾, "driving seat") is a local-first AI development workbench built on **Tauri 2**. It puts projects, working contexts, AI agent sessions and workflows behind a single dashboard — not another AI coding tool, but the driver's seat you use to *drive* ACP agents (Codex / ZCode / Claude / Cline / Cursor / Qoder …). One context, any harness.

## Features

- **Projects & contexts**: project groups with multiple working contexts; each context can bind several agent sessions and switching restores them instantly.
- **AI sessions (ACP)**: create or bind history sessions, streaming replies, tool-call timeline, permission dialogs, model/mode switching, Ctrl+F in-transcript search, editable session titles; session states (active / completed / interrupted) managed in one table.
- **Agent registry**: ten built-in ACP tools — Codex, ZCode, Claude, Cline, Codebuddy, Cursor, MiniMax, OpenCode, Qoder, Grok. Enable order = chat tab order; npm adapters install with one click (HTTP proxy support, npmmirror fallback, `--omit=optional` to skip huge platform binaries).
- **Visual workflows**: drag / free-wire / double-click editing on a canvas; start, note, toast, env, command (cmd/powershell/python), agent dispatch and delay nodes; parallel branches, scheduled or manual runs, live logs, copy/paste, autosave.
- **Shared-context MCP**: a local HTTP MCP service (default `127.0.0.1:8345`) exposing project context to agents with a git-like commit model — `context_get/update/history/search` plus six workflow tools, with conflict detection and size limits.
- **Coding MCP (standalone)**: `shidrive.exe --coding-mcp` exposes file & command tools as a separate process (path-escape protection, Token/Basic auth, execution switch).
- **File tree & editor**: directory tree, system clipboard interop, open in terminal/explorer, built-in editor.
- **Desktop integration**: custom title bar and dialogs, dark/light themes; closing the window keeps ShiDrive in the tray (right-click to reopen or quit), optional launch-on-startup.
- **Zero environment setup**: Node (≥22) is auto-detected, or downloaded as a portable runtime into the user data directory with one click (never touches the system install); adapters install on demand, everything proxy-capable.

## Architecture

```
Frontend Svelte 5 (runes) + Vite
  │  invoke / event
Backend Rust (Tauri 2)
  ├── acp.rs        ACP client: stdio JSON-RPC, permission/file requests, turn transcript
  ├── agents.rs     Agent registry: discovery / adapter install / enable order
  ├── manager.rs    Session lifecycle: launch, bind, history replay, config options
  ├── mcp.rs        Shared-context MCP service (/skills, /mcp)
  ├── coding_mcp.rs Standalone Coding MCP (--coding-mcp)
  ├── engine.rs     Workflow engine: graph execution / schedules / template vars
  └── db.rs         SQLite (projects / contexts / bindings / workflows / commits)
```

## Build

```powershell
cd app
pnpm install
pnpm check            # svelte-check
pnpm tauri dev        # develop
pnpm tauri build      # NSIS installer
pnpm tauri build --no-bundle   # produce target/release/shidrive.exe only
```

Requirements: Rust 1.88+, Node ≥ 20 (build only); Windows 10/11 (WebView2). See [app/README.md](app/README.md) for details.

## Runtime & adapters (zero environment setup)

- **Node runtime**: the system Node is auto-detected (auto-adopted only when ≥22); otherwise download the
  portable node22 (~35MB) into the user data directory from Settings, or point to any path — the system
  install is never touched. For bundled distribution, place node22 next to the exe under `.tools\node22\`.
- **Adapters**: open a tool in "Settings → Agent 管理" and click "安装适配器" — it `npm install`s into
  `%APPDATA%\com.shidrive.desktop\tools\acp` (`--omit=optional` skips huge platform binaries; falls back
  to the npmmirror registry; the proxy setting applies only to ShiDrive's own downloads).
- **Tool CLIs** (codex.exe / zcode.cjs / claude …) are discovered from standard install locations and
  logged-in on demand.
- `--debug` writes a file log to `%APPDATA%\com.shidrive.desktop\logs\shidrive.log`.

## Credits & references

- [Zed](https://github.com/zed-industries/zed) — reference for the ACP connection layer
  (`agent_servers`/`acp.rs`) and the "install adapters on demand into a data directory" model.
- [Agent Client Protocol](https://agentclientprotocol.com) /
  [agentclientprotocol/registry](https://github.com/agentclientprotocol/registry) — protocol spec and
  the agent registry our adapter list is based on.
- Adapter packages: [@agentclientprotocol/codex-acp](https://www.npmjs.com/package/@agentclientprotocol/codex-acp),
  [zcode-acp-server](https://www.npmjs.com/package/zcode-acp-server),
  [@zed-industries/claude-code-acp](https://www.npmjs.com/package/@zed-industries/claude-code-acp).
- [Tauri](https://tauri.app) / [Svelte](https://svelte.dev).

## Status

A personal project in active development; interfaces and data structures may still change. Issues welcome.

## License

[MIT](LICENSE)
