//! Discovery of external tools (node, codex CLI, zcode CLI, adapters, python).
//! No machine-specific paths are hardcoded: everything comes from settings
//! overrides, environment layout candidates, or PATH lookups.

use std::path::{Path, PathBuf};

use crate::db::Db;

#[derive(Clone)]
pub struct Tools {
    pub tools_dir: PathBuf,
    node_override: Option<PathBuf>,
    zcode_override: Option<PathBuf>,
    python_override: Option<PathBuf>,
    vscode_override: Option<PathBuf>,
}

fn first_existing(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|p| p.exists()).cloned()
}

fn setting(db: &Db, key: &str) -> Option<String> {
    db.get_setting(key).ok().flatten().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl Tools {
    /// Locate the `.tools` directory: explicit setting wins, then a folder named
    /// `.tools` containing `node22/node.exe` found by walking up from the exe,
    /// then a per-user copy under %APPDATA%\com.shidrive.desktop	ools.
    pub fn discover(db: &Db) -> Tools {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(|p| p.to_path_buf())).unwrap_or_default();
        let mut candidates = vec![];
        let mut dir = Some(exe_dir.as_path());
        for _ in 0..6 {
            if let Some(d) = dir {
                candidates.push(d.join(".tools"));
                dir = d.parent();
            } else {
                break;
            }
        }
        // 绿色部署回退：用户数据目录下的 tools（bootstrap 安装位置）
        if let Ok(appdata) = std::env::var("APPDATA") {
            candidates.push(PathBuf::from(appdata).join("com.shidrive.desktop").join("tools"));
        }
        let mut tools_dir = first_existing(&candidates).unwrap_or_else(|| exe_dir.join(".tools"));
        if let Some(override_dir) = setting(db, "tools.dir") {
            let p = PathBuf::from(&override_dir);
            if p.exists() {
                tools_dir = p;
            }
        }
        Tools {
            tools_dir,
            node_override: setting(db, "tools.node").map(PathBuf::from).filter(|p| p.exists()),
            zcode_override: setting(db, "tools.zcode_cli").map(PathBuf::from).filter(|p| p.exists()),
            python_override: setting(db, "tools.python").map(PathBuf::from).filter(|p| p.exists()),
            vscode_override: setting(db, "tools.vscode").map(PathBuf::from).filter(|p| p.exists()),
        }
    }

    pub fn node_override(&self) -> Option<&PathBuf> {
        self.node_override.as_ref()
    }

    pub fn node_exe(&self) -> PathBuf {
        if let Some(n) = &self.node_override {
            return n.clone();
        }
        // 用户数据目录的 node22（「下载」按钮落位处）
        if let Ok(appdata) = std::env::var("APPDATA") {
            let p = PathBuf::from(appdata).join("com.shidrive.desktop").join("tools").join("node22").join("node.exe");
            if p.exists() {
                return p;
            }
        }
        let portable = self.tools_dir.join("node22").join("node.exe");
        if portable.exists() {
            portable
        } else {
            PathBuf::from("node")
        }
    }

    pub fn acp_dir(&self) -> PathBuf {
        self.tools_dir.join("acp")
    }

    pub fn acp_node_modules(&self) -> PathBuf {
        self.acp_dir().join("node_modules")
    }

    /// npm-cli.js 相对 node.exe 的路径（随 Node 发行）。
    pub fn npm_cli(&self) -> PathBuf {
        let node = self.node_exe();
        if let Some(dir) = node.parent() {
            let npm = dir.join("node_modules").join("npm").join("bin").join("npm-cli.js");
            if npm.exists() {
                return npm;
            }
            // npm.cmd 同目录（PATH 安装）
            let npm_cmd = dir.join("npm.cmd");
            if npm_cmd.exists() {
                return npm_cmd;
            }
        }
        PathBuf::from("npm")
    }

    pub fn python_exe(&self) -> PathBuf {
        if let Some(p) = &self.python_override {
            return p.clone();
        }
        which("python.exe").or_else(|| which("python3.exe")).unwrap_or_else(|| PathBuf::from("python"))
    }

    /// VS Code 的 code.cmd / code：设置覆盖 → PATH → 标准安装位置。
    pub fn vscode_exe(&self) -> Option<PathBuf> {
        if let Some(v) = &self.vscode_override {
            return Some(v.clone());
        }
        if let Some(p) = which("code.cmd").or_else(|| which("code")) {
            return Some(p);
        }
        let mut candidates = vec![];
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(PathBuf::from(&local).join("Programs").join("Microsoft VS Code").join("bin").join("code.cmd"));
        }
        if let Some(pf) = std::env::var("ProgramFiles").ok() {
            candidates.push(PathBuf::from(pf).join("Microsoft VS Code").join("bin").join("code.cmd"));
        }
        first_existing(&candidates)
    }

    pub fn codex_adapter(&self) -> PathBuf {
        crate::agents::resolve_adapter_file(self, &["@agentclientprotocol", "codex-acp", "dist", "index.js"])
    }

    pub fn zcode_adapter(&self) -> PathBuf {
        crate::agents::resolve_adapter_file(self, &["zcode-acp-server", "dist", "cli.js"])
    }

    /// Newest codex.exe under %LOCALAPPDATA%\OpenAI\Codex\bin\<hash>\codex.exe
    pub fn codex_exe(&self) -> Option<PathBuf> {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            let base = Path::new(&local).join("OpenAI").join("Codex").join("bin");
            if let Ok(entries) = std::fs::read_dir(&base) {
                let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
                for e in entries.flatten() {
                    let candidate = e.path().join("codex.exe");
                    if candidate.exists() {
                        let mtime = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                        if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                            best = Some((mtime, candidate));
                        }
                    }
                }
                if let Some((_, p)) = best {
                    return Some(p);
                }
            }
        }
        which("codex.exe")
    }

    /// zcode.cjs shipped inside the ZCode desktop app. Discovery: settings
    /// override → standard locations → a bounded walk over Programs dirs
    /// (install folder names and layouts vary across versions).
    pub fn zcode_cli(&self) -> Option<PathBuf> {
        if let Some(z) = &self.zcode_override {
            return Some(z.clone());
        }
        let mut candidates = vec![];
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(PathBuf::from(&local).join("Programs").join("ZCode").join("resources").join("glm").join("zcode.cjs"));
            candidates.push(PathBuf::from(&local).join("Programs").join("zcode").join("resources").join("glm").join("zcode.cjs"));
        }
        if let Some(pf) = std::env::var("ProgramFiles").ok() {
            candidates.push(PathBuf::from(pf).join("ZCode").join("resources").join("glm").join("zcode.cjs"));
        }
        if let Some(p) = first_existing(&candidates) {
            return Some(p);
        }
        self.walk_for_zcode_cli()
    }

    /// 在 %LOCALAPPDATA%\Programs 与 Program Files 下逐应用目录探测 zcode.cjs
    ///（安装目录名与内部布局随版本变化，不做硬编码假设）。
    fn walk_for_zcode_cli(&self) -> Option<PathBuf> {
        let mut roots: Vec<PathBuf> = vec![];
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            roots.push(PathBuf::from(local).join("Programs"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            roots.push(PathBuf::from(pf));
        }
        for root in roots {
            let Ok(apps) = std::fs::read_dir(&root) else { continue };
            for app in apps.flatten() {
                let res = app.path().join("resources");
                if !res.is_dir() {
                    continue;
                }
                let rels = [
                    std::path::Path::new("glm").join("zcode.cjs"),
                    std::path::Path::new("app").join("glm").join("zcode.cjs"),
                    std::path::PathBuf::from("zcode.cjs"),
                ];
                for rel in rels {
                    let cand = res.join(&rel);
                    if cand.exists() {
                        return Some(cand);
                    }
                }
            }
        }
        None
    }
}

pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(';') {
        let p = Path::new(dir).join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}
