//! 聊天「执行后动作 → 弹窗提醒」的文案解析；弹窗本身由 `toast.rs`（使驾自有通知窗口）显示。

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
