//! mDNS 设备发现（D80 M0）：公告 + 浏览共享同一个 [`ServiceDaemon`]。
//!
//! 约定：
//! - 服务类型 [`share::MDNS_SERVICE`]；instance = `<设备名>-<id 前 6 位>`，
//!   id 后缀保证同机名双实例不撞；
//! - TXT：`proto=share1`（协议档位）、`id=<z32>`、`n=<设备名>`、
//!   `a=<ip:port;ip:port>`（endpoint 直连地址，分号连接）；
//! - 地址只从 TXT 取，不依赖 A 记录（注册时不填接口 IP），跨机解析零
//!   额外往返；TXT 单值 ≤255 字节，地址串超限截断（多网卡机器优先保证
//!   IPv4 在前）。
//!
//! M0 边界：启动时公告一次；会话中地址变化（DHCP 换址）不再刷新公告，
//! 对端重开会话即可看到新地址——M1 再引入地址变更重公告。

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use share::{DeviceEntry, MDNS_SERVICE};

const PROTO_VERSION: &str = "share1";
/// mDNS 浏览窗口：覆盖默认 1s 一问的完整询问循环。
pub const SCAN_WINDOW: Duration = Duration::from_millis(2500);
/// TXT `a` 值上限（RFC 6763 单条 TXT ≤255 字节，键名占位留余量）。
const ADDR_TXT_MAX: usize = 220;

/// 常驻公告句柄：drop 不隐式注销（worker 退出路径显式调 [`Announcer::unregister`]，
/// 避免进程退出竞态吞掉 Goodbye 包）。
pub struct Announcer {
    daemon: ServiceDaemon,
    fullname: String,
}

/// 设备名净化：去控制符/制表换行、截到 64 字符（与 share::SendRequest 校验
/// 对齐），空名回落「AssetDeck」。
pub fn sanitize_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .filter(|c| !c.is_control() && *c != '\t' && *c != '\n')
        .take(64)
        .collect();
    let trimmed = cleaned.trim().to_string();
    if trimmed.is_empty() {
        "AssetDeck".to_string()
    } else {
        trimmed
    }
}

fn addr_txt(addrs: &[SocketAddr]) -> String {
    let mut v4_first: Vec<String> = Vec::new();
    let mut v6: Vec<String> = Vec::new();
    for addr in addrs {
        if addr.ip().is_loopback() {
            continue;
        }
        let text = addr.to_string();
        if addr.is_ipv4() {
            v4_first.push(text);
        } else {
            v6.push(text);
        }
    }
    v4_first.extend(v6);
    let mut joined = v4_first.join(";");
    if joined.len() > ADDR_TXT_MAX {
        // 从尾部丢弃直到不超限（v4 在前已保证优先保留）。
        while joined.len() > ADDR_TXT_MAX {
            let Some(cut) = joined.rfind(';') else {
                joined.clear();
                break;
            };
            joined.truncate(cut);
        }
    }
    joined
}

impl Announcer {
    pub fn register(
        daemon: &ServiceDaemon,
        name: &str,
        id_z32: &str,
        port: u16,
        addrs: &[SocketAddr],
    ) -> Result<Self, String> {
        let name = sanitize_name(name);
        if id_z32.len() < 6 {
            return Err("endpoint id 过短，无法构造 mDNS 实例名".to_string());
        }
        let short = &id_z32[..6];
        let instance = format!("{name}-{short}");
        let host = format!("assetdeck-{short}.local.");
        let addr_value = addr_txt(addrs);
        let props = [
            ("proto", PROTO_VERSION),
            ("id", id_z32),
            ("n", name.as_str()),
            ("a", addr_value.as_str()),
        ];
        let info = ServiceInfo::new(MDNS_SERVICE, &instance, &host, (), port, &props[..])
            .map_err(|e| format!("mDNS ServiceInfo 构造失败: {e}"))?;
        let fullname = info.get_fullname().to_string();
        daemon
            .register(info)
            .map_err(|e| format!("mDNS 注册失败: {e}"))?;
        Ok(Self {
            daemon: daemon.clone(),
            fullname,
        })
    }

    pub fn unregister(self) {
        let _ = self.daemon.unregister(&self.fullname);
    }
}

/// 一轮浏览：窗口内收集 Resolved 事件，合并成设备表（同 id 后到者覆盖，
/// 地址取并集去重）。自己（`self_id`）剔除。
pub fn scan(daemon: &ServiceDaemon, self_id: &str, window: Duration) -> Vec<DeviceEntry> {
    let Ok(rx) = daemon.browse(MDNS_SERVICE) else {
        return Vec::new();
    };
    let deadline = Instant::now() + window;
    let mut devices: Vec<DeviceEntry> = Vec::new();
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let props = info.get_properties();
                let Some(id) = props.get_property_val_str("id") else {
                    continue;
                };
                if id == self_id || id.len() != 52 || !id.bytes().all(|b| b.is_ascii_alphanumeric())
                {
                    continue;
                }
                let name = sanitize_name(props.get_property_val_str("n").unwrap_or("未知设备"));
                let addrs: Vec<String> = props
                    .get_property_val_str("a")
                    .unwrap_or("")
                    .split(';')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
                if addrs.is_empty() {
                    continue;
                }
                match devices.iter_mut().find(|d| d.id == id) {
                    Some(entry) => {
                        entry.name = name;
                        for addr in addrs {
                            if !entry.addrs.contains(&addr) {
                                entry.addrs.push(addr);
                            }
                        }
                    }
                    None => devices.push(DeviceEntry {
                        id: id.to_string(),
                        name,
                        addrs,
                    }),
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let _ = daemon.stop_browse(MDNS_SERVICE);
    devices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_name_strips_control_and_truncates() {
        assert_eq!(sanitize_name("WS\x01-\tX"), "WS-X");
        assert_eq!(sanitize_name("  "), "AssetDeck");
        assert_eq!(sanitize_name(""), "AssetDeck");
        assert_eq!(sanitize_name(&"长".repeat(100)).chars().count(), 64);
    }

    #[test]
    fn addr_txt_prefers_v4_and_drops_loopback() {
        let addrs = vec![
            "[fe80::1]:40000".parse::<SocketAddr>().unwrap(),
            "192.168.1.8:40000".parse().unwrap(),
            "127.0.0.1:40000".parse().unwrap(),
        ];
        let text = addr_txt(&addrs);
        assert_eq!(text, "192.168.1.8:40000;[fe80::1]:40000");
    }

    #[test]
    fn addr_txt_truncates_when_over_limit() {
        let addrs: Vec<SocketAddr> = (0..40)
            .map(|i| format!("192.168.{i}.{i}:{}", 4000 + i).parse().unwrap())
            .collect();
        let text = addr_txt(&addrs);
        assert!(text.len() <= ADDR_TXT_MAX);
    }

    #[test]
    fn scan_ignores_garbage_and_self_without_daemon_crash() {
        // 无 mDNS 网络环境的降级路径：browse 失败返回空表而非 panic。
        let daemon = ServiceDaemon::new().expect("daemon");
        let devices = scan(&daemon, "a".repeat(52).as_str(), Duration::from_millis(50));
        assert!(devices.is_empty());
    }
}
