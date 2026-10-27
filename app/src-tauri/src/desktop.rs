//! 电脑操作工具：全局助手（API 模式）与内置 ShiDrive Agent 共用。
//!
//! 终端命令、查找 / 打开程序、窗口列表与切换、截图、读图、鼠标、键盘。
//! Windows 直接调 user32 / gdi32 / shell32（不引入新的系统依赖）；其他平台只支持 run_command。
//! 截图缩放后以 PNG data URL 交给调用方，由调用方按 OpenAI `image_url` 格式附给模型；
//! 鼠标坐标默认按「最近一张截图」的像素坐标换算回屏幕坐标。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

/// 工具执行结果：text 给模型，image = data URL（截图 / 读图）
pub struct Output {
    pub text: String,
    pub image: Option<String>,
}

impl Output {
    fn text(t: impl Into<String>) -> Self { Output { text: t.into(), image: None } }
}

/// 调用方的运行选项
pub struct Opts {
    /// run_command 默认工作目录
    pub workdir: PathBuf,
    /// 宿主（使驾主程序）进程号：截图时临时隐藏它的窗口（主窗口 / 桌宠），免得挡住画面；0 = 不处理
    pub host_pid: u32,
    pub hide: bool,
    /// 模型能看图（否则截图 / 读图直接报错，不浪费一轮）
    pub vision: bool,
}

const READ: &[&str] = &["desktop_screenshot", "desktop_read_image", "desktop_windows", "desktop_find_apps"];
const ACT: &[&str] = &["desktop_open", "desktop_focus_window", "desktop_mouse", "desktop_keyboard"];

pub fn is_desktop(name: &str) -> bool { name == "run_command" || READ.contains(&name) || ACT.contains(&name) }
/// 只读（不改变电脑状态）：不需要确认
pub fn is_read(name: &str) -> bool { READ.contains(&name) }
/// 需要用户确认的操作（切换窗口不算）
pub fn needs_confirm(name: &str) -> bool { is_desktop(name) && !is_read(name) && name != "desktop_focus_window" }

/// ACP ToolKind（权限弹窗图标 / 「始终允许」按类别记）
pub fn kind(name: &str) -> &'static str {
    match name {
        "desktop_screenshot" | "desktop_read_image" | "desktop_windows" => "read",
        "desktop_find_apps" => "search",
        "run_command" | "desktop_open" => "execute",
        _ => "other",
    }
}

fn s<'a>(a: &'a Value, k: &str) -> &'a str { a.get(k).and_then(Value::as_str).unwrap_or_default() }
fn cap(t: &str, n: usize) -> String {
    if t.chars().count() <= n { t.to_string() } else { format!("{}…", t.chars().take(n).collect::<String>()) }
}

pub fn title(name: &str, a: &Value) -> String {
    match name {
        "run_command" => format!("运行命令 {}", cap(s(a, "command"), 80)),
        "desktop_open" => format!("打开 {}", cap(s(a, "target"), 80)),
        "desktop_focus_window" => format!("切换到窗口「{}」", cap(s(a, "window"), 40)),
        "desktop_screenshot" => if s(a, "window").is_empty() { "截图".into() } else { format!("截图窗口「{}」", cap(s(a, "window"), 40)) },
        "desktop_read_image" => format!("查看图片 {}", cap(s(a, "path"), 80)),
        "desktop_windows" => "查看窗口列表".into(),
        "desktop_find_apps" => format!("查找程序「{}」", cap(s(a, "query"), 40)),
        "desktop_mouse" => {
            let act = a.get("action").and_then(Value::as_str).unwrap_or("click");
            match (a.get("x").and_then(Value::as_f64), a.get("y").and_then(Value::as_f64)) {
                (Some(x), Some(y)) => format!("鼠标 {act} ({}, {})", x.round(), y.round()),
                _ => format!("鼠标 {act}"),
            }
        }
        "desktop_keyboard" => {
            if !s(a, "text").is_empty() { format!("输入文字「{}」", cap(s(a, "text"), 40)) } else { format!("按键 {}", s(a, "keys")) }
        }
        other => other.to_string(),
    }
}

fn spec(name: &str, desc: &str, required: &[&str], props: Value) -> Value {
    json!({ "type": "function", "function": { "name": name, "description": desc,
        "parameters": { "type": "object", "required": required, "properties": props } } })
}

/// OpenAI function calling 格式的工具定义。`run` = 带上 run_command（内置 Agent 已有自己的）；`act` = 带上会改变电脑状态的工具
pub fn tool_specs(run: bool, act: bool, vision: bool) -> Vec<Value> {
    let mut v = Vec::new();
    if run {
        v.push(spec("run_command", if cfg!(windows) {
            "在用户电脑上执行终端命令（cmd /C，非交互，返回退出码与输出）。PowerShell 用 powershell -NoProfile -Command \"…\"；多行脚本用 stdin 传给解释器（如 command=python, stdin=脚本）。不要用它启动带界面的程序（用 desktop_open）。"
        } else { "在用户电脑上执行终端命令（sh -c，非交互，返回退出码与输出）。" }, &["command"], json!({
            "command": { "type": "string" },
            "workdir": { "type": "string", "description": "工作目录（默认用户主目录）" },
            "stdin": { "type": "string", "description": "写入标准输入的内容" },
            "timeout": { "type": "integer", "description": "秒，默认 60，最大 600" } })));
    }
    v.push(spec("desktop_windows", "列出当前打开的窗口（标题 / 进程名 / 位置 / 是否最小化）。", &[], json!({
        "query": { "type": "string", "description": "按标题或进程名过滤（可选）" } })));
    v.push(spec("desktop_find_apps", "在开始菜单和桌面快捷方式里查找已安装的程序，返回名称与路径（交给 desktop_open 打开）。", &["query"], json!({
        "query": { "type": "string", "description": "程序名关键字，如 网易云 / chrome / 微信；留空列出全部" } })));
    if vision {
        v.push(spec("desktop_screenshot", "截取屏幕（或某个窗口）给你看。返回文本里带图片尺寸：之后调用 desktop_mouse 时必须把 image_width / image_height 设为那张图的尺寸，坐标才能对准。刚打开程序时用 delay_ms 等它加载。", &[], json!({
            "window": { "type": "string", "description": "只截这个窗口（标题或进程名关键字，会先切到前台）；默认截主屏幕" },
            "screen": { "type": "string", "enum": ["primary", "all"], "description": "primary = 主屏幕（默认），all = 所有显示器" },
            "delay_ms": { "type": "integer", "description": "截图前等待毫秒数（0~10000）" },
            "max_width": { "type": "integer", "description": "图片最大宽度（480~1920，默认 1280）" } })));
        v.push(spec("desktop_read_image", "查看电脑上的图片文件（png / jpg / gif / webp / bmp）。", &["path"], json!({
            "path": { "type": "string", "description": "图片的绝对路径" } })));
    }
    if act {
        v.push(spec("desktop_open", "打开程序、文件、文件夹或网址（相当于双击 / 开始菜单启动）。target 可以是 desktop_find_apps 找到的快捷方式路径、exe 路径、文件路径或 https:// 网址。", &["target"], json!({
            "target": { "type": "string" },
            "args": { "type": "string", "description": "命令行参数（可选）" } })));
        v.push(spec("desktop_focus_window", "把某个窗口切到前台（最小化的会还原），之后的键盘输入会发给它。", &["window"], json!({
            "window": { "type": "string", "description": "窗口标题或进程名关键字，如 网易云音乐 / cloudmusic" } })));
        v.push(spec("desktop_mouse", "鼠标操作。x / y 是你在截图里看到的像素坐标；必须同时传 image_width / image_height（那张截图的像素尺寸，截图返回文本里有），工具按比例换算成真实屏幕坐标——不传或传错会点错位置。没截过图时才直接用屏幕坐标（此时传屏幕分辨率）。操作后建议再截图确认结果。", &["action", "image_width", "image_height"], json!({
            "action": { "type": "string", "enum": ["click", "double_click", "right_click", "middle_click", "move", "scroll", "drag"] },
            "image_width": { "type": "integer", "description": "x / y 所在截图的像素宽度（截图返回文本里有）" },
            "image_height": { "type": "integer", "description": "x / y 所在截图的像素高度" },
            "x": { "type": "number" }, "y": { "type": "number" },
            "to_x": { "type": "number", "description": "drag 的终点" }, "to_y": { "type": "number" },
            "amount": { "type": "integer", "description": "scroll 的格数：正数向上，负数向下（默认 -3）" } })));
        v.push(spec("desktop_keyboard", "键盘输入：text = 输入一段文字（支持中文）；keys = 按组合键，如 enter / ctrl+v / alt+f4 / win+d / space / media_play_pause / media_next / volume_up。二者可同时给（先输入文字再按键）。", &[], json!({
            "text": { "type": "string" },
            "keys": { "type": "string" },
            "repeat": { "type": "integer", "description": "keys 重复次数（默认 1，最多 20）" },
            "window": { "type": "string", "description": "先切到这个窗口再输入（可选）" } })));
    }
    v
}

/// 执行一个工具
pub fn call(name: &str, a: &Value, o: &Opts) -> Result<Output, String> {
    match name {
        "run_command" => run_command(a, o),
        "desktop_find_apps" => find_apps(s(a, "query")),
        "desktop_read_image" => {
            if !o.vision { return Err("当前模型没有开启读图（OpenAI 图片输入），无法查看图片".into()); }
            read_image(Path::new(s(a, "path")))
        }
        _ => {
            if name == "desktop_screenshot" && !o.vision { return Err("当前模型没有开启读图（OpenAI 图片输入），无法查看截图".into()); }
            if !cfg!(windows) { return Err("这个操作目前只支持 Windows".into()); }
            // 只在截图时临时隐藏使驾自己的窗口（主窗口 / 桌宠人物），截完立即恢复
            if name == "desktop_screenshot" && o.hide && o.host_pid != 0 {
                let hidden = win::hide_process_windows(o.host_pid);
                if !hidden.is_empty() { std::thread::sleep(Duration::from_millis(250)); }
                let r = win::call(name, a);
                win::show_windows(&hidden);
                return r;
            }
            // 键鼠 / 打开 / 切窗口：使驾主窗口正在前台时先最小化，免得点到、输入到使驾自己；
            // 鼠标落点在桌宠等使驾小窗上时，临时隐藏那个小窗
            if o.hide && o.host_pid != 0 && name != "desktop_windows" {
                if win::minimize_if_foreground(o.host_pid) > 0 { std::thread::sleep(Duration::from_millis(400)); }
            }
            let covered = if name == "desktop_mouse" && o.hide && o.host_pid != 0 {
                match (a.get("x").and_then(Value::as_f64), a.get("y").and_then(Value::as_f64)) {
                    (Some(x), Some(y)) => { let (sx, sy) = to_screen(x, y); win::hide_own_at(o.host_pid, sx, sy) }
                    _ => Vec::new(),
                }
            } else { Vec::new() };
            let r = win::call(name, a);
            win::show_windows(&covered);
            r
        }
    }
}

fn run_command(a: &Value, o: &Opts) -> Result<Output, String> {
    let cmd = s(a, "command").trim();
    if cmd.is_empty() { return Err("缺少 command".into()); }
    let dir = match s(a, "workdir").trim() {
        "" => o.workdir.clone(),
        w => PathBuf::from(w),
    };
    if !dir.is_dir() { return Err(format!("工作目录不存在：{}", dir.display())); }
    let timeout = a.get("timeout").and_then(Value::as_u64).unwrap_or(60).clamp(1, 600);
    let stdin = a.get("stdin").and_then(Value::as_str);
    // 中文系统的 cmd 默认 GBK 输出，切到 UTF-8 免得乱码
    let full = if cfg!(windows) { format!("chcp 65001>nul & {cmd}") } else { cmd.to_string() };
    let r = crate::coding_mcp::run_exec(&full, &dir, timeout, 12_000, stdin)?;
    let mut t = format!("exit_code: {}\n", r["exit_code"]);
    for (k, label) in [("stdout", "stdout"), ("stderr", "stderr")] {
        let v = r[k].as_str().unwrap_or_default();
        if !v.trim().is_empty() { t.push_str(&format!("--- {label} ---\n{v}\n")); }
    }
    Ok(Output::text(t))
}

/// 用户主目录（run_command 默认工作目录）
pub fn home_dir() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(std::env::temp_dir)
}

// ---------------------------------------------------------------- 查找程序

fn app_dirs() -> Vec<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let mut v = Vec::new();
    if let Some(p) = env("ProgramData") { v.push(p.join("Microsoft\\Windows\\Start Menu\\Programs")); }
    if let Some(p) = env("APPDATA") { v.push(p.join("Microsoft\\Windows\\Start Menu\\Programs")); }
    if let Some(p) = env("USERPROFILE") { v.push(p.join("Desktop")); v.push(p.join("OneDrive\\Desktop")); }
    if let Some(p) = env("PUBLIC") { v.push(p.join("Desktop")); }
    v.into_iter().filter(|p| p.is_dir()).collect()
}

fn walk_apps(dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    if depth > 4 || out.len() > 2000 { return; }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() { walk_apps(&p, depth + 1, out); continue; }
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or_default().to_ascii_lowercase();
        if !matches!(ext.as_str(), "lnk" | "url" | "exe" | "appref-ms") { continue; }
        let name = p.file_stem().and_then(|x| x.to_str()).unwrap_or_default().to_string();
        let low = name.to_lowercase();
        if low.contains("uninstall") || name.contains("卸载") { continue; }
        out.push((name, p));
    }
}

fn find_apps(query: &str) -> Result<Output, String> {
    let mut all = Vec::new();
    for d in app_dirs() { walk_apps(&d, 0, &mut all); }
    let q = query.trim().to_lowercase();
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut hits: Vec<&(String, PathBuf)> = all.iter()
        .filter(|(n, p)| words.iter().all(|w| n.to_lowercase().contains(w) || p.to_string_lossy().to_lowercase().contains(w)))
        .collect();
    hits.sort_by_key(|(n, _)| (n.chars().count(), n.clone()));
    hits.dedup_by(|a, b| a.0 == b.0);
    if hits.is_empty() {
        return Ok(Output::text(format!("没找到名字含「{query}」的程序快捷方式。可以换个关键字，或用 run_command 搜索（如 where 程序名 / dir 安装目录）。")));
    }
    let limit = if q.is_empty() { 120 } else { 20 };
    let mut t = format!("找到 {} 个：\n", hits.len());
    for (n, p) in hits.iter().take(limit) { t.push_str(&format!("- {n}：{}\n", p.display())); }
    Ok(Output::text(t))
}

// ---------------------------------------------------------------- 读图

fn mime_of(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) { Some("image/png") }
    else if b.starts_with(&[0xFF, 0xD8, 0xFF]) { Some("image/jpeg") }
    else if b.starts_with(b"GIF8") { Some("image/gif") }
    else if b.len() > 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" { Some("image/webp") }
    else if b.starts_with(b"BM") { Some("image/bmp") }
    else { None }
}

fn read_image(path: &Path) -> Result<Output, String> {
    if path.as_os_str().is_empty() { return Err("缺少 path".into()); }
    let meta = std::fs::metadata(path).map_err(|e| format!("读取失败：{e}"))?;
    if meta.len() > 20 * 1024 * 1024 { return Err("图片超过 20MB".into()); }
    let b = std::fs::read(path).map_err(|e| format!("读取失败：{e}"))?;
    let mime = mime_of(&b).ok_or("不是支持的图片格式（png / jpg / gif / webp / bmp）")?;
    // 大 PNG / BMP 缩小后再发（其余格式原样）
    if matches!(mime, "image/png" | "image/bmp") && b.len() > 1_500_000 {
        if let Ok(img) = image_load(&b, mime) {
            let (png, w, h) = encode_png(img.to_rgb8(), 1600)?;
            return Ok(Output { text: format!("图片 {}（已缩小到 {w}x{h}）", path.display()), image: Some(data_url("image/png", &png)) });
        }
    }
    if b.len() > 8 * 1024 * 1024 { return Err("图片超过 8MB，请先缩小".into()); }
    Ok(Output { text: format!("图片 {}（{} KB）", path.display(), b.len() / 1024), image: Some(data_url(mime, &b)) })
}

fn image_load(b: &[u8], mime: &str) -> Result<image::DynamicImage, String> {
    let fmt = if mime == "image/bmp" { image::ImageFormat::Bmp } else { image::ImageFormat::Png };
    image::load_from_memory_with_format(b, fmt).map_err(|e| e.to_string())
}

fn data_url(mime: &str, b: &[u8]) -> String {
    format!("data:{mime};base64,{}", b64(b))
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// 缩到最大宽 / 高 `max` 以内并编码 PNG；返回 (png, 宽, 高)
fn encode_png(img: image::RgbImage, max: u32) -> Result<(Vec<u8>, u32, u32), String> {
    use image::ImageEncoder;
    let (w, h) = img.dimensions();
    let k = (max as f64 / w.max(h) as f64).min(1.0);
    let img = if k < 1.0 {
        let (nw, nh) = (((w as f64) * k).round().max(1.0) as u32, ((h as f64) * k).round().max(1.0) as u32);
        image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle)
    } else { img };
    let (w, h) = img.dimensions();
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(&mut out, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Adaptive)
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("图片编码失败：{e}"))?;
    Ok((out, w, h))
}

// ---------------------------------------------------------------- 宿主窗口

/// 被我们最小化的宿主窗口（旧流程遗留；现在只在截图时临时隐藏，截完即恢复）
static HIDDEN: Mutex<Vec<isize>> = Mutex::new(Vec::new());

/// 还原被最小化的宿主窗口（需要用户确认前、回合结束时调用）
pub fn restore_host() {
    let list: Vec<isize> = std::mem::take(&mut *HIDDEN.lock().unwrap_or_else(|p| p.into_inner()));
    win::restore_windows(&list);
}

/// 截取主屏幕（先临时隐藏 `host_pid` 的窗口）：返回 (PNG base64, 前台窗口描述)。
/// 同时记为「最近一张截图」，之后 desktop_mouse 的坐标按这张图换算。
/// 当前前台窗口（不是使驾自己时）的「标题（进程）」描述
pub fn foreground_window(host_pid: u32) -> String {
    if cfg!(windows) { win::foreground_desc(host_pid) } else { String::new() }
}

pub fn snapshot(host_pid: u32) -> Result<(String, String), String> {
    if !cfg!(windows) { return Err("截屏目前只支持 Windows".into()); }
    win::snapshot(host_pid)
}

// ---------------------------------------------------------------- 坐标换算

#[derive(Clone, Copy)]
struct Geo { ox: i32, oy: i32, w: i32, h: i32, pw: u32 }
/// 最近一张截图：(屏幕区域, 图片宽度) → 模型给的图内像素坐标换算回屏幕坐标
static LAST: Mutex<Option<Geo>> = Mutex::new(None);

fn to_screen(x: f64, y: f64) -> (i32, i32) {
    match *LAST.lock().unwrap_or_else(|p| p.into_inner()) {
        Some(g) => {
            let scale = g.pw as f64 / g.w.max(1) as f64;
            (g.ox + (x / scale).round() as i32, g.oy + (y / scale).round() as i32)
        }
        None => (x.round() as i32, y.round() as i32),
    }
}

/// 模型给的坐标按「她看到的图片尺寸」换算回屏幕坐标：x_screen = 区域原点 + x_img * 区域实际宽 / 图片宽。
/// image_width/height 由调用方必填（desktop_mouse 的参数）；没给时退回 to_screen 的旧换算。
fn pos_of(a: &Value, kx: &str, ky: &str) -> Option<(i32, i32)> {
    let x = a.get(kx)?.as_f64()?;
    let y = a.get(ky)?.as_f64()?;
    let iw = a.get("image_width").and_then(Value::as_f64).filter(|v| *v > 0.0);
    let ih = a.get("image_height").and_then(Value::as_f64).filter(|v| *v > 0.0);
    if let (Some(iw), Some(ih)) = (iw, ih) {
        let guard = LAST.lock().unwrap_or_else(|p| p.into_inner());
        return Some(match guard.as_ref() {
            Some(g) => (
                (g.ox as f64 + x * g.w.max(1) as f64 / iw).round() as i32,
                (g.oy as f64 + y * g.h.max(1) as f64 / ih).round() as i32,
            ),
            // 没截过图：按主屏幕换算
            None => {
                #[cfg(windows)]
                {
                    let (sw, sh) = win::primary_size();
                    ((x * sw.max(1) as f64 / iw).round() as i32, (y * sh.max(1) as f64 / ih).round() as i32)
                }
                #[cfg(not(windows))]
                { (x.round() as i32, y.round() as i32) }
            }
        });
    }
    Some(to_screen(x, y))
}

/// 组合键名 → (虚拟键码, 扩展键)
fn vk_of(k: &str) -> Option<(u16, bool)> {
    let k = k.trim().to_ascii_lowercase();
    let v = match k.as_str() {
        "ctrl" | "control" => (0x11, false), "shift" => (0x10, false), "alt" => (0x12, false), "win" | "windows" | "meta" | "cmd" => (0x5B, true),
        "enter" | "return" => (0x0D, false), "esc" | "escape" => (0x1B, false), "tab" => (0x09, false), "space" => (0x20, false),
        "backspace" => (0x08, false), "delete" | "del" => (0x2E, true), "insert" | "ins" => (0x2D, true),
        "up" => (0x26, true), "down" => (0x28, true), "left" => (0x25, true), "right" => (0x27, true),
        "home" => (0x24, true), "end" => (0x23, true), "pageup" | "pgup" => (0x21, true), "pagedown" | "pgdn" => (0x22, true),
        "capslock" => (0x14, false), "printscreen" | "prtsc" => (0x2C, true), "menu" | "apps" => (0x5D, true),
        "media_play_pause" | "play_pause" | "playpause" => (0xB3, true), "media_next" | "next_track" => (0xB0, true),
        "media_prev" | "media_previous" | "prev_track" => (0xB1, true), "media_stop" => (0xB2, true),
        "volume_up" => (0xAF, true), "volume_down" => (0xAE, true), "volume_mute" | "mute" => (0xAD, true),
        _ => {
            let b = k.as_bytes();
            if b.len() == 1 && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit()) { return Some((b[0].to_ascii_uppercase() as u16, false)); }
            if let Some(n) = k.strip_prefix('f').and_then(|n| n.parse::<u16>().ok()).filter(|n| (1..=24).contains(n)) { return Some((0x6F + n, false)); }
            let p = match k.as_str() { "," => 0xBC, "." => 0xBE, "/" => 0xBF, ";" => 0xBA, "-" => 0xBD, "=" => 0xBB, "[" => 0xDB, "]" => 0xDD, "`" => 0xC0, "'" => 0xDE, "\\" => 0xDC, _ => return None };
            (p, false)
        }
    };
    Some(v)
}

pub fn parse_keys(keys: &str) -> Result<Vec<(u16, bool)>, String> {
    let parts: Vec<&str> = keys.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() { return Err("keys 为空".into()); }
    parts.iter().map(|p| vk_of(p).ok_or_else(|| format!("不认识的按键：{p}"))).collect()
}

// ---------------------------------------------------------------- Windows 原生实现

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Rect { l: i32, t: i32, r: i32, b: i32 }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MouseInput { dx: i32, dy: i32, data: u32, flags: u32, time: u32, extra: usize }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct KeybdInput { vk: u16, scan: u16, flags: u32, time: u32, extra: usize }
    #[repr(C)]
    #[derive(Clone, Copy)]
    union InputU { mi: MouseInput, ki: KeybdInput }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Input { kind: u32, u: InputU }

    #[repr(C)]
    struct BitmapInfoHeader {
        size: u32, width: i32, height: i32, planes: u16, bit_count: u16, compression: u32,
        size_image: u32, xppm: i32, yppm: i32, clr_used: u32, clr_important: u32,
    }
    #[repr(C)]
    struct BitmapInfo { h: BitmapInfoHeader, colors: [u32; 1] }

    type EnumProc = unsafe extern "system" fn(isize, isize) -> i32;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetSystemMetrics(i: i32) -> i32;
        fn GetDC(h: isize) -> isize;
        fn ReleaseDC(h: isize, dc: isize) -> i32;
        fn SetCursorPos(x: i32, y: i32) -> i32;
        fn SendInput(n: u32, inputs: *const Input, size: i32) -> u32;
        fn EnumWindows(f: EnumProc, l: isize) -> i32;
        fn GetWindowTextW(h: isize, buf: *mut u16, n: i32) -> i32;
        fn IsWindowVisible(h: isize) -> i32;
        fn IsIconic(h: isize) -> i32;
        fn GetWindowThreadProcessId(h: isize, pid: *mut u32) -> u32;
        fn GetWindowLongW(h: isize, i: i32) -> i32;
        fn GetWindowRect(h: isize, r: *mut Rect) -> i32;
        fn GetWindow(h: isize, cmd: u32) -> isize;
        fn ShowWindow(h: isize, cmd: i32) -> i32;
        fn ShowWindowAsync(h: isize, cmd: i32) -> i32;
        fn SetForegroundWindow(h: isize) -> i32;
        fn BringWindowToTop(h: isize) -> i32;
        fn GetForegroundWindow() -> isize;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateCompatibleDC(dc: isize) -> isize;
        fn CreateCompatibleBitmap(dc: isize, w: i32, h: i32) -> isize;
        fn SelectObject(dc: isize, o: isize) -> isize;
        fn BitBlt(d: isize, x: i32, y: i32, w: i32, h: i32, s: isize, sx: i32, sy: i32, rop: u32) -> i32;
        fn GetDIBits(dc: isize, bmp: isize, start: u32, lines: u32, bits: *mut c_void, bi: *mut BitmapInfo, usage: u32) -> i32;
        fn DeleteObject(o: isize) -> i32;
        fn DeleteDC(dc: isize) -> i32;
    }
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(h: isize, op: *const u16, file: *const u16, params: *const u16, dir: *const u16, show: i32) -> isize;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn CloseHandle(h: isize) -> i32;
        fn QueryFullProcessImageNameW(h: isize, flags: u32, buf: *mut u16, n: *mut u32) -> i32;
        fn GetModuleHandleW(name: *const u16) -> isize;
        fn GetProcAddress(m: isize, name: *const u8) -> *const c_void;
    }

    fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() }

    /// 按物理像素取坐标（内置 Agent 子进程默认不是 DPI 感知的；主程序已由窗口库设置过，再设会失败，无妨）
    fn dpi_aware() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            let m = GetModuleHandleW(wide("user32.dll").as_ptr());
            if m == 0 { return; }
            let f = GetProcAddress(m, b"SetProcessDpiAwarenessContext\0".as_ptr());
            if !f.is_null() {
                let f: unsafe extern "system" fn(isize) -> i32 = std::mem::transmute(f);
                f(-4); // PER_MONITOR_AWARE_V2
            }
        });
    }

    #[derive(Clone)]
    struct Win { h: isize, title: String, exe: String, pid: u32, rect: Rect, min: bool }

    unsafe extern "system" fn collect(h: isize, l: isize) -> i32 {
        let v = unsafe { &mut *(l as *mut Vec<isize>) };
        v.push(h);
        1
    }

    fn exe_name(pid: u32) -> String {
        unsafe {
            let h = OpenProcess(0x1000, 0, pid);
            if h == 0 { return String::new(); }
            let mut buf = [0u16; 520];
            let mut n = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut n);
            CloseHandle(h);
            if ok == 0 { return String::new(); }
            let full = String::from_utf16_lossy(&buf[..n as usize]);
            full.rsplit(['\\', '/']).next().unwrap_or_default().to_string()
        }
    }

    /// 可见的顶层应用窗口（有标题、不是工具窗口、没有所有者）
    fn windows() -> Vec<Win> {
        let mut hs: Vec<isize> = Vec::new();
        unsafe { EnumWindows(collect, &mut hs as *mut Vec<isize> as isize) };
        let mut out = Vec::new();
        for h in hs {
            unsafe {
                if IsWindowVisible(h) == 0 || GetWindow(h, 4) != 0 { continue; }
                if GetWindowLongW(h, -20) as u32 & 0x80 != 0 { continue; } // WS_EX_TOOLWINDOW
                let mut buf = [0u16; 512];
                let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
                if n <= 0 { continue; }
                let title = String::from_utf16_lossy(&buf[..n as usize]);
                if title == "Program Manager" { continue; }
                let mut pid = 0u32;
                GetWindowThreadProcessId(h, &mut pid);
                let mut rect = Rect::default();
                GetWindowRect(h, &mut rect);
                out.push(Win { h, title, exe: exe_name(pid), pid, rect, min: IsIconic(h) != 0 });
            }
        }
        out
    }

    fn find_window(q: &str) -> Result<Win, String> {
        let q = q.trim().to_lowercase();
        if q.is_empty() { return Err("缺少 window".into()); }
        let me = std::process::id();
        let list = windows();
        let pick = list.iter().filter(|w| w.pid != me).find(|w| w.title.to_lowercase() == q)
            .or_else(|| list.iter().filter(|w| w.pid != me).find(|w| w.title.to_lowercase().contains(&q)))
            .or_else(|| list.iter().filter(|w| w.pid != me).find(|w| w.exe.to_lowercase().trim_end_matches(".exe").contains(q.trim_end_matches(".exe"))));
        pick.cloned().ok_or_else(|| format!("没找到标题或进程名含「{q}」的窗口（可先用 desktop_windows 查看）"))
    }

    fn send(inputs: &[Input]) {
        unsafe { SendInput(inputs.len() as u32, inputs.as_ptr(), std::mem::size_of::<Input>() as i32) };
    }
    fn key(vk: u16, ext: bool, up: bool) -> Input {
        let flags = (if ext { 1 } else { 0 }) | (if up { 2 } else { 0 });
        Input { kind: 1, u: InputU { ki: KeybdInput { vk, scan: 0, flags, time: 0, extra: 0 } } }
    }
    fn mouse(flags: u32, data: u32) -> Input {
        Input { kind: 0, u: InputU { mi: MouseInput { dx: 0, dy: 0, data, flags, time: 0, extra: 0 } } }
    }

    fn focus(w: &Win) -> bool {
        unsafe {
            if IsIconic(w.h) != 0 { ShowWindow(w.h, 9); }
            // 后台进程抢前台会被系统拦：先模拟一下 Alt
            send(&[key(0x12, false, false), key(0x12, false, true)]);
            SetForegroundWindow(w.h);
            BringWindowToTop(w.h);
        }
        std::thread::sleep(Duration::from_millis(250));
        unsafe { GetForegroundWindow() == w.h }
    }

    /// 使驾主窗口（有任务栏按钮的普通顶层窗口）在前台时把它最小化（记入 HIDDEN，回合结束 / 弹确认前还原）。
    /// 桌宠等无任务栏小窗在前台不算：那时用户本来就在别的程序上操作。
    pub fn minimize_if_foreground(pid: u32) -> usize {
        let fg = unsafe { GetForegroundWindow() };
        if fg == 0 { return 0; }
        let list = windows();
        if !list.iter().any(|w| w.h == fg && w.pid == pid) { return 0; }
        let mut n = 0;
        for w in list {
            if w.pid != pid || w.min { continue; }
            unsafe { ShowWindowAsync(w.h, 6) }; // SW_MINIMIZE
            HIDDEN.lock().unwrap_or_else(|p| p.into_inner()).push(w.h);
            n += 1;
        }
        n
    }

    /// 临时隐藏本进程中覆盖屏幕点 (x, y) 的可见窗口（桌宠 / 通知），返回隐藏的句柄
    pub fn hide_own_at(pid: u32, x: i32, y: i32) -> Vec<isize> {
        let mut hs: Vec<isize> = Vec::new();
        unsafe { EnumWindows(collect, &mut hs as *mut Vec<isize> as isize) };
        let mut out = Vec::new();
        for h in hs {
            unsafe {
                if IsWindowVisible(h) == 0 || IsIconic(h) != 0 { continue; }
                let mut p = 0u32;
                GetWindowThreadProcessId(h, &mut p);
                if p != pid { continue; }
                let mut r = Rect::default();
                GetWindowRect(h, &mut r);
                if x < r.l || x >= r.r || y < r.t || y >= r.b { continue; }
                ShowWindow(h, 0);
                out.push(h);
            }
        }
        if !out.is_empty() { std::thread::sleep(Duration::from_millis(120)); }
        out
    }

    pub fn restore_windows(list: &[isize]) {
        for &h in list { unsafe { ShowWindowAsync(h, 4) }; } // SW_SHOWNOACTIVATE：还原但不抢前台
    }

    /// 截图前临时隐藏本进程所有可见、未最小化的顶层窗口（含无任务栏的桌宠 / 通知窗口）
    pub fn hide_process_windows(pid: u32) -> Vec<isize> {
        let mut hs: Vec<isize> = Vec::new();
        unsafe { EnumWindows(collect, &mut hs as *mut Vec<isize> as isize) };
        let mut out = Vec::new();
        for h in hs {
            unsafe {
                if IsWindowVisible(h) == 0 || IsIconic(h) != 0 { continue; }
                let mut p = 0u32;
                GetWindowThreadProcessId(h, &mut p);
                if p != pid { continue; }
                ShowWindow(h, 0); // SW_HIDE
                out.push(h);
            }
        }
        out
    }

    pub fn show_windows(list: &[isize]) {
        for &h in list { unsafe { ShowWindowAsync(h, 8) }; } // SW_SHOWNA：原样显示、不抢焦点
    }

    pub fn foreground_desc(own: u32) -> String {
        unsafe {
            let h = GetForegroundWindow();
            if h == 0 { return String::new(); }
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, &mut pid);
            if pid == own { return String::new(); }
            let mut buf = [0u16; 512];
            let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
            let title = if n > 0 { String::from_utf16_lossy(&buf[..n as usize]) } else { String::new() };
            let exe = exe_name(pid);
            match (title.is_empty(), exe.is_empty()) {
                (false, false) => format!("「{title}」（{exe}）"),
                (false, true) => format!("「{title}」"),
                (true, false) => exe,
                _ => String::new(),
            }
        }
    }

    pub fn primary_size() -> (i32, i32) { unsafe { (GetSystemMetrics(0), GetSystemMetrics(1)) } }

    pub fn snapshot(pid: u32) -> Result<(String, String), String> {
        dpi_aware();
        let desc = foreground_desc(pid);
        let hidden = hide_process_windows(pid);
        if !hidden.is_empty() { std::thread::sleep(Duration::from_millis(250)); }
        let (w, h) = unsafe { (GetSystemMetrics(0), GetSystemMetrics(1)) };
        let r = capture(0, 0, w, h).and_then(|img| encode_png(img, 1600));
        show_windows(&hidden);
        let (png, pw, _) = r?;
        *LAST.lock().unwrap_or_else(|p| p.into_inner()) = Some(Geo { ox: 0, oy: 0, w, h, pw });
        Ok((b64(&png), desc))
    }

    fn capture(x: i32, y: i32, w: i32, h: i32) -> Result<image::RgbImage, String> {
        if w <= 0 || h <= 0 { return Err("截图区域为空".into()); }
        unsafe {
            let screen = GetDC(0);
            if screen == 0 { return Err("无法获取屏幕".into()); }
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, w, h);
            let old = SelectObject(mem, bmp);
            let ok = BitBlt(mem, 0, 0, w, h, screen, x, y, 0x00CC_0020 | 0x4000_0000);
            SelectObject(mem, old);
            let mut bi = BitmapInfo {
                h: BitmapInfoHeader { size: std::mem::size_of::<BitmapInfoHeader>() as u32, width: w, height: -h, planes: 1, bit_count: 32,
                    compression: 0, size_image: 0, xppm: 0, yppm: 0, clr_used: 0, clr_important: 0 },
                colors: [0],
            };
            let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];
            let lines = GetDIBits(screen, bmp, 0, h as u32, buf.as_mut_ptr() as *mut c_void, &mut bi, 0);
            DeleteObject(bmp);
            DeleteDC(mem);
            ReleaseDC(0, screen);
            if ok == 0 || lines == 0 { return Err("截图失败".into()); }
            let mut rgb = Vec::with_capacity((w as usize) * (h as usize) * 3);
            for px in buf.chunks_exact(4) { rgb.extend_from_slice(&[px[2], px[1], px[0]]); }
            image::RgbImage::from_raw(w as u32, h as u32, rgb).ok_or_else(|| "截图失败".to_string())
        }
    }

    fn screenshot(a: &Value) -> Result<Output, String> {
        let delay = a.get("delay_ms").and_then(Value::as_u64).unwrap_or(0).min(10_000);
        if delay > 0 { std::thread::sleep(Duration::from_millis(delay)); }
        let max = a.get("max_width").and_then(Value::as_u64).unwrap_or(1280).clamp(480, 1920) as u32;
        let wq = s(a, "window");
        let (x, y, w, h, what) = if !wq.is_empty() {
            let win = find_window(wq)?;
            focus(&win);
            let mut r = Rect::default();
            unsafe { GetWindowRect(win.h, &mut r) };
            (r.l, r.t, r.r - r.l, r.b - r.t, format!("窗口「{}」", win.title))
        } else if s(a, "screen") == "all" {
            unsafe { (GetSystemMetrics(76), GetSystemMetrics(77), GetSystemMetrics(78), GetSystemMetrics(79), "所有显示器".to_string()) }
        } else {
            unsafe { (0, 0, GetSystemMetrics(0), GetSystemMetrics(1), "主屏幕".to_string()) }
        };
        let img = capture(x, y, w, h)?;
        let (png, pw, ph) = encode_png(img, max)?;
        *LAST.lock().unwrap_or_else(|p| p.into_inner()) = Some(Geo { ox: x, oy: y, w, h, pw });
        Ok(Output {
            text: format!("已截图：{what}，图片 {pw}x{ph}（屏幕区域 {w}x{h}，左上角 {x},{y}）。desktop_mouse 的 x / y 用这张图里的像素坐标，并把 image_width={pw} image_height={ph} 一起传入，工具会换算成真实屏幕坐标。"),
            image: Some(data_url("image/png", &png)),
        })
    }

    fn mouse_act(a: &Value) -> Result<Output, String> {
        let act = a.get("action").and_then(Value::as_str).unwrap_or("click");
        let pos = |kx: &str, ky: &str| -> Option<(i32, i32)> { pos_of(a, kx, ky) };
        let at = pos("x", "y");
        if let Some((x, y)) = at { unsafe { SetCursorPos(x, y) }; std::thread::sleep(Duration::from_millis(60)); }
        else if act != "scroll" {
            return Err("缺少 x / y（还要带上你看的截图尺寸 image_width / image_height）".into());
        }
        let click = |down: u32, up: u32, n: usize| {
            for _ in 0..n { send(&[mouse(down, 0), mouse(up, 0)]); std::thread::sleep(Duration::from_millis(70)); }
        };
        match act {
            "click" => click(0x02, 0x04, 1),
            "double_click" => click(0x02, 0x04, 2),
            "right_click" => click(0x08, 0x10, 1),
            "middle_click" => click(0x20, 0x40, 1),
            "move" => {}
            "scroll" => {
                let n = a.get("amount").and_then(Value::as_i64).unwrap_or(-3).clamp(-50, 50);
                send(&[mouse(0x0800, (n * 120) as i32 as u32)]);
            }
            "drag" => {
                let (tx, ty) = pos("to_x", "to_y").ok_or("drag 需要 to_x / to_y")?;
                let (fx, fy) = at.unwrap_or((tx, ty));
                send(&[mouse(0x02, 0)]);
                for i in 1..=12 {
                    let k = i as f64 / 12.0;
                    unsafe { SetCursorPos(fx + ((tx - fx) as f64 * k) as i32, fy + ((ty - fy) as f64 * k) as i32) };
                    std::thread::sleep(Duration::from_millis(25));
                }
                send(&[mouse(0x04, 0)]);
            }
            other => return Err(format!("不支持的 action：{other}")),
        }
        std::thread::sleep(Duration::from_millis(250));
        let fg = unsafe { GetForegroundWindow() };
        let title = windows().into_iter().find(|w| w.h == fg).map(|w| format!("，当前前台窗口「{}」", w.title)).unwrap_or_default();
        Ok(Output::text(format!("已{act}{}{title}", at.map(|(x, y)| format!("（屏幕坐标 {x},{y}）")).unwrap_or_default())))
    }

    fn keyboard(a: &Value) -> Result<Output, String> {
        let mut done = Vec::new();
        let wq = s(a, "window");
        if !wq.is_empty() {
            let w = find_window(wq)?;
            if !focus(&w) { return Err(format!("没能把「{}」切到前台", w.title)); }
            done.push(format!("切到「{}」", w.title));
        }
        let text = s(a, "text");
        if !text.is_empty() {
            for ch in text.replace("\r\n", "\n").encode_utf16() {
                if ch == '\n' as u16 { send(&[key(0x0D, false, false), key(0x0D, false, true)]); }
                else {
                    send(&[
                        Input { kind: 1, u: InputU { ki: KeybdInput { vk: 0, scan: ch, flags: 4, time: 0, extra: 0 } } },
                        Input { kind: 1, u: InputU { ki: KeybdInput { vk: 0, scan: ch, flags: 4 | 2, time: 0, extra: 0 } } },
                    ]);
                }
                std::thread::sleep(Duration::from_millis(4));
            }
            done.push(format!("输入了 {} 个字", text.chars().count()));
        }
        let keys = s(a, "keys");
        if !keys.is_empty() {
            let combo = parse_keys(keys)?;
            let n = a.get("repeat").and_then(Value::as_u64).unwrap_or(1).clamp(1, 20);
            for _ in 0..n {
                let mut seq: Vec<Input> = combo.iter().map(|&(vk, ext)| key(vk, ext, false)).collect();
                seq.extend(combo.iter().rev().map(|&(vk, ext)| key(vk, ext, true)));
                send(&seq);
                std::thread::sleep(Duration::from_millis(60));
            }
            done.push(format!("按了 {keys}{}", if n > 1 { format!(" ×{n}") } else { String::new() }));
        }
        if done.is_empty() { return Err("需要 text 或 keys".into()); }
        Ok(Output::text(done.join("，")))
    }

    fn open(a: &Value) -> Result<Output, String> {
        let target = s(a, "target").trim();
        if target.is_empty() { return Err("缺少 target".into()); }
        let args = s(a, "args");
        let (t, p, op) = (wide(target), wide(args), wide("open"));
        let r = unsafe { ShellExecuteW(0, op.as_ptr(), t.as_ptr(), if args.is_empty() { std::ptr::null() } else { p.as_ptr() }, std::ptr::null(), 1) };
        if r <= 32 {
            return Err(format!("打开失败（代码 {r}）：{target}。如果是程序名，先用 desktop_find_apps 找到快捷方式路径"));
        }
        Ok(Output::text(format!("已打开 {target}。程序启动需要时间，可以用 desktop_screenshot（delay_ms 1500~3000）看看界面")))
    }

    pub fn call(name: &str, a: &Value) -> Result<Output, String> {
        dpi_aware();
        match name {
            "desktop_windows" => {
                let q = s(a, "query").to_lowercase();
                let me = std::process::id();
                let list: Vec<Win> = windows().into_iter()
                    .filter(|w| q.is_empty() || w.title.to_lowercase().contains(&q) || w.exe.to_lowercase().contains(&q))
                    .collect();
                let fg = unsafe { GetForegroundWindow() };
                let mut t = format!("{} 个窗口：\n", list.len());
                for w in list.iter().take(40) {
                    t.push_str(&format!("- 「{}」 {}{}{}{} [{},{} {}x{}]\n", w.title, w.exe,
                        if w.h == fg { " · 前台" } else { "" }, if w.min { " · 最小化" } else { "" },
                        if w.pid == me { " · 使驾自己" } else { "" },
                        w.rect.l, w.rect.t, w.rect.r - w.rect.l, w.rect.b - w.rect.t));
                }
                Ok(Output::text(t))
            }
            "desktop_focus_window" => {
                let w = find_window(s(a, "window"))?;
                let ok = focus(&w);
                Ok(Output::text(if ok { format!("已切到「{}」（{}）", w.title, w.exe) } else { format!("尝试切到「{}」，但系统没让它到前台；可以点击一下它的窗口", w.title) }))
            }
            "desktop_screenshot" => screenshot(a),
            "desktop_mouse" => mouse_act(a),
            "desktop_keyboard" => keyboard(a),
            "desktop_open" => open(a),
            other => Err(format!("未知工具：{other}")),
        }
    }
}

#[cfg(not(windows))]
mod win {
    use super::*;
    pub fn call(_name: &str, _a: &Value) -> Result<Output, String> { Err("这个操作目前只支持 Windows".into()) }
    pub fn restore_windows(_list: &[isize]) {}
    pub fn minimize_if_foreground(_pid: u32) -> usize { 0 }
    pub fn hide_own_at(_pid: u32, _x: i32, _y: i32) -> Vec<isize> { Vec::new() }
    pub fn hide_process_windows(_pid: u32) -> Vec<isize> { Vec::new() }
    pub fn show_windows(_list: &[isize]) {}
    pub fn snapshot(_pid: u32) -> Result<(String, String), String> { Err("截屏目前只支持 Windows".into()) }
    pub fn foreground_desc(_own: u32) -> String { String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_specs() {
        assert_eq!(parse_keys("ctrl+shift+s").unwrap(), vec![(0x11, false), (0x10, false), (0x53, false)]);
        assert_eq!(parse_keys("media_play_pause").unwrap(), vec![(0xB3, true)]);
        assert_eq!(parse_keys("F5").unwrap(), vec![(0x74, false)]);
        assert!(parse_keys("ctrl+nope").is_err());
        let names = |v: Vec<Value>| v.iter().map(|x| x["function"]["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let all = names(tool_specs(true, true, true));
        assert!(all.contains(&"run_command".to_string()) && all.contains(&"desktop_screenshot".to_string()) && all.contains(&"desktop_mouse".to_string()));
        let ro = names(tool_specs(false, false, false));
        assert!(!ro.contains(&"run_command".to_string()) && !ro.contains(&"desktop_mouse".to_string()) && !ro.contains(&"desktop_screenshot".to_string()));
        assert!(needs_confirm("run_command") && needs_confirm("desktop_mouse") && !needs_confirm("desktop_screenshot") && !needs_confirm("desktop_focus_window"));
        assert!(title("desktop_keyboard", &json!({ "keys": "ctrl+v" })).contains("ctrl+v"));
    }

    /// 手动冒烟（不点击、不输入）：cargo test --release desktop_smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn desktop_smoke() {
        let o = Opts { workdir: std::env::temp_dir(), host_pid: 0, hide: false, vision: true };
        let w = call("desktop_windows", &json!({}), &o).unwrap();
        println!("WINDOWS_LINES {}", w.text.lines().count());
        let a = call("desktop_find_apps", &json!({ "query": "" }), &o).unwrap();
        println!("APPS {}", a.text.lines().next().unwrap_or_default());
        let s = call("desktop_screenshot", &json!({ "max_width": 640 }), &o).unwrap();
        println!("SHOT {} {}", s.text.split('，').nth(1).unwrap_or_default(), s.image.map(|i| i.len()).unwrap_or(0));
        let k = call("desktop_keyboard", &json!({}), &o);
        assert!(k.is_err());
    }

    #[test]
    fn coords_and_images() {
        // 区域 (100,50) 1920x1080，截图宽 960（scale 0.5）
        *LAST.lock().unwrap() = Some(Geo { ox: 100, oy: 50, w: 1920, h: 1080, pw: 960 });
        assert_eq!(to_screen(10.0, 20.0), (120, 90));
        // 带图片尺寸的换算：模型看的是 960x540 的图，(480,270) = 区域中心 → (1060, 590)
        assert_eq!(
            pos_of(&json!({ "x": 480, "y": 270, "image_width": 960, "image_height": 540 }), "x", "y"),
            Some((1060, 590))
        );
        // 图片尺寸与旧换算不一致时以图片尺寸为准（旧 scale 会给出 (1060,590) 之外的错值）
        assert_eq!(
            pos_of(&json!({ "x": 480, "y": 270, "image_width": 1280, "image_height": 720 }), "x", "y"),
            Some((820, 455))
        );
        // 没给图片尺寸：退回旧换算
        assert_eq!(pos_of(&json!({ "x": 10.0, "y": 20.0 }), "x", "y"), Some((120, 90)));
        *LAST.lock().unwrap() = None;
        assert_eq!(to_screen(10.4, 20.6), (10, 21));
        // 没截过图 + 图片尺寸 = 主屏幕尺寸 → 坐标原样（按真实主屏幕分辨率算期望值）
        {
            let (sw, sh) = win::primary_size();
            assert_eq!(
                pos_of(&json!({ "x": 480.0, "y": 270.0, "image_width": 1920, "image_height": 1080 }), "x", "y"),
                Some(((480.0 * sw.max(1) as f64 / 1920.0).round() as i32, (270.0 * sh.max(1) as f64 / 1080.0).round() as i32))
            );
        }
        assert_eq!(b64(b"hello"), "aGVsbG8=");
        assert_eq!(b64(b"hi!"), "aGkh");
        let img = image::RgbImage::from_pixel(3000, 1000, image::Rgb([10, 20, 30]));
        let (png, w, h) = encode_png(img, 1500).unwrap();
        assert_eq!((w, h), (1500, 500));
        assert_eq!(mime_of(&png), Some("image/png"));
        let dir = std::env::temp_dir().join(format!("sd-desk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.png");
        std::fs::write(&p, &png).unwrap();
        let o = read_image(&p).unwrap();
        assert!(o.image.unwrap().starts_with("data:image/png;base64,"));
        std::fs::write(dir.join("b.txt"), "hi").unwrap();
        assert!(read_image(&dir.join("b.txt")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        let opts = Opts { workdir: std::env::temp_dir(), host_pid: 0, hide: false, vision: false };
        assert!(call("desktop_screenshot", &json!({}), &opts).is_err());
        let r = call("run_command", &json!({ "command": "echo hello" }), &opts).unwrap();
        assert!(r.text.contains("hello") && r.text.contains("exit_code: 0"));
    }
}
