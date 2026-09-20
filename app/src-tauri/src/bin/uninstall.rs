//! 使驾独立卸载工具（console 程序，不依赖 UI）。
//! 功能：结束使驾进程 → 清除开机自启 → （可选）删除用户数据 → 提示删除程序目录。
//! 用法：uninstall.exe [--yes]（--yes 跳过全部确认，静默卸载并保留用户数据）

#[cfg(windows)]
fn enable_utf8_console() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleOutputCP(wcodepageid: u32) -> i32;
    }
    unsafe {
        SetConsoleOutputCP(65001);
    }
}

use std::io::Write;

fn print_line(s: &str) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{s}");
    let _ = out.flush();
}

fn ask(prompt: &str) -> bool {
    print!("{prompt} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    if std::io::stdin().read_line(&mut buf).is_err() {
        return false;
    }
    matches!(buf.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

fn main() {
    #[cfg(windows)]
    enable_utf8_console();

    let silent = std::env::args().any(|a| a == "--yes");
    print_line("==============================");
    print_line("  使驾 ShiDrive 卸载工具");
    print_line("==============================");
    print_line("将执行：结束使驾进程、清除开机自启；（可选）删除用户数据。");
    print_line("");

    let kill = std::process::Command::new("taskkill")
        .args(["/IM", "shidrive.exe", "/F"])
        .creation_flags(0x0800_0000)
        .output();
    match kill {
        Ok(o) if o.status.success() => print_line("√ 已结束使驾进程"),
        _ => print_line("· 使驾当前未在运行"),
    }

    // 开机自启（tauri-plugin-autostart 写在 HKCU Run，值名随应用名）
    let mut removed_run = false;
    for name in ["ShiDrive", "shidrive", "使驾"] {
        let st = std::process::Command::new("reg")
            .args([
                "delete",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                name,
                "/f",
            ])
            .creation_flags(0x0800_0000)
            .output();
        if let Ok(o) = st {
            if o.status.success() {
                removed_run = true;
            }
        }
    }
    print_line(if removed_run { "√ 已清除开机自启" } else { "· 未发现开机自启项" });

    // 用户数据（数据库 / 适配器 / 日志 / node22）
    if let Ok(appdata) = std::env::var("APPDATA") {
        let data = std::path::PathBuf::from(&appdata).join("com.shidrive.desktop");
        if data.exists() {
            let wipe = silent
                || ask("是否删除用户数据（会话记录 / 共享上下文 / 已装适配器 / 日志）？");
            if wipe {
                match std::fs::remove_dir_all(&data) {
                    Ok(_) => print_line("√ 已删除用户数据目录"),
                    Err(e) => print_line(&format!("× 用户数据删除失败（可能仍被占用，请稍后手动删除）：{e}")),
                }
            } else {
                print_line(&format!("· 保留用户数据：{}", data.display()));
            }
        }
    }

    print_line("");
    print_line("完成。请手动删除使驾程序所在文件夹（含 shidrive.exe / uninstall.exe）即完成卸载。");
    if !silent {
        print_line("按回车退出…");
        let mut buf = String::new();
        let _ = std::io::stdin().read_line(&mut buf);
    }
}

#[cfg(windows)]
trait CreationFlags {
    fn creation_flags(&mut self, flags: u32) -> &mut Self;
}
#[cfg(windows)]
impl CreationFlags for std::process::Command {
    fn creation_flags(&mut self, flags: u32) -> &mut Self {
        use std::os::windows::process::CommandExt as _;
        std::os::windows::process::CommandExt::creation_flags(self, flags)
    }
}
