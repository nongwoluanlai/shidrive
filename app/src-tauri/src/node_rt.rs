//! Node 运行时：版本探测与 node22 按需下载（仅 Windows；只动使驾自己的用户数据目录，
//! 不修改设备 PATH / 全局环境）。

use std::io::Read;
use std::path::PathBuf;

use crate::setup::Tools;

/// node22 便携版（Windows x64，含 npm）的发布下载地址。
pub const NODE22_URL: &str = "https://github.com/nongwoluanlai/shidrive/releases/download/v0.1.0/node22-win-x64.zip";

#[derive(Debug, Clone, serde::Serialize)]
pub struct NodeStatus {
    pub path: String,
    pub version: String,
    pub major: u32,
    /// major >= 22
    pub ok: bool,
    /// 自定义 / 用户数据 node22 / 随包 .tools/node22 / 系统 PATH / 未找到
    pub source: String,
}

/// 运行 `node --version`（隐藏控制台窗），解析 "v22.21.1"。
fn detect_version(node: &std::path::Path) -> Option<(String, u32)> {
    let mut c = std::process::Command::new(node);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    let out = c.arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let major = s.trim_start_matches('v').split('.').next()?.parse().ok()?;
    Some((s, major))
}

fn user_node22() -> Option<PathBuf> {
    std::env::var("APPDATA")
        .ok()
        .map(|a| PathBuf::from(a).join("com.shidrive.desktop").join("tools").join("node22").join("node.exe"))
}

/// 按优先级探测可用 Node：手动指定 → 用户数据 node22 → 随包 .tools/node22 → 系统 PATH。
/// 全部低于 22 时返回探测到的那个（ok=false），由界面提示指定路径或下载。
pub fn node_status(tools: &Tools) -> NodeStatus {
    let mut candidates: Vec<(&str, PathBuf)> = vec![];
    if let Some(o) = tools.node_override() {
        candidates.push(("自定义", o.clone()));
    }
    candidates.push(("用户数据 node22", user_node22().unwrap_or_default()));
    candidates.push(("随包 .tools/node22", tools.tools_dir.join("node22").join("node.exe")));
    candidates.push(("系统 PATH", PathBuf::from("node")));
    let mut fallback: Option<NodeStatus> = None;
    for (source, p) in candidates {
        let is_path = p.as_os_str() == "node";
        if !is_path && !p.exists() {
            continue;
        }
        if let Some((version, major)) = detect_version(&p) {
            let st = NodeStatus {
                path: if is_path { "node（PATH）".into() } else { p.to_string_lossy().to_string() },
                version,
                major,
                ok: major >= 22,
                source: source.into(),
            };
            if st.ok {
                return st;
            }
            if fallback.is_none() {
                fallback = Some(st);
            }
        }
    }
    fallback.unwrap_or(NodeStatus {
        path: String::new(),
        version: String::new(),
        major: 0,
        ok: false,
        source: "未找到".into(),
    })
}

/// 下载 node22 zip 并解压到用户数据目录 tools/node22（zip 内无顶层目录，直接落位）。
/// proxy（如 http://127.0.0.1:10809）仅作用于本次下载，不影响其它网络。
pub fn download_node22(proxy: Option<&str>) -> Result<String, String> {
    let Some(tools_root) = crate::agents::managed_acp_dir().map(|p| p.parent().map(|x| x.to_path_buf())).flatten() else {
        return Err("无法确定用户数据目录（APPDATA）".into());
    };
    let dest_dir = tools_root.join("node22");
    std::fs::create_dir_all(&dest_dir).map_err(|e| format!("创建目录失败: {e}"))?;

    // 流式下载到临时文件
    let agent = {
        let b = ureq::AgentBuilder::new();
        match proxy.map(|p| p.trim()).filter(|p| !p.is_empty()).map(ureq::Proxy::new) {
            Some(Ok(pr)) => b.proxy(pr).build(),
            Some(Err(e)) => return Err(format!("代理配置无效: {e}")),
            None => b.build(),
        }
    };
    let tmp = tools_root.join("node22.zip.part");
    let resp = agent
        .get(NODE22_URL)
        .call()
        .map_err(|e| format!("下载失败（GitHub 需要代理时请在下方填写）: {e}"))?;
    let mut reader = resp.into_reader().take(1024 * 1024 * 512); // 512MB 上限
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| format!("写入失败: {e}"))?;
        std::io::copy(&mut reader, &mut f).map_err(|e| format!("下载中断: {e}"))?;
    }

    // 解压（防 zip-slip：条目路径必须落在 dest_dir 内）
    let file = std::fs::File::open(&tmp).map_err(|e| format!("打开压缩包失败: {e}"))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("读取压缩包失败: {e}"))?;
    let dest_canon = dest_dir.canonicalize().unwrap_or_else(|_| dest_dir.clone());
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("压缩包损坏: {e}"))?;
        let Some(name) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            continue;
        };
        let out = dest_canon.join(&name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| format!("创建目录失败: {e}"))?;
        } else {
            if let Some(dir) = out.parent() {
                std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
            }
            let mut w = std::fs::File::create(&out).map_err(|e| format!("解压失败: {e}"))?;
            std::io::copy(&mut entry, &mut w).map_err(|e| format!("解压失败: {e}"))?;
        }
    }
    std::fs::remove_file(&tmp).ok();

    let exe = dest_dir.join("node.exe");
    match detect_version(&exe) {
        Some((v, _)) => Ok(format!("Node {v} 已就绪（{})", exe.display())),
        None => Err(format!("解压完成但 {} 未能运行", exe.display())),
    }
}
