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
    /// major >= 22（通用适配器）
    pub ok: bool,
    /// DeepSeek 的 import.meta.main / 当前依赖均满足最低版本
    pub deepseek_ok: bool,
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

/// 显示真正用于启动适配器的 Node，而不是在自定义 Node 不可用时
/// 又找出另一份「看起来可用」的 Node（Tools::node_exe 仍会使用自定义路径）。
pub fn node_status(tools: &Tools) -> NodeStatus {
    let node = tools.node_exe();
    let source = if tools.node_override().is_some() {
        "自定义"
    } else if user_node22().as_ref() == Some(&node) {
        "用户数据 node22"
    } else if node == tools.tools_dir.join("node22").join("node.exe") {
        "随包 .tools/node22"
    } else {
        "系统 PATH"
    };
    match detect_version(&node) {
        Some((version, major)) => {
            let deepseek_ok = supports_deepseek_node(&version);
            NodeStatus {
                path: node.to_string_lossy().to_string(),
                version,
                major,
                ok: major >= 22,
                deepseek_ok,
                source: source.into(),
            }
        },
        None => NodeStatus {
            path: node.to_string_lossy().to_string(),
            version: String::new(),
            major: 0,
            ok: false,
            deepseek_ok: false,
            source: if node.is_file() { source } else { "未找到" }.into(),
        },
    }
}

/// dsh 使用 import.meta.main（Node 22.18 才可用），当前依赖还要求
/// >=22.19。Node 23 和过旧的 Node 24 同样不能仅凭 major>=22 放行。
fn supports_deepseek_node(version: &str) -> bool {
    let mut fields = version.trim_start_matches('v').split('.');
    let (Some(major), Some(minor)) = (fields.next().and_then(|s| s.parse::<u32>().ok()),
                                       fields.next().and_then(|s| s.parse::<u32>().ok())) else {
        return false;
    };
    (major == 22 && minor >= 19) || (major == 24 && minor >= 2) || major >= 25
}

/// 检查实际启动路径；下载 Node22 后若自定义路径仍指向旧版，明确提示切换并重启。
pub fn require_deepseek_node(tools: &Tools) -> Result<PathBuf, String> {
    let node = tools.node_exe();
    let version = detect_version(&node).map(|(v, _)| v).unwrap_or_else(|| "无法运行".into());
    if supports_deepseek_node(&version) {
        return Ok(node);
    }
    Err(format!(
        "DeepSeek 需要可用的 Node 22.19+（推荐内置 Node 22）；实际使用 {}，版本 {}。请到「设置 → 环境与路径」下载 Node22；若已下载但配置了旧版自定义 Node，请清除自定义 Node 路径、保存并重启使驾。",
        node.display(), version
    ))
}

#[cfg(test)]
mod deepseek_tests {
    use super::supports_deepseek_node;

    #[test]
    fn version_requires_real_deepseek_runtime() {
        assert!(!supports_deepseek_node("v22.17.0"));
        assert!(!supports_deepseek_node("v22.18.0"));
        assert!(supports_deepseek_node("v22.21.1"));
        assert!(!supports_deepseek_node("v23.0.0"));
        assert!(!supports_deepseek_node("v24.1.0"));
        assert!(supports_deepseek_node("v24.2.0"));
        assert!(!supports_deepseek_node("oops"));
    }
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
