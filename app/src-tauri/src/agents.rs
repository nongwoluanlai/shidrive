//! Agent 注册表：支持的 ACP 工具清单、环境探测、适配器安装与启用顺序。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

use crate::db::Db;
use crate::setup::Tools;

/// 一个 ACP 工具的静态描述。
#[derive(Debug, Clone, Serialize)]
pub struct AgentSpec {
    pub id: String,
    pub name: String,
    /// npm 包名（安装到 acp 目录后由 node 启动）；None = 二进制发行，需手动配置命令
    pub npm: Option<String>,
    /// 适配器脚本之后追加的参数
    pub args: Vec<String>,
    /// 帮助指引（设置页 ? 悬浮显示）
    pub help: String,
}

pub fn agent_specs() -> &'static [AgentSpec] {
    static SPECS: std::sync::OnceLock<Vec<AgentSpec>> = std::sync::OnceLock::new();
    SPECS.get_or_init(|| vec![
    AgentSpec {
        id: "codex".into(),
        name: "Codex".into(),
        npm: Some("@agentclientprotocol/codex-acp".into()),
        args: vec![],
        help: "自动探测：%LOCALAPPDATA%\\OpenAI\\Codex\\bin 下的 codex.exe（随 Codex 桌面版安装）。也可在配置里用环境变量 PATH 指定 codex.exe 所在目录。需先登录 Codex。".into(),
    },
    AgentSpec {
        id: "zcode".into(),
        name: "ZCode".into(),
        npm: Some("zcode-acp-server".into()),
        args: vec![],
        help: "自动探测 ZCode 桌面版自带 CLI（zcode.cjs）与 Node ≥22。自定义安装可在环境变量里配 ZCODE_BIN（zcode.cjs 路径）与 ZCODE_NODE（node.exe 路径）。需已登录 ZCode 桌面端。".into(),
    },
    AgentSpec {
        id: "claude".into(),
        name: "Claude".into(),
        npm: Some("@agentclientprotocol/claude-agent-acp".into()),
        args: vec![],
        help: "包装 Claude Code。需本机已安装 Claude Code CLI（claude 命令可用）并完成登录；未安装时 npm i -g @anthropic-ai/claude-code。".into(),
    },
    AgentSpec {
        id: "cline".into(),
        name: "Cline".into(),
        npm: Some("cline".into()),
        args: vec![],
        help: "Cline CLI（npm 包 cline）。启用后点「安装适配器」自动安装；按 Cline 官方方式登录（浏览器授权）。".into(),
    },
    AgentSpec {
        id: "codebuddy".into(),
        name: "Codebuddy".into(),
        npm: Some("@tencent-ai/codebuddy-code".into()),
        args: vec![],
        help: "腾讯 CodeBuddy Code（npm 包 @tencent-ai/codebuddy-code）。安装适配器后按提示用腾讯账号登录。".into(),
    },
    AgentSpec {
        id: "cursor".into(),
        name: "Cursor".into(),
        npm: None,
        args: vec!["acp".into()],
        help: "Cursor 官方 agent CLI（二进制发行，非 npm）。从 https://cursor.com/downloads 或 registry 的 windows-x86_64 归档下载 agent-cli-package.zip，解压后在「命令」里填 dist-package\\cursor-agent.cmd 的完整路径（参数 acp 已自动附加）。需已登录 Cursor。".into(),
    },
    AgentSpec {
        id: "minimax".into(),
        name: "MiniMax".into(),
        npm: Some("@minimax-ai/code".into()),
        args: vec![],
        help: "MiniMax Code（npm 包 @minimax-ai/code）。安装适配器后按提示登录 MiniMax 账号。".into(),
    },
    AgentSpec {
        id: "opencode".into(),
        name: "OpenCode".into(),
        npm: None,
        args: vec!["acp".into()],
        help: "OpenCode CLI（二进制发行）。从 https://github.com/anomalyco/opencode/releases 下载 windows-x86_64.zip，解压后在「命令」里填 opencode.exe 完整路径（参数 acp 已自动附加）。首次运行按提示登录。".into(),
    },
    AgentSpec {
        id: "qoder".into(),
        name: "Qoder".into(),
        npm: Some("@qoder-ai/qodercli".into()),
        args: vec![],
        help: "Qoder CLI（npm 包 @qoder-ai/qodercli）。安装适配器后按提示登录 Qoder 账号。".into(),
    },
    AgentSpec {
        id: "grok".into(),
        name: "Grok".into(),
        npm: Some("@xai-official/grok".into()),
        args: vec![],
        help: "xAI 官方 Grok CLI（npm 包 @xai-official/grok）。安装适配器后按提示登录 xAI 账号。".into(),
    },
    AgentSpec {
        id: "deepseek".into(),
        name: "DeepSeek".into(),
        npm: Some("@deepseek-ai/dsh".into()),
        args: vec!["--profile".into(), "acp".into()],
        help: "DeepSeek Harness CLI（npm 包 @deepseek-ai/dsh），以 `dsh --profile acp` 提供 ACP stdio 服务。安装适配器后需配置 DeepSeek API Key：展开本行点「填充 API Key」，或在环境变量里加 DEEPSEEK_API_KEY=<key>（也可用系统环境变量）。支持 session/list 与 session/resume（不支持 session/load，恢复不回放历史，由使驾侧补齐）。".into(),
    },
    ])
}

pub fn spec(id: &str) -> Option<&'static AgentSpec> {
    agent_specs().iter().find(|s| s.id == id)
}

/// 启用的 agent（有序）。首次读取时种子：已有绑定的库保留 codex+zcode，否则全部停用。
pub fn enabled_agents(db: &Arc<Db>) -> Vec<String> {
    if let Ok(Some(raw)) = db.get_setting("agents.enabled") {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(&raw) {
            return list.into_iter().filter(|id| spec(id).is_some()).collect();
        }
    }
    // seed：存量安装（已有绑定）保留 codex/zcode；全新安装全部停用
    let has_bindings = db
        .bindings_list_all()
        .map(|l| !l.is_empty())
        .unwrap_or(false);
    if has_bindings {
        vec!["codex".into(), "zcode".into()]
    } else {
        Vec::new()
    }
}

pub fn set_enabled_agents(db: &Arc<Db>, ids: &[String]) -> Result<(), String> {
    let cleaned: Vec<String> = ids.iter().filter(|id| spec(id).is_some()).cloned().collect();
    db.set_setting("agents.enabled", &serde_json::to_string(&cleaned).unwrap_or_default())
}

/// 托管安装根：用户数据目录 tools/acp（始终可写；自动安装的适配器落位在这里）。
/// 参考 Zed 的做法——适配器不嵌进二进制，按需下载安装到数据目录。
pub fn managed_acp_dir() -> Option<PathBuf> {
    std::env::var("APPDATA")
        .ok()
        .map(|a| PathBuf::from(a).join("com.shidrive.desktop").join("tools").join("acp"))
}

const DEEPSEEK_PKG: &str = "@deepseek-ai/dsh";
const DEEPSEEK_VERIFIED: &str = ".shidrive-verified-version";

/// dsh 的 Windows 原生依赖（Koffi）不能 --omit=optional；独立 npm 根
/// 防止 --include=optional 顺便下载共享 acp 根下 Codex 的大型平台包。
pub fn managed_deepseek_dir() -> Option<PathBuf> {
    managed_acp_dir().map(|dir| dir.with_file_name("deepseek-acp"))
}

fn deepseek_package_version(dir: &std::path::Path) -> Option<String> {
    let pkg = dir.join("node_modules").join("@deepseek-ai").join("dsh").join("package.json");
    let raw = std::fs::read_to_string(pkg).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).ok()?;
    parsed.get("version")?.as_str().map(|v| v.to_string())
}

/// 安装完成后须通过 ACP initialize + session/new + session/close，
/// 再写入标记。一个仅包含 bin.js 的残缺安装不再显示「适配器就绪」。
pub fn deepseek_install_candidate() -> Option<PathBuf> {
    let dir = managed_deepseek_dir()?;
    adapter_script_in(&dir.join("node_modules"), DEEPSEEK_PKG)
}

pub fn mark_deepseek_verified() -> Result<(), String> {
    let dir = managed_deepseek_dir().ok_or("无法确定 DeepSeek 安装目录")?;
    let version = deepseek_package_version(&dir).ok_or("DeepSeek 包缺少有效版本")?;
    std::fs::write(dir.join(DEEPSEEK_VERIFIED), version).map_err(|e| format!("记录 DeepSeek 验证结果失败: {e}"))
}

fn verified_deepseek_script_in(dir: &std::path::Path) -> Option<PathBuf> {
    let version = deepseek_package_version(dir)?;
    if std::fs::read_to_string(dir.join(DEEPSEEK_VERIFIED)).ok()?.trim() != version {
        return None;
    }
    adapter_script_in(&dir.join("node_modules"), DEEPSEEK_PKG)
}

fn verified_deepseek_script() -> Option<PathBuf> {
    verified_deepseek_script_in(&managed_deepseek_dir()?)
}

/// 适配器查找根：.tools/acp（用户自建 / 老安装）优先，其次托管安装目录。
pub fn acp_roots(tools: &Tools) -> Vec<PathBuf> {
    let mut v = vec![tools.acp_node_modules()];
    if let Some(r) = managed_acp_dir() {
        v.push(r.join("node_modules"));
    }
    v
}

/// 在单个 node_modules 根下解析适配器脚本（读 package.json 的 bin 字段）。
fn adapter_script_in(nm: &std::path::Path, npm_pkg: &str) -> Option<PathBuf> {
    let pkg_dir = nm.join(npm_pkg);
    let pj = pkg_dir.join("package.json");
    let Ok(raw) = std::fs::read_to_string(&pj) else {
        return None;
    };
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let bin = v.get("bin")?;
    let rel: String = match bin {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(m) => {
            // 取与包名同名的 bin；serde_json Map 按键排序，不能取"第一个"，
            // 按包名最后一段后缀匹配（如 @qoder-ai/qodercli → qodercli）
            let short = npm_pkg.rsplit('/').next().unwrap_or(npm_pkg);
            m.get(npm_pkg).or_else(|| m.get(short))
                .or_else(|| m.iter().find(|(k, _)| k.as_str().ends_with(short)).map(|(_, v)| v))
                .or_else(|| m.values().next())
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())?
        }
        _ => return None,
    };
    // bin 字段常写 "./bin/cline" 这类 POSIX 风格路径，统一按分隔符拆分重组，
    // 避免 "pkg\./bin/cline" 混合斜杠出现在界面与命令行里
    let script = rel
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != ".")
        .fold(pkg_dir, |acc, seg| acc.join(seg));
    if script.is_file() {
        Some(script)
    } else {
        None
    }
}

/// DeepSeek 只启动独立目录中经 ACP 建会话验证的版本，避免旧版
/// .tools/acp 中的残缺 dsh 抢占新安装。手动命令覆盖仍由 manager 单独处理。
pub fn adapter_script(tools: &Tools, npm_pkg: &str) -> Option<PathBuf> {
    if npm_pkg == DEEPSEEK_PKG {
        return verified_deepseek_script();
    }
    acp_roots(tools).iter().find_map(|root| adapter_script_in(root, npm_pkg))
}

/// 按相对路径段解析适配器内的固定文件（如 dist/index.js），返回存在的那个根上的路径。
pub fn resolve_adapter_file(tools: &Tools, rel: &[&str]) -> PathBuf {
    for root in acp_roots(tools) {
        let p = rel.iter().fold(root, |acc, s| acc.join(s));
        if p.exists() {
            return p;
        }
    }
    rel.iter().fold(tools.acp_node_modules(), |acc, s| acc.join(s))
}

/// 后台 npm 不弹控制台窗。
#[cfg(test)]
mod audit_tests {
    use super::*;
    #[test]
    fn exact_package_bin_wins_over_suffix_match() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join(format!("bin-test-{}", uuid::Uuid::new_v4()));
        let package = root.join("@scope/cli");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("package.json"), r#"{"bin":{"a-cli":"wrong.js","cli":"right.js"}}"#).unwrap();
        std::fs::write(package.join("wrong.js"), "").unwrap();
        std::fs::write(package.join("right.js"), "").unwrap();
        assert_eq!(adapter_script_in(&root, "@scope/cli"), Some(package.join("right.js")));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deepseek_is_not_ready_until_session_probe_marks_its_version() {
        let root = std::env::temp_dir().join(format!("deepseek-bin-test-{}", uuid::Uuid::new_v4()));
        let pkg = root.join("node_modules/@deepseek-ai/dsh");
        std::fs::create_dir_all(pkg.join("lib")).unwrap();
        std::fs::write(pkg.join("package.json"), r#"{"version":"0.1.5","bin":{"dsh":"lib/bin.js"}}"#).unwrap();
        std::fs::write(pkg.join("lib/bin.js"), "").unwrap();
        assert_eq!(verified_deepseek_script_in(&root), None);
        std::fs::write(root.join(DEEPSEEK_VERIFIED), "0.1.5").unwrap();
        assert_eq!(verified_deepseek_script_in(&root), Some(pkg.join("lib/bin.js")));
        std::fs::write(root.join(DEEPSEEK_VERIFIED), "0.1.4").unwrap();
        assert_eq!(verified_deepseek_script_in(&root), None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deepseek_mirror_retry_keeps_native_packages_and_scripts() {
        for mirror_retry in [false, true] {
            let args = install_flags(true, mirror_retry);
            assert!(args.contains(&"--include=optional"));
            assert!(args.contains(&"--ignore-scripts=false"));
            assert!(!args.contains(&"--omit=optional"));
            assert!(!args.contains(&"--ignore-scripts"));
        }
        assert!(install_flags(false, true).contains(&"--omit=optional"));
    }
}

/// 后台 npm 不弹控制台窗。
#[cfg(windows)]
fn hide_console(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000)
}
#[cfg(not(windows))]
fn hide_console(cmd: &mut std::process::Command) -> &mut std::process::Command {
    cmd
}

fn install_flags(deepseek: bool, ignore_scripts: bool) -> Vec<&'static str> {
    let mut args = vec!["install"];
    if deepseek {
        args.extend(["--include=optional", "--ignore-scripts=false"]);
    } else {
        args.push("--omit=optional");
        if ignore_scripts { args.push("--ignore-scripts"); }
    }
    args.extend(["--no-audit", "--no-fund"]);
    args
}

/// 其他适配器仍按旧策略 --omit=optional；DeepSeek 独立安装且包含平台原生包，
/// 因为 session/new 在 Windows 必须使用 Koffi。镜像重试不得跳过安装脚本。
/// 返回 npm 结果后，还须由调用者完成 ACP 建会话探针才能标记「已就绪」。
pub fn bootstrap_adapter(tools: &Tools, npm_pkg: &str, proxy: Option<&str>) -> Result<String, String> {
    let deepseek = npm_pkg == DEEPSEEK_PKG;
    if deepseek {
        crate::node_rt::require_deepseek_node(tools)?;
    }
    let acp_dir = if deepseek { managed_deepseek_dir() } else { managed_acp_dir() }
        .ok_or("无法确定用户数据目录（APPDATA）")?;
    std::fs::create_dir_all(&acp_dir).map_err(|e| format!("创建目录失败: {e}"))?;
    if deepseek {
        // 重装过程中即使 npm 返回 0，旧验证标记也不代表新安装可用。
        let _ = std::fs::remove_file(acp_dir.join(DEEPSEEK_VERIFIED));
    }
    let pj = acp_dir.join("package.json");
    if !pj.exists() {
        let name = if deepseek { "shidrive-deepseek-acp" } else { "shidrive-acp" };
        std::fs::write(&pj, format!("{{\"name\":\"{name}\",\"private\":true}}\n"))
            .map_err(|e| e.to_string())?;
    }
    let node = tools.node_exe();
    let npm_cli = tools.npm_cli();
    // npm 必须真实存在：node <npm-cli.js> 需要随 Node 发行的那份 node_modules/npm；
    // 否则会得到 "Cannot find module ... npm" 这类难懂报错，这里直接说清楚。
    if npm_cli.as_os_str() == "npm" || !npm_cli.exists() {
        return Err(format!(
            "未找到 npm（应随 Node 运行时发行，{} 旁缺少 node_modules/npm）。请在「设置 → 环境与路径」确认 Node，或使用随使驾发布的内置 Node。",
            node.display()
        ));
    }
    let proxy = proxy.map(|p| p.trim()).filter(|p| !p.is_empty());
    let run = |registry: Option<&str>, ignore_scripts: bool| -> std::io::Result<std::process::Output> {
        let mut c = std::process::Command::new(&node);
        let out = hide_console(&mut c)
            .arg(npm_cli.to_string_lossy().to_string())
            // 其他适配器保留原镜像回退；DeepSeek 的镜像回退不能跳脚本。
            .args(install_flags(deepseek, ignore_scripts));
        if let Some(p) = proxy {
            out.args(["--proxy", p, "--https-proxy", p]);
        }
        if let Some(r) = registry {
            out.args(["--registry", r]);
        }
        // @latest：安装/重装都取最新版（旧版适配器缺少新版 CLI 需要的
        // ZCODE_BUILTIN_PROVIDER_CONFIG_FILE 注入，会导致 provider 配置报错）
        out.arg(format!("{npm_pkg}@latest")).current_dir(&acp_dir).output()
    };
    let out = run(None, false).map_err(|e| format!("npm 启动失败: {e}"))?;
    // 镜像源重试；DeepSeek 保持原生依赖和 postinstall，否则可能成功安装却无法建会话。
    let out = if out.status.success() {
        out
    } else {
        run(Some("https://registry.npmmirror.com"), true)
            .map_err(|e| format!("npm 启动失败: {e}"))?
    };
    if out.status.success() {
        Ok(format!("已安装 {npm_pkg}"))
    } else {
        Err(format!(
            "npm install 失败: {}",
            String::from_utf8_lossy(&out.stderr).trim().chars().take(300).collect::<String>()
        ))
    }
}

/// 卸载 npm 适配器：在托管目录与 .tools/acp 中所有存在该包的位置执行
/// npm uninstall（--ignore-scripts 防卸载钩子联网）。返回可读结果。
pub fn uninstall_adapter(tools: &Tools, npm_pkg: &str, proxy: Option<&str>) -> Result<String, String> {
    let node = tools.node_exe();
    let npm_cli = tools.npm_cli();
    if npm_cli.as_os_str() == "npm" || !npm_cli.exists() {
        return Err("未找到 npm（应随 Node 运行时发行）。请先在「环境与路径」配置可用的 Node。".into());
    }
    let mut targets: Vec<PathBuf> = vec![tools.acp_dir()];
    if let Some(m) = managed_acp_dir() {
        targets.push(m);
    }
    if npm_pkg == DEEPSEEK_PKG {
        if let Some(dir) = managed_deepseek_dir() {
            let _ = std::fs::remove_file(dir.join(DEEPSEEK_VERIFIED));
            targets.push(dir);
        }
    }
    let mut removed = 0usize;
    let mut last_err = String::new();
    for dir in targets {
        let nm = dir.join("node_modules");
        let pkg_dir = npm_pkg
            .split('/')
            .fold(nm.clone(), |acc, seg| acc.join(seg));
        if !pkg_dir.exists() {
            continue;
        }
        let pj = dir.join("package.json");
        if !pj.exists() {
            std::fs::write(&pj, "{\"name\":\"shidrive-acp\",\"private\":true}\n").ok();
        }
        let mut c = std::process::Command::new(&node);
        let cmd = hide_console(&mut c)
            .arg(npm_cli.to_string_lossy().to_string())
            .args(["uninstall", "--ignore-scripts", "--no-audit", "--no-fund"]);
        if let Some(p) = proxy.map(|p| p.trim()).filter(|p| !p.is_empty()) {
            cmd.args(["--proxy", p, "--https-proxy", p]);
        }
        let out = cmd
            .arg(npm_pkg)
            .current_dir(&dir)
            .output()
            .map_err(|e| format!("npm 启动失败: {e}"))?;
        // 双重保险：npm 失败时直接移除包目录（幂等）
        if out.status.success() || !pkg_dir.exists() {
            if pkg_dir.exists() {
                std::fs::remove_dir_all(&pkg_dir).ok();
            }
            removed += 1;
        } else {
            last_err = String::from_utf8_lossy(&out.stderr).trim().chars().take(200).collect();
        }
    }
    if removed > 0 {
        Ok(format!("已卸载 {npm_pkg}（{removed} 处）"))
    } else if last_err.is_empty() {
        Err(format!("{npm_pkg} 未安装"))
    } else {
        Err(format!("卸载失败: {last_err}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEnvStatus {
    pub id: String,
    pub name: String,
    pub npm: Option<String>,
    pub help: String,
    pub enabled: bool,
    /// 适配器脚本是否就绪（npm 已装 / 二进制命令可解析）
    pub adapter_ready: bool,
    pub adapter_path: String,
    /// 手动覆盖的命令（settings agent.<id>）
    pub manual_command: String,
    pub node_ready: bool,
    pub node_path: String,
    /// 启动时自动注入的环境变量（只读展示：ZCODE_BIN / ZCODE_NODE / CODEX_PATH 等）
    pub auto_env: std::collections::BTreeMap<String, String>,
}

/// 汇总所有 agent 的环境状态（设置页显示用）。
pub fn registry_status(db: &Arc<Db>, tools: &Tools) -> Vec<AgentEnvStatus> {
    let enabled = enabled_agents(db);
    let node = tools.node_exe();
    let node_ready = node.is_file();
    agent_specs()
        .iter()
        .map(|sp| {
            let (adapter_ready, adapter_path) = if let Some(pkg) = &sp.npm {
                match adapter_script(tools, pkg) {
                    Some(p) => (true, p.to_string_lossy().to_string()),
                    None => (false, String::new()),
                }
            } else {
                (false, String::new())
            };
            let manual = db
                .get_setting(&format!("agent.{}", sp.id))
                .ok()
                .flatten()
                .and_then(|raw| serde_json::from_str::<crate::models::AgentLaunch>(&raw).ok())
                .map(|l| l.command)
                .unwrap_or_default();
            let mut auto_env = std::collections::BTreeMap::new();
            match sp.id.as_str() {
                "codex" => {
                    if let Some(c) = tools.codex_exe() {
                        auto_env.insert("CODEX_PATH".to_string(), c.to_string_lossy().to_string());
                    }
                }
                "zcode" => {
                    if let Some(z) = tools.zcode_cli() {
                        auto_env.insert("ZCODE_BIN".to_string(), z.to_string_lossy().to_string());
                        if let Some(provider) = tools.zcode_provider_config(&z) {
                            auto_env.insert("ZCODE_BUILTIN_PROVIDER_CONFIG_FILE".into(), provider.to_string_lossy().to_string());
                        }
                    }
                    let node = tools.node_exe();
                    if node.exists() {
                        auto_env.insert("ZCODE_NODE".to_string(), node.to_string_lossy().to_string());
                    }
                }
                "deepseek" => {
                    // 密钥只探测"是否已设置"，值为常量占位：绝不回显真实 Key，
                    // 也绝不预填进可保存的草稿（保存会把明文落盘）
                    if std::env::var("DEEPSEEK_API_KEY").map(|v| !v.trim().is_empty()).unwrap_or(false) {
                        auto_env.insert("DEEPSEEK_API_KEY".into(), "set".into());
                    }
                    if let Ok(home) = std::env::var("DSH_HOME") {
                        if !home.trim().is_empty() {
                            auto_env.insert("DSH_HOME".into(), home);
                        }
                    }
                }
                _ => {}
            }
            AgentEnvStatus {
                id: sp.id.clone(),
                name: sp.name.clone(),
                npm: sp.npm.clone(),
                help: sp.help.clone(),
                enabled: enabled.iter().any(|e| e == &sp.id),
                adapter_ready,
                adapter_path,
                manual_command: manual,
                node_ready,
                node_path: node.to_string_lossy().to_string(),
                auto_env,
            }
        })
        .collect()
}
