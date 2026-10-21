# ShiDrive

[中文](README.md) | English

> **One Context. Any Harness.**

ShiDrive (使驾, "driving seat") is a local-first AI development workbench built on **Tauri 2**. It puts projects, working contexts, AI agent sessions and workflows behind a single dashboard — not another AI coding tool, but the driver's seat you use to *drive* ACP agents (Codex / ZCode / Claude / Cline / Cursor / Qoder …). One context, any harness.

## Features

- **Projects & contexts**: project groups with multiple working contexts; each context can bind several agent sessions and switching restores them instantly.
- **AI sessions (ACP)**: create or bind history sessions, streaming replies, tool-call timeline, permission dialogs, model/mode switching, Ctrl+F in-transcript search, editable session titles; session states (active / completed / interrupted) managed in one table.
- **Built-in ShiDrive agent**: no adapter to install — fill in an OpenAI-compatible API (endpoint / API key / model) under Settings → Agents → ShiDrive. It reads/writes files, runs commands and uses the shared-context tools; sessions can be saved, listed and resumed. Modes: Ask (confirm before editing files / running commands), Auto, Read-only.
- **Agent registry**: ten built-in ACP tools — Codex, ZCode, Claude, Cline, Codebuddy, Cursor, MiniMax, OpenCode, Qoder, Grok. Enable order = chat tab order; npm adapters install with one click (HTTP proxy support, npmmirror fallback, `--omit=optional` to skip huge platform binaries).
- **Visual workflows**: drag / free-wire / double-click editing on a canvas; start, note, toast, env, command (cmd/powershell/python), agent dispatch and delay nodes; parallel branches, scheduled or manual runs, live logs, copy/paste, autosave.
- **Shared-context MCP**: a local HTTP MCP service (default `127.0.0.1:8345`) exposing project context to agents with a git-like commit model — `context_get/update/history/search` plus six workflow tools, with conflict detection and size limits.
- **Coding MCP (standalone)**: `shidrive.exe --coding-mcp` exposes file & command tools as a separate process (path-escape protection, Token/Basic auth, execution switch).
- **External coding access**: Passcode and OAuth access; desktop-wide OAuth consent reuses the current open directories and contexts across projects, with token revocation and standard MCP endpoint discovery. See [merge notes](docs/oauth-global-permissions.md).
- **File tree & editor**: directory tree, system clipboard interop, open in terminal/explorer, built-in editor.
- **Global assistant**: a long-lived companion independent of project sessions, shown as a bubble at the bottom-left (or next to the skin character). Its brain is an installed ACP agent or any OpenAI-compatible API; it keeps long-term memory of your preferences and takes its persona from the skin character. When you're not busy it occasionally checks in (late night, long sessions, mealtimes, unfinished todos — configurable interval, at most 6 times a day); a lightweight **Todos** panel lives in the title bar.
- **Voice**: double-tap Ctrl or say the wake word (default "Christina", detected offline) for multi-turn voice chat. Speech recognition via local SenseVoice (offline) or Baidu short-speech ASR; speech output via local MeloTTS / Kokoro, system voices, or **character voice packs** (e.g. Makise Kurisu · Amadeus in Japanese, with subtitles in the UI language). The voice runtime and models are downloaded on demand from Settings and never touch the system.
- **Skins**: two built-in skins (Steins;Gate, Hell's Paradise); more (including animated characters) from the skin store. Skin packages (`skin.json` + images, zip) can be dragged in and support character art, dynamic effects, timeline illustrations, opacity, and a bound character voice pack. See the [skin guide](docs/skin-guide.md).
- **Global search**: `Alt+F` in the file tree — quick-open by file name plus content search.
- **Web access**: a built-in lightweight web server (default port 5165); with your own reverse tunnel or LAN you can pick projects/contexts, chat with agents, run workflows and browse files from another device's browser (guest-code login with rate limiting).
- **Desktop integration**: custom title bar and dialogs, dark/light themes; ShiDrive's own non-focus-stealing notification popups; closing the window keeps ShiDrive in the tray (right-click to reopen or quit), optional launch-on-startup; single instance — launching it again (e.g. clicking the pinned taskbar icon) just brings the running main window to the front.
- **Zero environment setup**: Node (≥22) is auto-detected, or downloaded as a portable runtime into the user data directory with one click (never touches the system install); adapters, the voice runtime and models install on demand, everything proxy / mirror capable.

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
  ├── remote_mcp.rs External coding access (Passcode / OAuth via remote_oauth.rs)
  ├── assistant.rs  Global assistant: long-lived session, memory, proactive care
  ├── voice.rs      Voice: runtime/model downloads, ASR, TTS, wake word, voice packs
  ├── skins.rs      Skin package import & validation
  ├── search.rs     Global search (Alt+F)
  ├── web.rs        Web access (built-in web server)
  ├── toast.rs      Native notification popups
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

## Resources & updates (skins / voice)

To keep the installer small, **large resources ship neither in the installer nor in the git repo** — fetch them on demand:

| Resource | Where to get it | How to update |
| --- | --- | --- |
| App + built-in skins | Installer / portable zip on [Releases](https://github.com/nongwoluanlai/shidrive/releases) | Install the new version over the old one; user data (`%APPDATA%\com.shidrive.desktop`) is kept |
| Community skins | [Skin store](https://shidrive.nwll.top/skinstore) → import the zip in "Settings → 外观主题" (or drag it in) | Delete the old version in the skin list, then import the new zip (an installed id is never silently overwritten) |
| Voice runtime & generic models (SenseVoice / MeloTTS / Kokoro / wake word) | One click in "Settings → 全局助手 → Voice" (official sherpa-onnx releases; an optional GitHub download-accelerator prefix is supported) | Updated with app releases; delete and re-download any time |
| Character voice packs (e.g. Makise Kurisu · Amadeus) | [Resources page](https://shidrive.nwll.top/resources) or the `voice-packs` release on GitHub → "Import voice pack (.zip)" | Import the new zip and choose overwrite |

- Voice pack layout: `model.onnx`, `tokens.json` and `voice.json` (`id`, name, language, engine, target `skins`, character names, license, source). Switching to one of its `skins` uses the pack automatically.
- Manifest: `https://shidrive.nwll.top/api/resources.json` lists skins and voice packs with version, size, sha256 and download URL, for verification and future in-app update checks.
- Third-party models / character voices follow their own licenses (the Amadeus pack is CC-BY-4.0; see `voice.json` and the README inside the pack).

## Credits & references

- [Zed](https://github.com/zed-industries/zed) — reference for the ACP connection layer
  (`agent_servers`/`acp.rs`) and the "install adapters on demand into a data directory" model.
- [Agent Client Protocol](https://agentclientprotocol.com) /
  [agentclientprotocol/registry](https://github.com/agentclientprotocol/registry) — protocol spec and
  the agent registry our adapter list is based on.
- Adapter packages: [@agentclientprotocol/codex-acp](https://www.npmjs.com/package/@agentclientprotocol/codex-acp),
  [zcode-acp-server](https://www.npmjs.com/package/zcode-acp-server),
  [@zed-industries/claude-code-acp](https://www.npmjs.com/package/@zed-industries/claude-code-acp).
- [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) — local ASR / TTS / keyword spotting; models from its official releases (SenseVoice, MeloTTS, Kokoro, GigaSpeech KWS).
- [mio/amadeus](https://huggingface.co/mio/amadeus) — Makise Kurisu Japanese VITS model (CC-BY-4.0), exported to ONNX as a character voice pack.
- [Tauri](https://tauri.app) / [Svelte](https://svelte.dev).

## Status

A personal project in active development; interfaces and data structures may still change. Issues welcome.

## License

[MIT](LICENSE)
