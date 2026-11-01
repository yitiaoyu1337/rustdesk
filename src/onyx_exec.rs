// ONYX: 远程命令执行（任务类型 `exec`）
//
// 只有通过 Ed25519 验签的 OnyxTask 才会走到这里，签名由服务端私钥签发，
// 客户端拿不到签发能力（见 onyx_task.rs / ONYX_IMPL_PLAN.md §7）。
//
// 约定：
//   params: {"cmd":"...","shell":"cmd|powershell","timeout":30,"cwd":"...","confirm":"..."}
//   返回:   {"ok":true,"code":0,"stdout":"...","stderr":"...","duration_ms":12}
//
// 两道闸：
//   1) 危险命令先回 need_confirm + token，不执行
//   2) 每次执行（含被拦下的）都写审计日志
use hbb_common::log;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// 回传上限，避免一个 dir /s 把消息撑爆
const MAX_OUTPUT: usize = 16 * 1024;
/// 默认超时（秒）
const DEFAULT_TIMEOUT_SEC: u64 = 30;
/// 审计日志轮转阈值
const AUDIT_MAX_BYTES: u64 = 2 * 1024 * 1024;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 危险命令识别。命中就要求二次确认——防的是"手滑一条 del /s 把证据也删了"，
/// 不是防操作员本人（他本来就有全权）。
const RISK_RULES: &[(&str, &str)] = &[
    ("format ", "格式化磁盘"),
    ("diskpart", "分区操作"),
    ("del ", "删除文件"),
    ("erase ", "删除文件"),
    ("rd ", "删除目录"),
    ("rmdir ", "删除目录"),
    ("remove-item", "删除"),
    ("shutdown", "关机/重启"),
    ("restart-computer", "重启"),
    ("stop-computer", "关机"),
    ("reg delete", "删注册表"),
    ("bcdedit", "改启动项"),
    ("cipher /w", "擦除磁盘"),
    ("vssadmin delete", "删卷影副本"),
    ("wbadmin delete", "删备份"),
    ("net user", "账号操作"),
    ("net localgroup", "组操作"),
    ("takeown", "夺取所有权"),
    ("icacls", "改权限"),
];

fn risk_of(cmd: &str) -> Option<&'static str> {
    let l = cmd.to_lowercase();
    RISK_RULES
        .iter()
        .find(|(pat, _)| l.contains(pat))
        .map(|(_, cat)| *cat)
}

/// 确认令牌绑定「这条命令 + 这台机器」，换个命令就对不上
fn exec_token(cmd: &str, hwid: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"onyx-exec-v1|");
    h.update(cmd.as_bytes());
    h.update(b"|");
    h.update(hwid.as_bytes());
    format!("{:x}", h.finalize())[..16].to_owned()
}

fn now_sec() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn audit_dir() -> std::path::PathBuf {
    match std::env::var("ProgramData") {
        Ok(p) => std::path::Path::new(&p).join("Onyx"),
        Err(_) => std::env::temp_dir(),
    }
}

/// 追加一条审计记录（JSONL）。超过阈值就轮转成 .1
fn audit(rec: Value) {
    let dir = audit_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("exec_audit.log");
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > AUDIT_MAX_BYTES {
        let _ = std::fs::rename(&path, dir.join("exec_audit.log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{}", rec);
    }
}

/// 控制台输出解码：cmd.exe / 中文程序按系统 ANSI(GBK) 输出，
/// 直接 from_utf8_lossy 会整段变乱码（实测 `rd /?` 的帮助文字全是 `ɾ��`）。
fn decode_console(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_owned();
    }
    #[cfg(windows)]
    {
        use windows::Win32::Globalization::{MultiByteToWideChar, CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS};
        unsafe {
            let need = MultiByteToWideChar(CP_ACP, MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0), bytes, None);
            if need > 0 {
                let mut buf = vec![0u16; need as usize];
                let got = MultiByteToWideChar(
                    CP_ACP,
                    MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0),
                    bytes,
                    Some(&mut buf),
                );
                if got > 0 {
                    return String::from_utf16_lossy(&buf[..got as usize]);
                }
            }
        }
    }
    String::from_utf8_lossy(bytes).to_string()
}

fn truncate(mut s: String) -> String {
    if s.len() > MAX_OUTPUT {
        let mut end = MAX_OUTPUT;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("\n...[truncated]");
    }
    s
}

pub fn run(params: &[u8], task_id: &str, hwid: &str) -> String {
    let v: Value = serde_json::from_slice(params).unwrap_or(Value::Null);
    let cmd = v.get("cmd").and_then(|x| x.as_str()).unwrap_or("").trim();
    if cmd.is_empty() {
        return json!({"ok": false, "error": "cmd 为空"}).to_string();
    }
    let shell = v.get("shell").and_then(|x| x.as_str()).unwrap_or("cmd");
    let timeout_sec = v
        .get("timeout")
        .and_then(|x| x.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_SEC)
        .clamp(1, 600);
    let cwd = v.get("cwd").and_then(|x| x.as_str()).unwrap_or("");
    let confirm = v.get("confirm").and_then(|x| x.as_str()).unwrap_or("");

    // ── 第一道闸：危险命令必须二次确认 ──
    if let Some(risk) = risk_of(cmd) {
        let token = exec_token(cmd, hwid);
        if confirm != token {
            log::warn!("[ONYX] exec 危险命令被拦下（{risk}）: {cmd}");
            audit(json!({
                "ts": now_sec(), "task_id": task_id, "phase": "blocked",
                "risk": risk, "cmd": cmd, "shell": shell,
            }));
            return json!({
                "ok": false,
                "need_confirm": true,
                "risk": risk,
                "token": token,
                "hint": "重发同一条 cmd 并带上 confirm=<token>",
            })
            .to_string();
        }
    }

    log::warn!(
        "[ONYX] exec 任务: shell={} timeout={}s cmd={}",
        shell,
        timeout_sec,
        cmd
    );

    let mut command = if shell.eq_ignore_ascii_case("powershell") || shell.eq_ignore_ascii_case("pwsh") {
        let mut c = Command::new("powershell.exe");
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            cmd,
        ]);
        c
    } else {
        let mut c = Command::new("cmd.exe");
        c.args(["/C", cmd]);
        c
    };

    if !cwd.is_empty() {
        command.current_dir(cwd);
    }
    command.stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let started = Instant::now();
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => return json!({"ok": false, "error": format!("启动失败: {e}")}).to_string(),
    };

    let deadline = Duration::from_secs(timeout_sec);
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= deadline {
                    // 只 kill 直接子进程不够：`cmd /C ping ...` 里 ping 是孙子进程，
                    // 孙进程还握着管道写端，wait_with_output 会一直阻塞到它自己退出。
                    // 所以用 taskkill /T 把整棵树带走。
                    #[cfg(windows)]
                    {
                        use std::os::windows::process::CommandExt;
                        let pid = child.id().to_string();
                        let _ = Command::new("taskkill")
                            .args(["/F", "/T", "/PID", pid.as_str()])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .creation_flags(CREATE_NO_WINDOW)
                            .status();
                    }
                    let _ = child.kill();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return json!({"ok": false, "error": format!("等待失败: {e}")}).to_string(),
        }
    }

    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return json!({"ok": false, "error": format!("取输出失败: {e}")}).to_string(),
    };
    let duration_ms = started.elapsed().as_millis() as u64;
    let code = out.status.code().unwrap_or(-1);
    let stdout = truncate(decode_console(&out.stdout));
    let stderr = truncate(decode_console(&out.stderr));

    audit(json!({
        "ts": now_sec(), "task_id": task_id, "phase": "executed",
        "cmd": cmd, "shell": shell, "cwd": cwd,
        "risk": risk_of(cmd), "confirmed": !confirm.is_empty(),
        "code": code, "timed_out": timed_out, "duration_ms": duration_ms,
        "out_len": stdout.len() + stderr.len(),
    }));

    json!({
        "ok": !timed_out,
        "code": code,
        "timed_out": timed_out,
        "stdout": stdout,
        "stderr": stderr,
        "duration_ms": duration_ms,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_returns_output() {
        let out = run(br#"{"cmd":"echo onyx-exec-ok"}"#, "tk-test", "hw-test");
        let v: Value = serde_json::from_str(&out).expect("回执不是合法 JSON");
        assert_eq!(v["ok"], true, "{out}");
        assert!(
            v["stdout"].as_str().unwrap_or_default().contains("onyx-exec-ok"),
            "{out}"
        );
        assert_eq!(v["code"], 0);
    }

    #[test]
    fn empty_cmd_rejected() {
        let out = run(br#"{"cmd":"   "}"#, "tk-test", "hw-test");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
    }

    #[test]
    fn bad_json_rejected() {
        let out = run(b"not json", "tk-test", "hw-test");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
    }

    #[test]
    fn powershell_shell_works() {
        let out = run(br#"{"cmd":"Write-Output 'ps-ok'","shell":"powershell"}"#, "tk-test", "hw-test");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], true, "{out}");
        assert!(v["stdout"].as_str().unwrap_or_default().contains("ps-ok"), "{out}");
    }

    #[test]
    fn timeout_kills_long_command() {
        let out = run(br#"{"cmd":"ping -n 10 127.0.0.1 > nul","timeout":1}"#, "tk-test", "hw-test");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["timed_out"], true, "{out}");
        assert_eq!(v["ok"], false);
        let ms = v["duration_ms"].as_u64().unwrap_or(0);
        assert!(ms < 5000, "超时没生效: {ms}ms");
    }

    #[test]
    fn truncate_keeps_utf8_boundary() {
        let s = "中".repeat(MAX_OUTPUT);
        let t = truncate(s);
        assert!(t.ends_with("...[truncated]"));
        assert!(t.len() <= MAX_OUTPUT + 20);
    }

    #[test]
    fn risky_commands_need_confirm() {
        // `rd /?` 只是打印帮助，但含 "rd " 会被判成删除类，正好用来测闸门
        let out = run(br#"{"cmd":"rd /?"}"#, "tk-risk", "hw-test");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false, "{out}");
        assert_eq!(v["need_confirm"], true, "{out}");
        assert_eq!(v["risk"], "删除目录", "{out}");
        assert!(v["token"].as_str().unwrap_or("").len() == 16, "{out}");
        assert!(v["stdout"].is_null(), "被拦下时不该有输出: {out}");
    }

    #[test]
    fn token_matches_then_executes() {
        let first = run(br#"{"cmd":"rd /?"}"#, "tk-1", "hw-test");
        let v: Value = serde_json::from_str(&first).unwrap();
        let token = v["token"].as_str().unwrap().to_owned();

        let body = format!(r#"{{"cmd":"rd /?","confirm":"{token}"}}"#);
        let out = run(body.as_bytes(), "tk-2", "hw-test");
        let v2: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v2["ok"], true, "带正确 token 应执行: {out}");
    }

    #[test]
    fn token_is_bound_to_command_and_hwid() {
        let a = exec_token("del x", "hw-a");
        let b = exec_token("del y", "hw-a");
        let c = exec_token("del x", "hw-b");
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_eq!(a, exec_token("del x", "hw-a"));
    }

    #[test]
    fn safe_commands_run_without_confirm() {
        assert!(risk_of("echo hello").is_none());
        assert!(risk_of("ipconfig /all").is_none());
        assert_eq!(risk_of("shutdown /s"), Some("关机/重启"));
        assert_eq!(risk_of("reg delete HKCU\\X /f"), Some("删注册表"));
    }

    #[test]
    fn audit_log_written() {
        let _ = run(br#"{"cmd":"echo audit-probe"}"#, "tk-audit", "hw-test");
        let path = audit_dir().join("exec_audit.log");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(text.contains("tk-audit"), "审计日志里应有这条任务: {text}");
        assert!(text.contains("audit-probe"));
    }

    #[test]
    fn gbk_output_is_decoded() {
        // "删除" 的 GBK 编码
        let gbk = [0xC9u8, 0xBE, 0xB3, 0xFD];
        let s = decode_console(&gbk);
        assert_eq!(s, "删除", "GBK 输出没解对: {s:?}");
    }

    #[test]
    fn utf8_output_passes_through() {
        assert_eq!(decode_console("正常".as_bytes()), "正常");
        assert_eq!(decode_console(b""), "");
    }
}
