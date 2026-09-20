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
        if let Some(pf86) = std::env::var("ProgramFiles(x86)").ok() {
            candidates.push(PathBuf::from(pf86).join("Microsoft VS Code").join("bin").join("code.cmd"));
        }
        if let Some(p) = first_existing(&candidates) {
            return Some(p);
        }
        self.vscode_from_registry()
    }

    /// 注册表兜底：VS Code（Inno Setup）的卸载键带 InstallLocation，bin\code.cmd 在其下。
    fn vscode_from_registry(&self) -> Option<PathBuf> {
        let keys = [
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\{771FD6B0-FA20-440A-A002-3B3BAC16DC50}_is1",
            r"HKLM\Software\Microsoft\Windows\CurrentVersion\Uninstall\{EA457B21-F73E-494C-8ABF-78153535DEC2}_is1",
            r"HKLM\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\{EA457B21-F73E-494C-8ABF-78153535DEC2}_is1",
        ];
        for key in keys {
            let Ok(o) = std::process::Command::new("reg")
                .args(["query", key, "/v", "InstallLocation"])
                .no_window()
                .output()
            else {
                continue;
            };
            if !o.status.success() {
                continue;
            }
            let text = String::from_utf8_lossy(&o.stdout);
            for line in text.lines() {
                let Some(idx) = line.find("REG_SZ") else { continue };
                let dir = line[idx + 6..].trim();
                if dir.is_empty() {
                    continue;
                }
                let cand = PathBuf::from(dir).join("bin").join("code.cmd");
                if cand.exists() {
                    return Some(cand);
                }
                let cand_exe = PathBuf::from(dir).join("bin").join("code");
                if cand_exe.exists() {
                    return Some(cand_exe);
                }
            }
        }
        None
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
        // 多候选时按完整性择优：完整安装（含 @zcode 运行时包 / provider 配置）优先于残缺拷贝
        let mut all: Vec<PathBuf> = vec![];
        all.extend(self.walk_for_zcode_cli());
        all.extend(self.zcode_via_start_menu_all());
        all.retain(|p| p.exists());
        all.sort_by_key(|p| std::cmp::Reverse(self.zcode_completeness(p)));
        all.into_iter().next()
    }

    /// zcode.cjs 所在 glm 目录的完整性评分：2=含 @zcode 运行时包，1=含 provider 配置，0=只有裸文件。
    fn zcode_completeness(&self, zc: &Path) -> i32 {
        let Some(glm) = zc.parent() else { return 0 };
        let mut score = 0;
        if glm.join("node_modules").join("@zcode").exists() || glm.join("packages").join("@zcode").exists() {
            score += 2;
        }
        if glm.join("provider").join("zcode-builtin.json").exists() {
            score += 1;
        }
        score
    }

    /// 完整 ZCode 桌面安装的 glm 目录候选（标准位置 + 开始菜单快捷方式解析出的安装目录），
    /// 供适配器 provider 配置缺失时自动补全使用。仅保留真实存在内容的目录。
    pub fn zcode_desktop_glm_dirs(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = vec![];
        let mut push_glm = |install: PathBuf| {
            let glm = install.join("resources").join("glm");
            if !out.contains(&glm) {
                out.push(glm);
            }
        };
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            push_glm(PathBuf::from(&local).join("Programs").join("ZCode"));
            push_glm(PathBuf::from(&local).join("Programs").join("zcode"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            push_glm(PathBuf::from(pf).join("ZCode"));
        }
        for lnk in self.zcode_start_menu_lnks() {
            if let Some(install) = self.resolve_lnk_install(&lnk) {
                push_glm(install);
            }
        }
        out.retain(|g| g.join("provider").exists() || g.join("zcode.cjs").exists());
        out
    }

    /// 收集开始菜单里名字含 zcode 的 .lnk（两层深度内）。
    fn zcode_start_menu_lnks(&self) -> Vec<PathBuf> {
        let mut menus: Vec<PathBuf> = vec![];
        if let Ok(appdata) = std::env::var("APPDATA") {
            menus.push(PathBuf::from(appdata).join("Microsoft").join("Start Menu").join("Programs"));
        }
        if let Ok(pd) = std::env::var("ProgramData") {
            menus.push(PathBuf::from(pd).join("Microsoft").join("Windows").join("Start Menu").join("Programs"));
        }
        let mut lnks: Vec<PathBuf> = vec![];
        for dir in menus {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                let mut sub: Vec<PathBuf> = vec![];
                if p.is_dir() {
                    if let Ok(inner) = std::fs::read_dir(&p) {
                        sub.extend(inner.flatten().map(|f| f.path()));
                    }
                } else {
                    sub.push(p);
                }
                for lnk in sub {
                    let n = lnk.file_name().map(|f| f.to_string_lossy().to_lowercase()).unwrap_or_default();
                    if n.ends_with(".lnk") && n.contains("zcode") {
                        lnks.push(lnk);
                    }
                }
            }
        }
        lnks
    }

    /// 解析 .lnk → 目标 exe 路径（WScript.Shell COM，无第三方依赖）。
    fn resolve_lnk_install(&self, lnk: &Path) -> Option<PathBuf> {
        let out = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "(New-Object -ComObject WScript.Shell).CreateShortcut('{}').TargetPath",
                    lnk.to_string_lossy().replace("'", "''"),
                ),
            ])
            .no_window()
            .output()
            .ok()?;
        let target = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if target.is_empty() {
            return None;
        }
        Some(PathBuf::from(target))
    }

    /// 兜底：从开始菜单的 ZCode 快捷方式解析安装目录，再找 resources\glm\zcode.cjs。
    fn zcode_via_start_menu_all(&self) -> Vec<PathBuf> {
        let mut found = vec![];
        for lnk in self.zcode_start_menu_lnks() {
            let Some(install) = self.resolve_lnk_install(&lnk) else { continue };
            // 快捷方式指向 <install>\ZCode.exe → resources\glm 在其旁/上一级
            for base in [install.clone(), install.parent().map(|p| p.to_path_buf()).unwrap_or_default()] {
                let cand = base.join("resources").join("glm").join("zcode.cjs");
                if cand.exists() && !found.contains(&cand) {
                    found.push(cand);
                }
            }
        }
        found
    }

    /// 在 %LOCALAPPDATA%\Programs 与 Program Files 下逐应用目录探测 zcode.cjs
    ///（安装目录名与内部布局随版本变化，不做硬编码假设）。
    fn walk_for_zcode_cli_all(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = vec![];
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            roots.push(PathBuf::from(local).join("Programs"));
        }
        if let Ok(pf) = std::env::var("ProgramFiles") {
            roots.push(PathBuf::from(pf));
        }
        let mut found = vec![];
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
                    if cand.exists() && !found.contains(&cand) {
                        found.push(cand);
                    }
                }
            }
        }
        found
    }

    fn walk_for_zcode_cli(&self) -> Option<PathBuf> {
        self.walk_for_zcode_cli_all().into_iter().next()
    }
}

#[cfg(windows)]
trait CmdFlags {
    fn no_window(&mut self) -> &mut Self;
}
#[cfg(windows)]
impl CmdFlags for std::process::Command {
    fn no_window(&mut self) -> &mut Self {
        use std::os::windows::process::CommandExt;
        self.creation_flags(0x0800_0000)
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
