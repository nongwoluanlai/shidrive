//! 聊天「执行后动作 → 弹窗提醒」：系统托盘气泡通知，点击后唤出主窗口并跳到对应会话。
//!
//! 与工作流 balloon 节点同样走 PowerShell + WinForms NotifyIcon（不引入额外依赖），
//! 区别是这里需要知道用户是否点击了气泡：脚本里跑 DoEvents 消息循环（Start-Sleep 不泵
//! 消息，点击事件永远不会触发），点击后在 stdout 输出 `clicked`。
//! 标题 / 正文经环境变量传入，免去 PowerShell 引号转义与命令行编码问题。

/// 阻塞显示气泡，返回用户是否点击（超时 / 关闭返回 false）。
#[cfg(windows)]
pub fn show_clickable(title: &str, body: &str, timeout_secs: u32) -> Result<bool, String> {
    let script = format!(
        "$ErrorActionPreference='Stop'; Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; \
         $n=New-Object System.Windows.Forms.NotifyIcon; \
         try {{ $n.Icon=[System.Drawing.Icon]::ExtractAssociatedIcon($env:SD_NOTIFY_EXE) }} catch {{ $n.Icon=[System.Drawing.SystemIcons]::Information }}; \
         $n.Text='ShiDrive'; $n.Visible=$true; $global:sdClicked=$false; $global:sdClosed=$false; \
         $n.add_BalloonTipClicked({{ $global:sdClicked=$true }}); $n.add_Click({{ $global:sdClicked=$true }}); \
         $n.add_BalloonTipClosed({{ $global:sdClosed=$true }}); \
         $n.ShowBalloonTip({ms}, $env:SD_NOTIFY_TITLE, $env:SD_NOTIFY_BODY, [System.Windows.Forms.ToolTipIcon]::Info); \
         $sw=[Diagnostics.Stopwatch]::StartNew(); \
         while (-not $global:sdClicked -and -not $global:sdClosed -and $sw.ElapsedMilliseconds -lt {limit}) {{ [System.Windows.Forms.Application]::DoEvents(); Start-Sleep -Milliseconds 80 }}; \
         $n.Visible=$false; $n.Dispose(); if ($global:sdClicked) {{ [Console]::Out.Write('clicked') }}",
        ms = timeout_secs * 1000,
        limit = (timeout_secs + 2) * 1000,
    );
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    // 气泡正文不能为空（WinForms 会抛异常）
    let body = if body.trim().is_empty() { " " } else { body };
    let mut c = std::process::Command::new("powershell");
    c.args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-Command", &script])
        .env("SD_NOTIFY_TITLE", title)
        .env("SD_NOTIFY_BODY", body)
        .env("SD_NOTIFY_EXE", exe)
        .stdin(std::process::Stdio::null());
    crate::setup::hide_console(&mut c);
    let out = c.output().map_err(|e| format!("弹窗提醒启动失败: {e}"))?;
    if !out.status.success() {
        return Err(format!("弹窗提醒失败: {}", String::from_utf8_lossy(&out.stderr).trim().chars().take(200).collect::<String>()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).contains("clicked"))
}

#[cfg(not(windows))]
pub fn show_clickable(_title: &str, _body: &str, _timeout_secs: u32) -> Result<bool, String> {
    Err("弹窗提醒仅支持 Windows".into())
}

/// 「标题:内容」拆分（中英文冒号均可）；没有冒号时整段作正文、标题用 ShiDrive。
pub fn split_title(text: &str) -> (String, String) {
    let t = text.trim();
    let pos = t.char_indices().find(|(_, c)| *c == ':' || *c == '：');
    match pos {
        Some((i, c)) => {
            let title = t[..i].trim();
            let body = t[i + c.len_utf8()..].trim();
            (if title.is_empty() { "ShiDrive".into() } else { title.into() }, body.into())
        }
        None => ("ShiDrive".into(), t.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_title_variants() {
        assert_eq!(split_title("ShiDrive:嘟嘟噜"), ("ShiDrive".into(), "嘟嘟噜".into()));
        assert_eq!(split_title("完成：回合结束: ok"), ("完成".into(), "回合结束: ok".into()));
        assert_eq!(split_title("只有正文"), ("ShiDrive".into(), "只有正文".into()));
        assert_eq!(split_title(":x"), ("ShiDrive".into(), "x".into()));
    }
}
