//! Filesystem operations for the file tree view.

use chrono::TimeZone;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct DirEntryInfo {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: String,
}

pub fn list_dir(path: &str) -> Result<Vec<DirEntryInfo>, String> {
    let mut entries = Vec::new();
    let rd = std::fs::read_dir(path).map_err(|e| format!("无法读取目录 {path}: {e}"))?;
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == ".git" {
            continue;
        }
        let meta = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = meta.is_dir();
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| {
                chrono::Local
                    .timestamp_opt(d.as_secs() as i64, 0)
                    .single()
                    .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        entries.push(DirEntryInfo {
            name,
            path: e.path().to_string_lossy().to_string(),
            is_dir,
            size: if is_dir { 0 } else { meta.len() },
            mtime,
        });
    }
    entries.sort_by(|a, b| match (b.is_dir, a.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(entries)
}

pub fn read_file(path: &str) -> Result<(String, bool), String> {
    let data = std::fs::read(path).map_err(|e| format!("无法读取文件: {e}"))?;
    if data.len() > 5 * 1024 * 1024 {
        return Err("文件超过 5MB，暂不支持预览".into());
    }
    let binary = data.contains(&0);
    if binary {
        return Ok((format!("[二进制文件，{} 字节]", data.len()), true));
    }
    Ok((String::from_utf8_lossy(&data).to_string(), false))
}

pub fn write_file(path: &str, content: &str) -> Result<(), String> {
    std::fs::write(path, content).map_err(|e| format!("写入失败: {e}"))
}

pub fn create_file(path: &str) -> Result<(), String> {
    if std::path::Path::new(path).exists() {
        return Err("文件已存在".into());
    }
    std::fs::write(path, "").map_err(|e| format!("创建失败: {e}"))
}

pub fn create_dir(path: &str) -> Result<(), String> {
    std::fs::create_dir_all(path).map_err(|e| format!("创建失败: {e}"))
}

pub fn rename_path(from: &str, to: &str) -> Result<(), String> {
    if std::path::Path::new(to).exists() {
        return Err("目标已存在".into());
    }
    std::fs::rename(from, to).map_err(|e| format!("重命名失败: {e}"))
}

/// Move to recycle bin; falls back to permanent delete when unavailable.
pub fn delete_path(path: &str) -> Result<(), String> {
    match trash::delete(path) {
        Ok(_) => Ok(()),
        Err(e) => {
            let p = std::path::Path::new(path);
            let res = if p.is_dir() { std::fs::remove_dir_all(p) } else { std::fs::remove_file(p) };
            res.map_err(|e2| format!("删除失败（回收站: {e}）: {e2}"))
        }
    }
}

pub fn open_in_explorer(path: &str) -> Result<(), String> {
    let p = std::path::Path::new(path);
    if p.is_dir() {
        open::that_detached(path).map_err(|e| e.to_string())
    } else {
        // select the file in its folder
        std::process::Command::new("explorer")
            .arg(format!("/select,{path}"))
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

pub fn open_in_terminal(path: &str) -> Result<(), String> {
    let script = format!("Set-Location -LiteralPath '{}'; Clear-Host", path.replace('\'', "''"));
    std::process::Command::new("powershell")
        .args(["-NoExit", "-Command", &script])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("打开终端失败: {e}"))
}

/// Open cmd.exe kept open at the given directory.
pub fn open_in_cmd(path: &str) -> Result<(), String> {
    std::process::Command::new("cmd")
        .args(["/K", "cd", "/d", path])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("打开 cmd 失败: {e}"))
}

/// 在 VS Code 中打开目录：.cmd/.bat shim 经 cmd /d /s /c 调起（隐藏控制台，
/// spawn 不等待，旧使驾同款引号形式）；其余可执行文件直接启动。
pub fn open_in_vscode(code_path: &str, dir: &str) -> Result<(), String> {
    let lower = code_path.to_ascii_lowercase();
    let mut c = if lower.ends_with(".cmd") || lower.ends_with(".bat") {
        let mut c = std::process::Command::new("cmd");
        c.args(["/d", "/s", "/c", &format!("\"\"{code_path}\" \"{dir}\"\"")]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000);
        }
        c
    } else {
        let mut c = std::process::Command::new(code_path);
        c.arg(dir);
        c
    };
    c.spawn()
        .map(|_| ())
        .map_err(|e| format!("启动 VS Code 失败: {e}"))
}

/// 后台执行的控制台程序不弹窗（CREATE_NO_WINDOW）。
#[cfg(windows)]
fn hide_window(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::windows::process::CommandExt;
    cmd.creation_flags(0x0800_0000)
}
#[cfg(not(windows))]
fn hide_window(cmd: &mut std::process::Command) -> &mut std::process::Command {
    cmd
}

/// Copy file(s)/dir(s) to the SYSTEM clipboard (Explorer-compatible paste).
pub fn copy_to_clipboard(paths: &[String]) -> Result<(), String> {
    let list: Vec<String> = paths
        .iter()
        .map(|p| format!("'{}'", p.replace("'", "''")))
        .collect();
    let script = format!("Set-Clipboard -LiteralPath @({})", list.join(","));
    let mut c = std::process::Command::new("powershell");
    let out = hide_window(&mut c)
        .args(["-NoProfile", "-Command", &script])
        .output()
        .map_err(|e| format!("执行失败: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("复制到剪贴板失败: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Paste file(s)/dir(s) from the SYSTEM clipboard into dest_dir (Explorer copy semantics).
pub fn paste_from_clipboard(dest_dir: &str) -> Result<(), String> {
    let script = format!(
        "$d = Get-Clipboard -Format FileDropList; if ($d) {{ Copy-Item -LiteralPath $d.FullName -Destination '{}' -Recurse -Force }} else {{ Write-Error '剪贴板中没有文件' }}",
        dest_dir.replace("'", "''")
    );
    let mut c = std::process::Command::new("powershell");
    let out = hide_window(&mut c)
        .args(["-NoProfile", "-Command", &script])
        .output()
        .map_err(|e| format!("执行失败: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("粘贴失败: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Open with the system default application (double-click behavior).
pub fn open_default(path: &str) -> Result<(), String> {
    open::that(path).map_err(|e| format!("打开失败: {e}"))
}

/// Current user's Desktop folder path.
pub fn desktop_dir() -> Result<String, String> {
    let mut c = std::process::Command::new("powershell");
    let out = hide_window(&mut c)
        .args(["-NoProfile", "-Command", "[Environment]::GetFolderPath('Desktop')"])
        .output()
        .map_err(|e| format!("执行失败: {e}"))?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        Err("无法获取桌面路径".into())
    } else {
        Ok(s)
    }
}
