//! Skin packages are data, installed under one fixed root. Never trust caller paths.
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const MAX_FILES: usize = 128;
const MAX_ASSET: u64 = 8 * 1024 * 1024;
const MAX_TOTAL: u64 = 32 * 1024 * 1024;
const MAX_MANIFEST: u64 = 64 * 1024;

pub fn root() -> Result<PathBuf, String> {
    Ok(
        PathBuf::from(std::env::var("APPDATA").map_err(|_| "无法确定用户数据目录")?)
            .join("com.shidrive.desktop")
            .join("skins"),
    )
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
        && !matches!(id, "steins-gate" | "hell" | "none")
        && !windows_reserved(id)
}
fn windows_reserved(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}
fn safe_relative(name: &str) -> Result<PathBuf, String> {
    if name.is_empty() || name.len() > 240 || name.contains('\\') || name.starts_with('/') {
        return Err("皮肤文件路径无效".into());
    }
    for part in name.split('/') {
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || windows_reserved(part)
            || part
                .chars()
                .any(|c| c.is_control() || ":*?\"<>|".contains(c))
        {
            return Err("皮肤文件路径无效".into());
        }
    }
    Ok(PathBuf::from(name))
}
fn no_link(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("不允许符号链接".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("不允许重解析点".into());
        }
    }
    Ok(())
}
fn bounded_read(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    no_link(path)?;
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > max {
        return Err("皮肤文件超出大小限制".into());
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err("皮肤文件超出大小限制".into());
    }
    Ok(bytes)
}
fn image_mime(path: &Path, bytes: &[u8]) -> Result<&'static str, String> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" if bytes.starts_with(b"\x89PNG\r\n\x1a\n") => Ok("image/png"),
        "jpg" | "jpeg" if bytes.starts_with(&[0xff, 0xd8, 0xff]) => Ok("image/jpeg"),
        "webp" if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") => {
            Ok("image/webp")
        }
        "gif" if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") => Ok("image/gif"),
        _ => Err("皮肤图片仅支持 PNG、JPEG、WebP、GIF，文件内容必须匹配扩展名".into()),
    }
}
fn parse_manifest(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err("skin.json 过大".into());
    }
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("skin.json 解析失败: {e}"))?;
    let obj = value.as_object_mut().ok_or("skin.json 必须是对象")?;
    if !obj.get("id").and_then(Value::as_str).is_some_and(valid_id) {
        return Err("皮肤 id 无效、保留或与内置皮肤冲突".into());
    }
    if let Some(name) = obj.get("name") {
        if !name
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 240)
        {
            return Err("皮肤名称无效".into());
        }
    }
    for key in ["background", "character"] {
        if let Some(file) = obj.get(key) {
            safe_relative(file.as_str().ok_or("图片路径必须为字符串")?)?;
        }
    }
    if let Some(css) = obj.get("css") {
        if !css
            .as_str()
            .is_some_and(|s| s.len() <= MAX_MANIFEST as usize)
        {
            return Err("皮肤 CSS 无效或过大".into());
        }
    }
    if let Some(vars) = obj.get("vars") {
        if !vars.as_object().is_some_and(|m| {
            m.len() <= 32
                && m.iter()
                    .all(|(k, v)| k.len() <= 64 && v.as_str().is_some_and(|s| s.len() <= 128))
        }) {
            return Err("皮肤变量无效或过多".into());
        }
    }
    obj.remove("dir");
    Ok(value)
}
fn installed_dir(root: &Path, requested: &str) -> Result<PathBuf, String> {
    no_link(root)?;
    let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let requested_path = Path::new(requested);
    let candidate = if valid_id(requested) {
        root.join(requested)
    } else if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        return Err("皮肤目录无效".into());
    };
    no_link(&candidate)?;
    let candidate = fs::canonicalize(candidate).map_err(|e| e.to_string())?;
    if candidate.parent() != Some(root.as_path())
        || !candidate
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(valid_id)
    {
        return Err("仅可读取已安装的皮肤".into());
    }
    Ok(candidate)
}
fn contained_file(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let relative = safe_relative(name)?;
    let mut path = dir.to_path_buf();
    for component in relative.components() {
        path.push(component);
        no_link(&path)?;
    }
    let canonical = fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !canonical.starts_with(dir) {
        return Err("皮肤文件超出目录".into());
    }
    Ok(canonical)
}

pub fn import(root: &Path, path: &Path) -> Result<Value, String> {
    let input = fs::File::open(path).map_err(|e| format!("打开皮肤包失败: {e}"))?;
    if input.metadata().map_err(|e| e.to_string())?.len() > MAX_TOTAL {
        return Err("皮肤包超过 32 MiB".into());
    }
    let mut archive = zip::ZipArchive::new(input).map_err(|e| e.to_string())?;
    if archive.len() > MAX_FILES {
        return Err("皮肤包文件数超过 128".into());
    }
    let mut names = HashSet::new();
    let mut entries = Vec::new();
    let mut total = 0u64;
    let mut manifest_index = None;
    for i in 0..archive.len() {
        let f = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = f.name().trim_end_matches('/');
        let relative = safe_relative(name)?;
        if !names.insert(name.to_lowercase()) {
            return Err("皮肤包包含重名文件".into());
        }
        if let Some(mode) = f.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 && kind != 0o040000 {
                return Err("皮肤包包含特殊文件或符号链接".into());
            }
        }
        if f.is_dir() {
            continue;
        }
        let limit = if relative.file_name().and_then(|s| s.to_str()) == Some("skin.json") {
            MAX_MANIFEST
        } else {
            MAX_ASSET
        };
        if f.size() > limit {
            return Err("皮肤文件超出大小限制".into());
        }
        total = total.checked_add(f.size()).ok_or("皮肤包过大")?;
        if total > MAX_TOTAL {
            return Err("皮肤包解压后超过 32 MiB".into());
        }
        if relative.file_name().and_then(|s| s.to_str()) == Some("skin.json") {
            if manifest_index.replace(i).is_some() {
                return Err("皮肤包只能包含一个 skin.json".into());
            }
        }
        entries.push((i, relative, limit));
    }
    let index = manifest_index.ok_or("皮肤包缺少 skin.json")?;
    let mut bytes = Vec::new();
    archive
        .by_index(index)
        .map_err(|e| e.to_string())?
        .take(MAX_MANIFEST + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let manifest = parse_manifest(&bytes)?;
    let id = manifest["id"].as_str().ok_or("皮肤 id 无效")?;
    let prefix = entries
        .iter()
        .find(|(i, _, _)| *i == index)
        .unwrap()
        .1
        .parent()
        .unwrap()
        .to_path_buf();
    // Validate and read the complete bounded package before making any storage changes.
    let mut files = Vec::new();
    let mut actual_total = 0u64;
    for (i, relative, limit) in entries {
        let rel = relative
            .strip_prefix(&prefix)
            .map_err(|_| "皮肤文件必须位于 skin.json 同级目录内")?
            .to_path_buf();
        let mut bytes = Vec::new();
        archive
            .by_index(i)
            .map_err(|e| e.to_string())?
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        actual_total += bytes.len() as u64;
        if bytes.len() as u64 > limit || actual_total > MAX_TOTAL {
            return Err("皮肤解压内容过大".into());
        }
        if i != index {
            if rel.extension().and_then(|s| s.to_str()) != Some("css") {
                image_mime(&rel, &bytes)?;
            } else if bytes.len() as u64 > MAX_MANIFEST {
                return Err("CSS 文件过大".into());
            }
        }
        files.push((rel, bytes));
    }
    for key in ["background", "character"] {
        if let Some(name) = manifest[key].as_str() {
            let rel = safe_relative(name)?;
            let (_, bytes) = files
                .iter()
                .find(|(p, _)| *p == rel)
                .ok_or("皮肤引用的图片不存在")?;
            image_mime(&rel, bytes)?;
        }
    }
    fs::create_dir_all(root).map_err(|e| e.to_string())?;
    no_link(root)?;
    let root = fs::canonicalize(root).map_err(|e| e.to_string())?;
    let dest = root.join(id);
    if dest.try_exists().map_err(|e| e.to_string())? {
        return Err("该皮肤 id 已安装，请使用不同 id".into());
    }
    let stage = root.join(format!(".import-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&stage).map_err(|e| e.to_string())?;
    let result = (|| {
        for (rel, bytes) in files {
            let out = stage.join(rel);
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut output = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(out)
                .map_err(|e| e.to_string())?;
            output.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        // Renaming publishes a complete skin; never overwrite an existing installation.
        fs::rename(&stage, &dest).map_err(|e| e.to_string())?;
        Ok(manifest)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(stage);
    }
    result
}

pub fn list(root: &Path) -> Result<Vec<Value>, String> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    no_link(root)?;
    let mut result = Vec::new();
    for entry in fs::read_dir(root)
        .map_err(|e| e.to_string())?
        .take(512)
        .flatten()
    {
        let id = entry.file_name().to_string_lossy().to_string();
        if !valid_id(&id) {
            continue;
        }
        let Ok(dir) = installed_dir(root, &id) else {
            continue;
        };
        let Ok(bytes) = bounded_read(&dir.join("skin.json"), MAX_MANIFEST) else {
            continue;
        };
        let Ok(mut manifest) = parse_manifest(&bytes) else {
            continue;
        };
        if manifest["id"].as_str() != Some(id.as_str()) {
            continue;
        }
        manifest["dir"] = Value::String(id);
        result.push(manifest);
    }
    result.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(result)
}

pub fn asset(root: &Path, requested: &str, file: &str) -> Result<String, String> {
    let dir = installed_dir(root, requested)?;
    let manifest = parse_manifest(&bounded_read(&dir.join("skin.json"), MAX_MANIFEST)?)?;
    if manifest["id"].as_str() != dir.file_name().and_then(|s| s.to_str()) {
        return Err("皮肤标识不匹配".into());
    }
    if !["background", "character"]
        .iter()
        .any(|key| manifest[*key].as_str() == Some(file))
    {
        return Err("仅可读取 skin.json 声明的图片".into());
    }
    let path = contained_file(&dir, file)?;
    let bytes = bounded_read(&path, MAX_ASSET)?;
    let mime = image_mime(&path, &bytes)?;
    Ok(format!("data:{mime};base64,{}", base64(&bytes)))
}
fn base64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((*chunk.get(1).unwrap_or(&0) as u32) << 8)
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let p =
                std::env::temp_dir().join(format!("shidrive-skin-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn package(dir: &Path, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join(format!("{}.zip", uuid::Uuid::new_v4()));
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        for (name, data) in entries {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
        path
    }
    #[test]
    fn rejects_unsafe_names_and_reserved_ids() {
        for p in [
            "../a", "/a", "a\\b", "a/../b", "C:/a", "a:stream", "a//b", "a.", "CON.png", "a/NUL",
        ] {
            assert!(safe_relative(p).is_err(), "{p}");
        }
        for id in [
            "steins-gate",
            "hell",
            "none",
            "CON",
            "con",
            "a/b",
            ".hidden",
            "a b",
            "",
        ] {
            assert!(!valid_id(id), "{id}");
        }
        assert!(valid_id("my-skin_2"));
    }
    #[test]
    fn imports_nested_assets_without_flattening_and_reads_only_declared_assets() {
        let tmp = Temp::new();
        let root = tmp.0.join("skins");
        let png = b"\x89PNG\r\n\x1a\nexample";
        let zip = package(&tmp.0, &[("pack/skin.json", br##"{"id":"sample","background":"a/bg.png","character":"b/bg.png","vars":{"--bg":"#112233"}}"##), ("pack/a/bg.png", png), ("pack/b/bg.png", png)]);
        import(&root, &zip).unwrap();
        assert_eq!(list(&root).unwrap()[0]["dir"], "sample");
        assert!(asset(&root, "sample", "a/bg.png")
            .unwrap()
            .starts_with("data:image/png;base64,"));
        assert!(asset(&root, "sample", "skin.json").is_err());
        assert!(asset(&root, tmp.0.to_str().unwrap(), "a/bg.png").is_err());
        assert!(asset(&root, "sample", "../a/bg.png").is_err());
        assert!(import(&root, &zip).is_err());
    }
    #[test]
    fn rejects_traversal_duplicate_case_and_reserved_manifest_without_installing() {
        let tmp = Temp::new();
        let root = tmp.0.join("skins");
        let manifest = br#"{"id":"sample"}"#;
        for entries in [
            vec![
                ("skin.json", manifest.as_slice()),
                ("../x.png", b"x".as_slice()),
            ],
            vec![
                ("skin.json", manifest.as_slice()),
                ("A.png", b"x".as_slice()),
                ("a.png", b"x".as_slice()),
            ],
            vec![("skin.json", br#"{"id":"hell"}"#.as_slice())],
            vec![
                ("skin.json", manifest.as_slice()),
                ("other/skin.json", manifest.as_slice()),
            ],
        ] {
            let zip = package(&tmp.0, &entries);
            assert!(import(&root, &zip).is_err());
            assert!(!root.exists());
        }
    }
    #[test]
    fn rejects_oversized_and_missing_assets() {
        let tmp = Temp::new();
        let root = tmp.0.join("skins");
        let large = vec![b'x'; MAX_MANIFEST as usize + 1];
        let zip = package(&tmp.0, &[("skin.json", &large)]);
        assert!(import(&root, &zip).is_err());
        let zip = package(
            &tmp.0,
            &[(
                "skin.json",
                br#"{"id":"sample","background":"missing.png"}"#,
            )],
        );
        assert!(import(&root, &zip).is_err());
        assert!(!root.exists());
    }
    #[test]
    fn rejects_entry_count_and_asset_size_limits() {
        let tmp = Temp::new();
        let root = tmp.0.join("skins");
        let names: Vec<String> = (0..MAX_FILES).map(|i| format!("{i}.png")).collect();
        let mut entries = vec![("skin.json", br#"{"id":"sample"}"#.as_slice())];
        entries.extend(names.iter().map(|s| (s.as_str(), b"x".as_slice())));
        assert!(import(&root, &package(&tmp.0, &entries)).is_err());
        let large = vec![0; MAX_ASSET as usize + 1];
        let entries = [
            ("skin.json", br#"{"id":"sample"}"#.as_slice()),
            ("large.png", large.as_slice()),
        ];
        assert!(import(&root, &package(&tmp.0, &entries)).is_err());
        assert!(!root.exists());
    }
    #[test]
    fn rejects_wrong_image_signature_and_zip_symlink() {
        let tmp = Temp::new();
        let root = tmp.0.join("skins");
        let zip = package(
            &tmp.0,
            &[
                ("skin.json", br#"{"id":"sample","background":"image.png"}"#),
                ("image.png", b"private text"),
            ],
        );
        assert!(import(&root, &zip).is_err());
        let path = tmp.0.join("symlink.zip");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        zip.start_file("skin.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(br#"{"id":"sample"}"#).unwrap();
        zip.add_symlink(
            "image.png",
            "../outside.png",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.finish().unwrap();
        assert!(import(&root, &path).is_err());
        assert!(!root.exists());
    }
    #[test]
    fn base64_padding() {
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
    }
}
