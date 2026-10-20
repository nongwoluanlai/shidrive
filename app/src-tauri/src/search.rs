//! 全局搜索（文件树 Alt+F）：文件名快速打开 + 内容搜索。
//!
//! 轻量自研实现，不引入 ignore/grep 全家桶（regex / memchr 本就是已有依赖的传递依赖）：
//! - 多线程目录遍历（工作队列 + 条件变量），跳过 .git/.svn/.hg/node_modules 与符号链接；
//! - 轻量 .gitignore 支持（含子目录 .gitignore、上级仓库的 .gitignore、`!` 反选、`dir/`）；
//! - 内容搜索用 regex::bytes（字面量自动走 memchr/memmem 加速），跳过二进制与超大文件；
//! - 新搜索自动取消旧搜索（全局代数），结果数有上限，避免卡 UI。

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use regex::bytes::{Regex as BytesRegex, RegexBuilder as BytesRegexBuilder};
use regex::{RegexSet, RegexSetBuilder};
use serde::{Deserialize, Serialize};

const ALWAYS_SKIP: &[&str] = &[".git", ".svn", ".hg", "node_modules"];
const MAX_FILE_SIZE: u64 = 4 * 1024 * 1024;
const MAX_MATCHES_PER_FILE: usize = 100;
const MAX_LIST_FILES: usize = 300_000;
const PREVIEW_CHARS: usize = 220;
const FILE_LIST_TTL: Duration = Duration::from_secs(10);

static GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchOpts {
    pub root: String,
    pub query: String,
    /// "files"（文件名）| "content"（内容）
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub case_sensitive: bool,
    #[serde(default)]
    pub whole_word: bool,
    #[serde(default)]
    pub regex: bool,
    /// 逗号分隔的 glob，如 `*.rs, src/**`
    #[serde(default)]
    pub include: String,
    #[serde(default)]
    pub exclude: String,
    /// false = 不读取 .gitignore
    #[serde(default = "yes")]
    pub use_ignore: bool,
    #[serde(default)]
    pub max_results: Option<usize>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHit {
    pub path: String,
    pub rel: String,
    pub score: i64,
    /// rel 中命中字符的下标（按 char 计），用于高亮
    pub indices: Vec<usize>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LineMatch {
    /// 1 起
    pub line: usize,
    /// 行内 UTF-16 偏移（0 起），与 JS 字符串下标一致
    pub col: usize,
    /// 匹配长度（UTF-16）
    pub len: usize,
    pub preview: String,
    /// 匹配在 preview 中的 UTF-16 偏移
    pub pstart: usize,
    pub plen: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentHit {
    pub path: String,
    pub rel: String,
    pub matches: Vec<LineMatch>,
    /// 单文件匹配超过上限被截断
    pub more: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub files: Vec<FileHit>,
    pub content: Vec<ContentHit>,
    pub total_matches: usize,
    pub searched_files: usize,
    pub truncated: bool,
    pub cancelled: bool,
    pub elapsed_ms: u64,
}

// ---------------------------------------------------------------- glob / ignore

/// gitignore 风格 glob → 正则（不含锚点）。
fn glob_to_regex(glob: &str) -> String {
    let chars: Vec<char> = glob.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' => {
                if chars.get(i + 1) == Some(&'*') {
                    i += 1;
                    if chars.get(i + 1) == Some(&'/') {
                        i += 1;
                        out.push_str("(?:.*/)?");
                    } else {
                        out.push_str(".*");
                    }
                } else {
                    out.push_str("[^/]*");
                }
            }
            '?' => out.push_str("[^/]"),
            '[' => {
                if let Some(end) = chars[i + 1..].iter().position(|&x| x == ']').map(|p| p + i + 1) {
                    let mut cls: String = chars[i + 1..end].iter().collect();
                    if let Some(rest) = cls.strip_prefix('!') {
                        cls = format!("^{rest}");
                    }
                    out.push('[');
                    out.push_str(&cls.replace('\\', "\\\\"));
                    out.push(']');
                    i = end;
                } else {
                    out.push_str("\\[");
                }
            }
            '\\' => {
                if let Some(n) = chars.get(i + 1) {
                    out.push_str(&regex::escape(&n.to_string()));
                    i += 1;
                }
            }
            c => out.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    out
}

struct Rule {
    negate: bool,
    dir_only: bool,
}

/// 一个 .gitignore 文件编译后的规则集合。
struct IgnoreFile {
    /// 该 .gitignore 所在目录相对搜索根的路径（"" = 根；`/` 分隔）
    base: String,
    /// 位于搜索根之上的 .gitignore：搜索根相对该文件目录的前缀（如 "app/"）
    above_prefix: Option<String>,
    rules: Vec<Rule>,
    names: RegexSet,
    names_idx: Vec<usize>,
    paths: RegexSet,
    paths_idx: Vec<usize>,
}

impl IgnoreFile {
    fn parse(text: &str, base: String, above_prefix: Option<String>) -> Option<IgnoreFile> {
        let mut rules = Vec::new();
        let (mut names, mut names_idx, mut paths, mut paths_idx) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for raw in text.lines() {
            let line = raw.trim_end_matches('\r');
            let line = if line.ends_with("\\ ") { line } else { line.trim_end() };
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (negate, mut pat) = match line.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, line),
            };
            if let Some(rest) = pat.strip_prefix("\\") {
                if rest.starts_with('#') || rest.starts_with('!') {
                    pat = rest;
                }
            }
            let dir_only = pat.ends_with('/');
            let pat = pat.trim_end_matches('/');
            if pat.is_empty() {
                continue;
            }
            let anchored = pat.contains('/');
            let pat = pat.trim_start_matches('/');
            let re = format!("^{}$", glob_to_regex(pat));
            let idx = rules.len();
            rules.push(Rule { negate, dir_only });
            if anchored {
                paths.push(re);
                paths_idx.push(idx);
            } else {
                names.push(re);
                names_idx.push(idx);
            }
        }
        if rules.is_empty() {
            return None;
        }
        let build = |v: &Vec<String>| RegexSetBuilder::new(v).case_insensitive(cfg!(windows)).size_limit(8 << 20).build();
        let names = build(&names).ok()?;
        let paths = build(&paths).ok()?;
        Some(IgnoreFile { base, above_prefix, rules, names, names_idx, paths, paths_idx })
    }

    /// Some(true)=忽略，Some(false)=被 `!` 反选，None=本文件无规则命中。
    fn decide(&self, rel: &str, name: &str, is_dir: bool) -> Option<bool> {
        let local: std::borrow::Cow<str> = match &self.above_prefix {
            Some(p) => format!("{p}{rel}").into(),
            None if self.base.is_empty() => rel.into(),
            None => rel.strip_prefix(&self.base)?.strip_prefix('/')?.into(),
        };
        let mut best: Option<usize> = None;
        let mut consider = |idx: usize| {
            let r = &self.rules[idx];
            if r.dir_only && !is_dir {
                return;
            }
            if best.map_or(true, |b| idx > b) {
                best = Some(idx);
            }
        };
        for m in self.names.matches(name).iter() {
            consider(self.names_idx[m]);
        }
        for m in self.paths.matches(&local).iter() {
            consider(self.paths_idx[m]);
        }
        best.map(|i| !self.rules[i].negate)
    }
}

/// 由根到当前目录的 .gitignore 链（Arc 共享父链，子目录只追加自己的）。
struct IgnoreChain {
    file: Option<IgnoreFile>,
    parent: Option<Arc<IgnoreChain>>,
}

impl IgnoreChain {
    fn is_ignored(&self, rel: &str, name: &str, is_dir: bool) -> bool {
        let mut cur = Some(self);
        while let Some(c) = cur {
            if let Some(f) = &c.file {
                if let Some(d) = f.decide(rel, name, is_dir) {
                    return d;
                }
            }
            cur = c.parent.as_deref();
        }
        false
    }
}

/// 搜索根之上、直到仓库根（含 .git 的目录）的 .gitignore 链。
fn ancestor_chain(root: &Path) -> Option<Arc<IgnoreChain>> {
    let mut dirs = Vec::new();
    let mut cur = root.parent();
    let mut found_repo = root.join(".git").exists();
    while !found_repo {
        let Some(d) = cur else { break };
        dirs.push(d.to_path_buf());
        if d.join(".git").exists() {
            found_repo = true;
            break;
        }
        if dirs.len() > 12 {
            break;
        }
        cur = d.parent();
    }
    if !found_repo {
        return None;
    }
    let mut chain: Option<Arc<IgnoreChain>> = None;
    // 从仓库根往下（浅→深），深的规则优先，所以深的放在链头
    for d in dirs.iter().rev() {
        let Ok(text) = std::fs::read_to_string(d.join(".gitignore")) else { continue };
        let prefix = root
            .strip_prefix(d)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .map(|p| if p.is_empty() { p } else { format!("{p}/") })
            .unwrap_or_default();
        if let Some(f) = IgnoreFile::parse(&text, String::new(), Some(prefix)) {
            chain = Some(Arc::new(IgnoreChain { file: Some(f), parent: chain }));
        }
    }
    chain
}

/// include/exclude 过滤（逗号分隔 glob；不含 `/` 的匹配文件名，否则匹配相对路径）。
struct PathFilter {
    names: Option<RegexSet>,
    paths: Option<RegexSet>,
}

impl PathFilter {
    fn new(spec: &str) -> Result<Option<PathFilter>, String> {
        let mut names = Vec::new();
        let mut paths = Vec::new();
        for g in spec.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()) {
            let g = g.replace('\\', "/");
            let g = g.trim_start_matches("./").trim_start_matches('/');
            let g = g.trim_end_matches('/');
            if g.is_empty() {
                continue;
            }
            if g.contains('/') {
                // `src/**` 与 `src` 都应覆盖 src 下所有文件
                paths.push(format!("^{}(?:/.*)?$", glob_to_regex(g)));
            } else {
                names.push(format!("^{}$", glob_to_regex(g)));
                // 目录名形式：任意一层目录名匹配
                paths.push(format!("(?:^|/){}/", glob_to_regex(g)));
            }
        }
        if names.is_empty() && paths.is_empty() {
            return Ok(None);
        }
        let build = |v: Vec<String>| -> Result<Option<RegexSet>, String> {
            if v.is_empty() {
                return Ok(None);
            }
            RegexSetBuilder::new(v).case_insensitive(true).build().map(Some).map_err(|e| format!("过滤规则无效: {e}"))
        };
        Ok(Some(PathFilter { names: build(names)?, paths: build(paths)? }))
    }

    fn matches(&self, rel: &str, name: &str) -> bool {
        self.names.as_ref().is_some_and(|s| s.is_match(name)) || self.paths.as_ref().is_some_and(|s| s.is_match(rel))
    }
}

// ---------------------------------------------------------------- walker

struct Job {
    dir: PathBuf,
    rel: String,
    chain: Option<Arc<IgnoreChain>>,
}

struct Queue {
    state: Mutex<(Vec<Job>, usize)>,
    cv: Condvar,
}

impl Queue {
    fn pop(&self, abort: &dyn Fn() -> bool) -> Option<Job> {
        let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if abort() {
                g.0.clear();
                self.cv.notify_all();
                return None;
            }
            if let Some(j) = g.0.pop() {
                g.1 += 1;
                return Some(j);
            }
            if g.1 == 0 {
                self.cv.notify_all();
                return None;
            }
            g = self.cv.wait_timeout(g, Duration::from_millis(50)).unwrap_or_else(|p| p.into_inner()).0;
        }
    }
    fn finish(&self, more: Vec<Job>) {
        let mut g = self.state.lock().unwrap_or_else(|p| p.into_inner());
        g.1 -= 1;
        let n = more.len();
        g.0.extend(more);
        if n > 0 || g.1 == 0 {
            self.cv.notify_all();
        }
    }
}

/// 并行遍历；对每个（未被忽略的）文件调用 `on_file(path, rel, name)`。
fn walk(
    root: &Path,
    use_ignore: bool,
    abort: &(dyn Fn() -> bool + Sync),
    on_file: &(dyn Fn(&Path, &str, &str) + Sync),
) {
    let chain = if use_ignore { ancestor_chain(root) } else { None };
    let queue = Queue { state: Mutex::new((vec![Job { dir: root.to_path_buf(), rel: String::new(), chain }], 0)), cv: Condvar::new() };
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(2, 12);
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                while let Some(job) = queue.pop(abort) {
                    let more = visit_dir(&job, use_ignore, abort, on_file);
                    queue.finish(more);
                }
            });
        }
    });
}

fn visit_dir(
    job: &Job,
    use_ignore: bool,
    abort: &(dyn Fn() -> bool + Sync),
    on_file: &(dyn Fn(&Path, &str, &str) + Sync),
) -> Vec<Job> {
    let Ok(rd) = std::fs::read_dir(&job.dir) else { return Vec::new() };
    let entries: Vec<std::fs::DirEntry> = rd.flatten().collect();
    let mut chain = job.chain.clone();
    if use_ignore && entries.iter().any(|e| e.file_name() == ".gitignore") {
        if let Ok(text) = std::fs::read_to_string(job.dir.join(".gitignore")) {
            if let Some(f) = IgnoreFile::parse(&text, job.rel.clone(), None) {
                chain = Some(Arc::new(IgnoreChain { file: Some(f), parent: chain }));
            }
        }
    }
    let mut subdirs = Vec::new();
    for e in entries {
        if abort() {
            break;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue; // 不跟随符号链接 / junction，避免环与跑出项目
        }
        let name_os = e.file_name();
        let name = name_os.to_string_lossy();
        let is_dir = ft.is_dir();
        if is_dir && ALWAYS_SKIP.iter().any(|s| name.eq_ignore_ascii_case(s)) {
            continue;
        }
        let rel = if job.rel.is_empty() { name.to_string() } else { format!("{}/{}", job.rel, name) };
        if let Some(c) = &chain {
            if c.is_ignored(&rel, &name, is_dir) {
                continue;
            }
        }
        if is_dir {
            subdirs.push(Job { dir: e.path(), rel, chain: chain.clone() });
        } else {
            on_file(&e.path(), &rel, &name);
        }
    }
    subdirs
}

// ---------------------------------------------------------------- file-name mode

struct FileListCache {
    key: String,
    at: Instant,
    files: Arc<Vec<String>>,
}

static FILE_CACHE: Mutex<Option<FileListCache>> = Mutex::new(None);

fn list_files(root: &Path, use_ignore: bool, abort: &(dyn Fn() -> bool + Sync)) -> (Arc<Vec<String>>, bool) {
    let key = format!("{}|{use_ignore}", root.to_string_lossy());
    if let Some(c) = FILE_CACHE.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        if c.key == key && c.at.elapsed() < FILE_LIST_TTL {
            return (c.files.clone(), false);
        }
    }
    let out = Mutex::new(Vec::new());
    let full = AtomicBool::new(false);
    let stop = || abort() || full.load(Ordering::Relaxed);
    walk(root, use_ignore, &stop, &|_p, rel, _n| {
        let mut g = out.lock().unwrap_or_else(|p| p.into_inner());
        if g.len() >= MAX_LIST_FILES {
            full.store(true, Ordering::Relaxed);
            return;
        }
        g.push(rel.to_string());
    });
    let truncated = full.load(Ordering::Relaxed);
    let mut files = out.into_inner().unwrap_or_else(|p| p.into_inner());
    files.sort_unstable();
    let files = Arc::new(files);
    if !abort() {
        *FILE_CACHE.lock().unwrap_or_else(|p| p.into_inner()) = Some(FileListCache { key, at: Instant::now(), files: files.clone() });
    }
    (files, truncated)
}

fn is_boundary(prev: Option<char>, c: char) -> bool {
    match prev {
        None => true,
        Some(p) => matches!(p, '/' | '\\' | '_' | '-' | '.' | ' ') || (p.is_lowercase() && c.is_uppercase()),
    }
}

/// 子序列模糊匹配打分；None = 不匹配。`text` 为原文，`indices` 为 char 下标。
pub fn fuzzy_score(query: &str, text: &str) -> Option<(i64, Vec<usize>)> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(|c| c.to_lowercase()).collect();
    if q.is_empty() {
        return Some((0, Vec::new()));
    }
    let t: Vec<char> = text.chars().collect();
    let tl: Vec<char> = t.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    // 连续子串命中最优先
    let ql: String = q.iter().collect();
    let tls: String = tl.iter().collect();
    if let Some(byte_pos) = tls.rfind(&ql) {
        let start = tls[..byte_pos].chars().count();
        let mut score = 100 + q.len() as i64 * 10;
        if is_boundary(start.checked_sub(1).map(|i| t[i]), t[start]) {
            score += 30;
        }
        return Some((score, (start..start + q.len()).collect()));
    }
    let mut indices = Vec::with_capacity(q.len());
    let mut score = 0i64;
    let mut ti = 0;
    let mut last: Option<usize> = None;
    for &qc in &q {
        let mut found = None;
        while ti < tl.len() {
            if tl[ti] == qc {
                found = Some(ti);
                ti += 1;
                break;
            }
            ti += 1;
        }
        let i = found?;
        score += 1;
        if is_boundary(i.checked_sub(1).map(|k| t[k]), t[i]) {
            score += 8;
        }
        if let Some(l) = last {
            if l + 1 == i {
                score += 5;
            } else {
                score -= ((i - l) as i64).min(10);
            }
        }
        last = Some(i);
        indices.push(i);
    }
    Some((score, indices))
}

fn search_files(root: &Path, opts: &SearchOpts, abort: &(dyn Fn() -> bool + Sync), res: &mut SearchResult) {
    let (files, truncated) = list_files(root, opts.use_ignore, abort);
    res.truncated = truncated;
    res.searched_files = files.len();
    let limit = opts.max_results.unwrap_or(200).clamp(1, 2000);
    let query = opts.query.trim().replace('\\', "/").replace('＿', "_");
    let include = PathFilter::new(&opts.include).ok().flatten();
    let exclude = PathFilter::new(&opts.exclude).ok().flatten();
    let mut hits: Vec<FileHit> = Vec::new();
    for rel in files.iter() {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        if include.as_ref().is_some_and(|f| !f.matches(rel, name)) || exclude.as_ref().is_some_and(|f| f.matches(rel, name)) {
            continue;
        }
        let name_start = rel.chars().count() - name.chars().count();
        let scored = if query.contains('/') {
            fuzzy_score(&query, rel)
        } else {
            match fuzzy_score(&query, name) {
                // 文件名命中加权，下标换算回 rel
                Some((s, idx)) => Some((s + 50, idx.into_iter().map(|i| i + name_start).collect())),
                None => fuzzy_score(&query, rel).map(|(s, i)| (s - 20, i)),
            }
        };
        if let Some((score, indices)) = scored {
            // 越短、越浅越靠前
            let score = score * 100 - rel.len() as i64 - rel.matches('/').count() as i64 * 5;
            hits.push(FileHit { path: String::new(), rel: rel.clone(), score, indices });
        }
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.rel.cmp(&b.rel)));
    if hits.len() > limit {
        hits.truncate(limit);
        res.truncated = true;
    }
    for h in &mut hits {
        h.path = root.join(h.rel.replace('/', std::path::MAIN_SEPARATOR_STR)).to_string_lossy().to_string();
        h.rel = h.rel.replace('/', "\\");
    }
    res.total_matches = hits.len();
    res.files = hits;
}

// ---------------------------------------------------------------- content mode

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 字面量查询转正则：中文输入法下容易打出全角下划线「＿」，半角 / 全角互相匹配。
fn literal_pattern(q: &str) -> String {
    q.chars()
        .map(|c| match c {
            '_' | '＿' => "[_＿]".to_string(),
            c => regex::escape(c.encode_utf8(&mut [0; 4])),
        })
        .collect()
}

fn build_matcher(opts: &SearchOpts) -> Result<BytesRegex, String> {
    let q = &opts.query;
    let mut pat = if opts.regex { q.clone() } else { literal_pattern(q) };
    if opts.whole_word {
        if opts.regex {
            pat = format!(r"\b(?:{pat})\b");
        } else {
            // 只在首 / 尾是单词字符的一侧加 \b：否则 `foo(`、`.bar` 这类查询在全字匹配下永远搜不到
            let lead = if q.chars().next().is_some_and(is_word_char) { r"\b" } else { "" };
            let tail = if q.chars().last().is_some_and(is_word_char) { r"\b" } else { "" };
            pat = format!("{lead}(?:{pat}){tail}");
        }
    }
    BytesRegexBuilder::new(&pat)
        .case_insensitive(!opts.case_sensitive)
        .multi_line(true)
        .size_limit(32 << 20)
        .build()
        .map_err(|e| format!("正则表达式无效: {e}"))
}

fn utf16_len(b: &[u8]) -> usize {
    String::from_utf8_lossy(b).encode_utf16().count()
}

/// 在一个文件内搜索；返回 None 表示跳过（二进制/过大/读失败）。
fn search_in_bytes(data: &[u8], re: &BytesRegex, max: usize) -> (Vec<LineMatch>, bool) {
    let mut out = Vec::new();
    let mut more = false;
    let mut line_no = 1usize;
    let mut counted_to = 0usize;
    for m in re.find_iter(data) {
        if m.start() == m.end() {
            continue; // 空匹配（如 `^`）无意义
        }
        if out.len() >= max {
            more = true;
            break;
        }
        line_no += memchr::memchr_iter(b'\n', &data[counted_to..m.start()]).count();
        counted_to = m.start();
        let ls = memchr::memrchr(b'\n', &data[..m.start()]).map(|p| p + 1).unwrap_or(0);
        let le = memchr::memchr(b'\n', &data[m.start()..]).map(|p| p + m.start()).unwrap_or(data.len());
        let me = m.end().min(le); // 跨行匹配只高亮首行部分
        let line = &data[ls..le];
        let line = if line.ends_with(b"\r") { &line[..line.len() - 1] } else { line };
        let col = utf16_len(&data[ls..m.start()]);
        let len = utf16_len(&data[m.start()..me]).max(1);
        // 预览：行太长时以匹配为中心截取
        let text = String::from_utf8_lossy(line);
        let chars: Vec<char> = text.chars().collect();
        let col_chars = String::from_utf8_lossy(&data[ls..m.start()]).chars().count();
        let (pv, pstart) = if chars.len() <= PREVIEW_CHARS {
            let lead = chars.iter().take_while(|c| c.is_whitespace()).count().min(col_chars);
            let pv: String = chars[lead..].iter().collect();
            let pstart: usize = chars[lead..col_chars.min(chars.len())].iter().map(|c| c.len_utf16()).sum();
            (pv, pstart)
        } else {
            let from = col_chars.saturating_sub(60);
            let to = (from + PREVIEW_CHARS).min(chars.len());
            let pv: String = std::iter::once('…').chain(chars[from..to].iter().copied()).collect();
            let pstart: usize = 1 + chars[from..col_chars.min(to)].iter().map(|c| c.len_utf16()).sum::<usize>();
            (pv, pstart)
        };
        let plen = len.min(pv.encode_utf16().count().saturating_sub(pstart));
        out.push(LineMatch { line: line_no, col, len, preview: pv, pstart, plen });
    }
    (out, more)
}

fn search_content(root: &Path, opts: &SearchOpts, abort: &(dyn Fn() -> bool + Sync), res: &mut SearchResult) -> Result<(), String> {
    let re = build_matcher(opts)?;
    let include = PathFilter::new(&opts.include)?;
    let exclude = PathFilter::new(&opts.exclude)?;
    let limit = opts.max_results.unwrap_or(2000).clamp(1, 20000);
    let total = AtomicUsize::new(0);
    let searched = AtomicUsize::new(0);
    let full = AtomicBool::new(false);
    let hits = Mutex::new(Vec::<ContentHit>::new());
    let stop = || abort() || full.load(Ordering::Relaxed);
    walk(root, opts.use_ignore, &stop, &|path, rel, name| {
        if include.as_ref().is_some_and(|f| !f.matches(rel, name)) || exclude.as_ref().is_some_and(|f| f.matches(rel, name)) {
            return;
        }
        if full.load(Ordering::Relaxed) {
            return;
        }
        let Ok(meta) = std::fs::metadata(path) else { return };
        if meta.len() > MAX_FILE_SIZE || meta.len() == 0 {
            return;
        }
        let Ok(data) = std::fs::read(path) else { return };
        if memchr::memchr(0, &data[..data.len().min(8192)]).is_some() {
            return; // 二进制
        }
        searched.fetch_add(1, Ordering::Relaxed);
        let (matches, more) = search_in_bytes(&data, &re, MAX_MATCHES_PER_FILE);
        if matches.is_empty() {
            return;
        }
        let n = matches.len();
        let before = total.fetch_add(n, Ordering::Relaxed);
        if before + n >= limit {
            full.store(true, Ordering::Relaxed);
        }
        hits.lock().unwrap_or_else(|p| p.into_inner()).push(ContentHit {
            path: path.to_string_lossy().to_string(),
            rel: rel.replace('/', "\\"),
            matches,
            more,
        });
    });
    let mut hits = hits.into_inner().unwrap_or_else(|p| p.into_inner());
    hits.sort_by(|a, b| a.rel.to_lowercase().cmp(&b.rel.to_lowercase()));
    res.total_matches = hits.iter().map(|h| h.matches.len()).sum();
    res.truncated = full.load(Ordering::Relaxed) || hits.iter().any(|h| h.more);
    res.searched_files = searched.load(Ordering::Relaxed);
    res.content = hits;
    Ok(())
}

// ---------------------------------------------------------------- entry points

pub fn cancel() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

/// 阻塞执行（调用方放到 spawn_blocking 里）。新的调用会让进行中的旧搜索尽快返回 cancelled。
pub fn search(opts: SearchOpts) -> Result<SearchResult, String> {
    let my_gen = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let abort = move || GENERATION.load(Ordering::Relaxed) != my_gen;
    let started = Instant::now();
    let root_s = crate::fsops::normalize_dir(&opts.root);
    if root_s.is_empty() {
        return Err("项目未绑定目录".into());
    }
    let root = PathBuf::from(&root_s);
    if !root.is_dir() {
        return Err(format!("目录不存在: {root_s}"));
    }
    let mut res = SearchResult::default();
    if !opts.query.is_empty() {
        if opts.mode == "content" {
            search_content(&root, &opts, &abort, &mut res)?;
        } else {
            search_files(&root, &opts, &abort, &mut res);
        }
    }
    res.cancelled = abort();
    res.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(res)
}

/// 目录「签名」：子项名称 + 类型的哈希。文件树轮询它来判断是否需要重新加载，
/// 只读目录项、不读文件内容，开销极低。读取失败返回 None（目录被删/无权限）。
pub fn dir_signatures(paths: &[String]) -> Vec<Option<String>> {
    paths
        .iter()
        .map(|p| {
            let dir = crate::fsops::normalize_dir(p);
            let rd = std::fs::read_dir(&dir).ok()?;
            let mut names: Vec<(String, bool)> = rd
                .flatten()
                .filter_map(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    if n == ".git" {
                        return None;
                    }
                    let d = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    Some((n, d))
                })
                .collect();
            names.sort();
            let mut h = DefaultHasher::new();
            names.hash(&mut h);
            Some(format!("{:x}", h.finish()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ig(text: &str) -> IgnoreFile {
        IgnoreFile::parse(text, String::new(), None).unwrap()
    }

    #[test]
    fn gitignore_basics() {
        let f = ig("# c\n/target\nnode_modules/\n*.log\n!keep.log\ndocs/**/*.tmp\n");
        assert_eq!(f.decide("target", "target", true), Some(true));
        assert_eq!(f.decide("src/target", "target", true), None, "anchored to root");
        assert_eq!(f.decide("a/node_modules", "node_modules", true), Some(true));
        assert_eq!(f.decide("a/node_modules", "node_modules", false), None, "dir-only");
        assert_eq!(f.decide("x/err.log", "err.log", false), Some(true));
        assert_eq!(f.decide("x/keep.log", "keep.log", false), Some(false), "negation wins (later rule)");
        assert_eq!(f.decide("docs/a/b/c.tmp", "c.tmp", false), Some(true));
        assert_eq!(f.decide("docs/c.tmp", "c.tmp", false), Some(true));
        assert_eq!(f.decide("src/main.rs", "main.rs", false), None);
    }

    #[test]
    fn nested_and_ancestor_gitignore_paths() {
        let nested = IgnoreFile::parse("/dist\n", "app".into(), None).unwrap();
        assert_eq!(nested.decide("app/dist", "dist", true), Some(true));
        assert_eq!(nested.decide("dist", "dist", true), None);
        let above = IgnoreFile::parse("/app/build\n", String::new(), Some("app/".into())).unwrap();
        assert_eq!(above.decide("build", "build", true), Some(true));
    }

    #[test]
    fn fuzzy_prefers_contiguous_and_boundaries() {
        let (s1, i1) = fuzzy_score("ftree", "FileTree.svelte").unwrap();
        assert_eq!(i1.len(), 5);
        let (s2, _) = fuzzy_score("filetree", "FileTree.svelte").unwrap();
        assert!(s2 > s1);
        assert!(fuzzy_score("xyz", "FileTree.svelte").is_none());
        let (_, idx) = fuzzy_score("tree", "FileTree.svelte").unwrap();
        assert_eq!(idx, vec![4, 5, 6, 7]);
    }

    #[test]
    fn content_matches_report_utf16_columns() {
        let opts = SearchOpts {
            root: String::new(),
            query: "世界".into(),
            mode: "content".into(),
            case_sensitive: false,
            whole_word: false,
            regex: false,
            include: String::new(),
            exclude: String::new(),
            use_ignore: true,
            max_results: None,
        };
        let re = build_matcher(&opts).unwrap();
        let data = "第一行\r\n  你好😀世界 世界\nend".as_bytes();
        let (m, more) = search_in_bytes(data, &re, 10);
        assert!(!more);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].line, 2);
        assert_eq!(m[0].col, 6, "2 spaces + 你好 + surrogate pair");
        assert_eq!(m[0].len, 2);
        assert_eq!(m[0].preview, "你好😀世界 世界");
        assert_eq!(m[0].pstart, 4);
        assert_eq!(m[1].col, 9);
    }

    #[test]
    fn whole_word_and_case() {
        let mut opts = SearchOpts {
            root: String::new(),
            query: "Foo".into(),
            mode: "content".into(),
            case_sensitive: true,
            whole_word: true,
            regex: false,
            include: String::new(),
            exclude: String::new(),
            use_ignore: true,
            max_results: None,
        };
        let re = build_matcher(&opts).unwrap();
        assert_eq!(search_in_bytes(b"Foo foo FooBar", &re, 10).0.len(), 1);
        opts.case_sensitive = false;
        opts.whole_word = false;
        let re = build_matcher(&opts).unwrap();
        assert_eq!(search_in_bytes(b"Foo foo FooBar", &re, 10).0.len(), 3);
        opts.regex = true;
        opts.query = "(".into();
        assert!(build_matcher(&opts).is_err());
    }

    #[test]
    fn path_filter() {
        let f = PathFilter::new("*.rs, src/lib").unwrap().unwrap();
        assert!(f.matches("a/b.rs", "b.rs"));
        assert!(f.matches("src/lib/x.ts", "x.ts"));
        assert!(!f.matches("src/x.ts", "x.ts"));
        let d = PathFilter::new("dist").unwrap().unwrap();
        assert!(d.matches("app/dist/a.js", "a.js"));
        assert!(PathFilter::new(" , ").unwrap().is_none());
    }

    #[test]
    fn underscore_queries() {
        let base = std::env::temp_dir().join(format!("shidrive-search-underscore-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::write(base.join(".gitignore"), "[._]*.s[a-v][a-z]\n*.swp\n").unwrap();
        std::fs::write(base.join("src/voice_server.py"), "def run_voice_server():\n    return include_str\n").unwrap();
        std::fs::write(base.join("src/a.rs"), "call(foo_bar); x.__init__\n").unwrap();
        std::fs::write(base.join("src/_server.swp"), "voice_server\n").unwrap();
        let mk = |q: &str, mode: &str, ww: bool| SearchOpts {
            root: base.to_string_lossy().to_string(),
            query: q.into(),
            mode: mode.into(),
            case_sensitive: false,
            whole_word: ww,
            regex: false,
            include: String::new(),
            exclude: String::new(),
            use_ignore: true,
            max_results: None,
        };
        let content = |q: &str, ww: bool| search(mk(q, "content", ww)).unwrap().total_matches;
        assert_eq!(content("voice_server", false), 1, "ignored .swp must be skipped");
        assert_eq!(content("_server", false), 1);
        assert_eq!(content("voice＿server", false), 1, "full-width underscore");
        assert_eq!(content("run_voice_server", true), 1);
        assert_eq!(content("voice_server", true), 0, "part of run_voice_server");
        assert_eq!(content("__init__", true), 1);
        assert_eq!(content("call(", true), 1, "edge non-word char");
        let files = |q: &str| search(mk(q, "files", false)).unwrap().files.into_iter().map(|h| h.rel.replace('\\', "/")).collect::<Vec<_>>();
        assert_eq!(files("voice_server"), vec!["src/voice_server.py"]);
        assert_eq!(files("_server"), vec!["src/voice_server.py"]);
        assert_eq!(files("voice＿"), vec!["src/voice_server.py"]);
        let _ = std::fs::remove_dir_all(&base);
    }
}
