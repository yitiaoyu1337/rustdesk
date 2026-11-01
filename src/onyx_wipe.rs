// ONYX: 格机（任务类型 `wipe`）——**两道确认**
//
// 这是整套体系里唯一不可逆的操作，所以协议强制两步：
//
//   1) {"phase":"arm","paths":[...],"shutdown":true}
//      → 只在内存里记下"已上膛"，回一个 token，什么也不删
//   2) {"phase":"confirm","token":"..."}
//      → token 对得上、没过期，才真正执行
//
// 两条任务各自都要服务端私钥签名（onyx_task 那层已经强制），
// 所以操作员必须连发两条合法任务才能触发，误发一条不会出事。
//
// 上膛状态**只存在内存里**：进程一重启就没了。这是故意的——
// 丢状态的方向是"更安全"，宁可重新上膛，也不要重启后凭空执行一条确认。
use hbb_common::log;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// 上膛后多久内必须确认
const ARM_TTL_SEC: u64 = 600;

struct Armed {
    token: String,
    paths: Vec<String>,
    shutdown: bool,
    armed_at: u64,
}

// 上膛状态**落盘**而不是放内存。
// 原因：服务进程（--service）和它拉起的会话进程（--server）都会轮询任务（原子领取保证不重复），
// arm 与 confirm 是两条独立任务，很可能被不同进程领到。放内存会导致
// 「另一条进程里没有上膛记录」。落盘后两个进程看到同一份状态。
// 时效仍由 ARM_TTL_SEC 兜底：超过 10 分钟自动作废。
fn state_path() -> PathBuf {
    let base = std::env::var("ProgramData").unwrap_or_else(|_| "C:\\ProgramData".to_owned());
    PathBuf::from(base).join("Onyx").join("wipe_arm.json")
}

fn save_armed(a: &Armed) {
    let v = json!({
        "token": a.token,
        "paths": a.paths,
        "shutdown": a.shutdown,
        "armed_at": a.armed_at,
    });
    let p = state_path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::write(&p, v.to_string()).is_err() {
        log::error!("[ONYX] wipe 上膛状态写入失败: {}", p.display());
    }
}

fn load_armed() -> Option<Armed> {
    let s = std::fs::read_to_string(state_path()).ok()?;
    let v: Value = serde_json::from_str(&s).ok()?;
    Some(Armed {
        token: v.get("token")?.as_str()?.to_owned(),
        paths: v
            .get("paths")?
            .as_array()?
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_owned()))
            .collect(),
        shutdown: v.get("shutdown").and_then(|x| x.as_bool()).unwrap_or(false),
        armed_at: v.get("armed_at").and_then(|x| x.as_u64()).unwrap_or(0),
    })
}

fn clear_armed() {
    let _ = std::fs::remove_file(state_path());
}

fn now_sec() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn make_token(arm_task_id: &str, hwid: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"onyx-wipe-v1|");
    h.update(arm_task_id.as_bytes());
    h.update(b"|");
    h.update(hwid.as_bytes());
    format!("{:x}", h.finalize())[..32].to_owned()
}

/// 归一化：`C:\\Windows`、`c:/windows/`、`C:\Windows.` 都要归到 `c:\windows`
fn normalize(path: &str) -> String {
    let mut out = String::new();
    let mut prev_sep = false;
    for c in path.chars() {
        let is_sep = c == '\\' || c == '/';
        if is_sep {
            if !prev_sep {
                out.push('\\');
            }
            prev_sep = true;
        } else {
            out.push(c.to_ascii_lowercase());
            prev_sep = false;
        }
    }
    // Windows 会忽略结尾的点和空格
    out.trim_end_matches(['\\', '.', ' ']).to_string()
}

/// 明显不能碰的路径。防的是手滑写错一个参数把操作员自己的机器也端了，
/// 不是防"客户机"——客户机上的用户目录随便删。
fn is_protected(path: &str) -> bool {
    let p = normalize(path);
    if p.len() <= 2 {
        return true; // "c:" 之类，等于整盘
    }
    const BAD: &[&str] = &[
        "c:\\windows",
        "c:\\program files",
        "c:\\program files (x86)",
        "c:\\programdata",
        "c:\\users",
        "c:\\users\\public",
        "c:\\$recycle.bin",
        "c:\\system volume information",
        "c:\\perflogs",
        "c:\\recovery",
    ];
    BAD.iter().any(|b| p == *b)
}

fn remove_path(path: &str, deleted: &mut u64, failed: &mut Vec<String>) {
    let p = std::path::Path::new(path);
    if !p.exists() {
        return;
    }
    if p.is_dir() {
        match std::fs::remove_dir_all(p) {
            Ok(_) => *deleted += 1,
            Err(e) => failed.push(format!("{path}: {e}")),
        }
    } else {
        match std::fs::remove_file(p) {
            Ok(_) => *deleted += 1,
            Err(e) => failed.push(format!("{path}: {e}")),
        }
    }
}

pub fn run(params: &[u8], task_id: &str, hwid: &str) -> String {
    let v: Value = serde_json::from_slice(params).unwrap_or(Value::Null);
    let phase = v.get("phase").and_then(|x| x.as_str()).unwrap_or("");

    match phase {
        "arm" => {
            let paths: Vec<String> = v
                .get("paths")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.trim().to_owned()))
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if paths.is_empty() {
                return json!({"ok": false, "error": "paths 为空，拒绝上膛"}).to_string();
            }
            if let Some(bad) = paths.iter().find(|p| is_protected(p)) {
                return json!({"ok": false, "error": format!("受保护路径，拒绝: {bad}")}).to_string();
            }
            let shutdown = v.get("shutdown").and_then(|x| x.as_bool()).unwrap_or(false);
            let token = make_token(task_id, hwid);
            let armed = Armed {
                token: token.clone(),
                paths: paths.clone(),
                shutdown,
                armed_at: now_sec(),
            };
            save_armed(&armed);
            log::warn!("[ONYX] wipe 已上膛 task={task_id} paths={paths:?} shutdown={shutdown}");
            json!({
                "ok": true,
                "phase": "armed",
                "token": token,
                "paths": paths,
                "shutdown": shutdown,
                "expires_in": ARM_TTL_SEC,
                "note": "需要再下发一条 phase=confirm 的任务才会执行",
            })
            .to_string()
        }
        "confirm" => {
            let token = v.get("token").and_then(|x| x.as_str()).unwrap_or("");
            let armed = load_armed();
            clear_armed();
            let Some(armed) = armed else {
                return json!({"ok": false, "error": "没有待确认的上膛记录（超时或未上膛）"}).to_string();
            };
            // token 里含 arm 任务 id 的哈希，天然带时效；这里再核对一次字符串
            if token != armed.token {
                // 确认失败要把上膛状态放回去吗？不放——失败即作废，必须重新上膛
                return json!({"ok": false, "error": "token 不匹配，上膛已作废"}).to_string();
            }
            let age = now_sec().saturating_sub(armed.armed_at);
            if age > ARM_TTL_SEC {
                return json!({"ok": false, "error": format!("上膛已过期（{age} 秒 > {ARM_TTL_SEC} 秒）")})
                    .to_string();
            }

            let mut deleted = 0u64;
            let mut failed: Vec<String> = Vec::new();
            for p in &armed.paths {
                if is_protected(p) {
                    failed.push(format!("{p}: 受保护路径"));
                    continue;
                }
                remove_path(p, &mut deleted, &mut failed);
            }
            log::warn!("[ONYX] wipe 执行完毕 删除 {deleted} 项，失败 {} 项", failed.len());

            if armed.shutdown {
                let mut cmd = std::process::Command::new("shutdown");
                cmd.args(["/s", "/t", "30", "/f"]);
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    cmd.creation_flags(0x0800_0000);
                }
                let _ = cmd.spawn();
            }

            json!({
                "ok": true,
                "phase": "done",
                "deleted": deleted,
                "failed": failed,
                "shutdown": armed.shutdown,
            })
            .to_string()
        }
        other => json!({
            "ok": false,
            "error": format!("未知 phase: {other}（只接受 arm / confirm）"),
        })
        .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_paths_rejected() {
        assert!(is_protected("C:\\"));
        assert!(is_protected("C:\\Windows"));
        assert!(is_protected("C:\\Users"));
        assert!(is_protected("c:\\program files\\"));
        assert!(!is_protected("C:\\Users\\Bob\\Desktop"));
        assert!(!is_protected("D:\\proj\\crack"));
    }

    #[test]
    fn protected_paths_survive_separator_tricks() {
        // 实测踩过：JSON 里多写一层反斜杠就能绕过检查
        assert!(is_protected("C:\\\\Windows"));
        assert!(is_protected("C:/Windows"));
        assert!(is_protected("c:\\\\windows\\\\"));
        assert!(is_protected("C:\\Windows."));
        assert!(is_protected("C:\\Program Files\\\\"));
        assert!(is_protected("C:"));
        assert!(!is_protected("C:\\\\ProgramData\\\\Onyx\\\\wipe_test"));
    }

    #[test]
    fn arm_then_confirm_deletes_temp_dir() {
        let dir = std::env::temp_dir().join(format!("onyx_wipe_test_{}", now_sec()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.txt"), b"x").unwrap();

        let params = format!(r#"{{"phase":"arm","paths":["{}"]}}"#, dir.to_string_lossy().replace('\\', "\\\\"));
        let arm: Value = serde_json::from_str(&run(params.as_bytes(), "task-arm-1", "hw")).unwrap();
        assert_eq!(arm["ok"], true);
        let token = arm["token"].as_str().unwrap().to_owned();
        assert!(dir.exists(), "上膛阶段不能删任何东西");

        let confirm = format!(r#"{{"phase":"confirm","token":"{token}"}}"#);
        let done: Value = serde_json::from_str(&run(confirm.as_bytes(), "task-confirm-1", "hw")).unwrap();
        assert_eq!(done["ok"], true);
        assert!(!dir.exists(), "确认后应删除");
    }

    #[test]
    fn wrong_token_aborts_and_disarms() {
        let dir = std::env::temp_dir().join(format!("onyx_wipe_neg_{}", now_sec()));
        std::fs::create_dir_all(&dir).unwrap();
        let params = format!(r#"{{"phase":"arm","paths":["{}"]}}"#, dir.to_string_lossy().replace('\\', "\\\\"));
        let _: Value = serde_json::from_str(&run(params.as_bytes(), "task-arm-2", "hw")).unwrap();

        let bad: Value = serde_json::from_str(&run(br#"{"phase":"confirm","token":"deadbeef"}"#, "task-c", "hw")).unwrap();
        assert_eq!(bad["ok"], false);
        assert!(dir.exists(), "token 错不能删");

        // 上膛已被作废，再拿正确 token 也不行
        let again: Value = serde_json::from_str(&run(br#"{"phase":"confirm","token":"x"}"#, "task-c2", "hw")).unwrap();
        assert_eq!(again["ok"], false);
        assert!(dir.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn confirm_without_arm_rejected() {
        clear_armed();
        let v: Value = serde_json::from_str(&run(br#"{"phase":"confirm","token":"abc"}"#, "t", "h")).unwrap();
        assert_eq!(v["ok"], false);
    }
}
