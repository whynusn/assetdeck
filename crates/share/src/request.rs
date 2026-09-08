//! 发送请求与设备发现的数据契约（D80 M0）。
//!
//! app-ui 组装 [`SendRequest`]（选区物化：uuid/文件名/类别/对象路径），
//! 经 stdin 行协议交给 share-worker 执行；`receiver_id`/`receiver_addrs`
//! 来自 mDNS 扫描结果（[`DeviceEntry`]）。校验失败即拒绝执行——坏输入
//! 不进网络层，与 [`crate::manifest`] 的清单校验同精神。

use std::net::SocketAddr;
use std::path::PathBuf;

use domain::AssetKind;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::manifest::{is_safe_display_name, MANIFEST_MAX_ITEMS};

/// QUIC ALPN（D80）：传输层与应用层之间的协议分隔符，收发两侧必须一致。
pub const ALPN: &str = "assetdeck-share/1";

/// mDNS 服务类型（D80 M0 设备发现）：instance = 设备名-短 id。
pub const MDNS_SERVICE: &str = "_assetdeck-share._udp.local.";

/// 一条待发送素材：库内物化结果 + 分类建议（接收侧仅展示，不替用户归类）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendItem {
    pub asset_uuid: Uuid,
    pub file_name: String,
    pub kind: AssetKind,
    pub size_bytes: u64,
    pub category_hint: Option<String>,
    /// 库内对象文件的绝对路径（worker 流式读取 + 现算 SHA-256）。
    pub path: PathBuf,
}

/// 一次显式推送：UI 选定目标设备与素材集合后交给 worker 的完整指令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendRequest {
    pub batch_id: Uuid,
    /// 本机显示名（M0 = 计算机名），随清单展示给接收者。
    pub sender_name: String,
    /// 接收方 EndpointId（z32 文本，52 字符）。
    pub receiver_id: String,
    /// 接收方直连地址（mDNS TXT 携带，SocketAddr 文本）。
    pub receiver_addrs: Vec<String>,
    pub items: Vec<SendItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestError {
    Empty,
    TooMany { count: usize },
    BadFileName { index: usize },
    BadSenderName,
    BadPeerId,
    BadAddr { value: String },
    NotAbsolute { index: usize },
}

impl core::fmt::Display for RequestError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RequestError::Empty => write!(f, "共享列表为空"),
            RequestError::TooMany { count } => {
                write!(f, "一次最多共享 {MANIFEST_MAX_ITEMS} 项（当前 {count}）")
            }
            RequestError::BadFileName { index } => {
                write!(f, "第 {index} 项文件名不安全，无法共享")
            }
            RequestError::BadSenderName => write!(f, "本机设备名不合法"),
            RequestError::BadPeerId => write!(f, "目标设备标识不合法"),
            RequestError::BadAddr { value } => write!(f, "目标设备地址不合法：{value}"),
            RequestError::NotAbsolute { index } => {
                write!(f, "第 {index} 项对象路径不是绝对路径")
            }
        }
    }
}

impl SendRequest {
    /// 结构性守卫：条数复用清单上限（MANIFEST_MAX_ITEMS，D80 红线 1 的
    /// 「不能整库静默倒灌」在上游入口同样成立）；对端 id/地址与对象路径
    /// 在此定死合法形状，引擎侧不再兜底。
    pub fn validate(&self) -> Result<(), RequestError> {
        if self.items.is_empty() {
            return Err(RequestError::Empty);
        }
        if self.items.len() > MANIFEST_MAX_ITEMS {
            return Err(RequestError::TooMany {
                count: self.items.len(),
            });
        }
        let name_ok = !self.sender_name.is_empty()
            && self.sender_name.chars().count() <= 64
            && self
                .sender_name
                .chars()
                .all(|c| !c.is_control() && c != '\t' && c != '\n');
        if !name_ok {
            return Err(RequestError::BadSenderName);
        }
        // z32（base32 小写字母数字表）编码 32 字节恒为 52 字符。
        let id_ok = self.receiver_id.len() == 52
            && self
                .receiver_id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        if !id_ok {
            return Err(RequestError::BadPeerId);
        }
        for value in &self.receiver_addrs {
            value
                .parse::<SocketAddr>()
                .map_err(|_| RequestError::BadAddr {
                    value: value.clone(),
                })?;
        }
        for (index, item) in self.items.iter().enumerate() {
            if !is_safe_display_name(&item.file_name) {
                return Err(RequestError::BadFileName { index });
            }
            if !item.path.is_absolute() {
                return Err(RequestError::NotAbsolute { index });
            }
        }
        Ok(())
    }
}

/// mDNS 扫描结果：另一台在线实例（id + 设备名 + 直连地址集）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEntry {
    pub id: String,
    pub name: String,
    pub addrs: Vec<String>,
}

impl DeviceEntry {
    pub fn validate(&self) -> Result<(), RequestError> {
        let name_ok = !self.name.is_empty()
            && self.name.chars().count() <= 64
            && self
                .name
                .chars()
                .all(|c| !c.is_control() && c != '\t' && c != '\n');
        if !name_ok {
            return Err(RequestError::BadSenderName);
        }
        let id_ok = self.id.len() == 52
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        if !id_ok {
            return Err(RequestError::BadPeerId);
        }
        for value in &self.addrs {
            value
                .parse::<SocketAddr>()
                .map_err(|_| RequestError::BadAddr {
                    value: value.clone(),
                })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::MANIFEST_MAX_ITEMS;

    // 52 字符 z32 形状（小写字母+数字）的合法 id 构造。
    fn peer_id() -> String {
        let alphabet = b"0123456789abcdefghijklmnopqrstuvwxyz";
        (0..52).map(|i| alphabet[i % 36] as char).collect()
    }

    fn item(name: &str) -> SendItem {
        SendItem {
            asset_uuid: Uuid::new_v4(),
            file_name: name.to_string(),
            kind: AssetKind::Image,
            size_bytes: 128,
            category_hint: None,
            path: PathBuf::from(r"C:\lib\objects\x\raw.png"),
        }
    }

    fn request() -> SendRequest {
        SendRequest {
            batch_id: Uuid::new_v4(),
            sender_name: "WORKSTATION".to_string(),
            receiver_id: peer_id(),
            receiver_addrs: vec!["192.168.1.8:48920".to_string()],
            items: vec![item("photo.png")],
        }
    }

    #[test]
    fn valid_request_passes() {
        request().validate().unwrap();
    }

    #[test]
    fn empty_and_oversized_are_rejected() {
        let mut req = request();
        req.items.clear();
        assert!(matches!(req.validate(), Err(RequestError::Empty)));

        let mut req = request();
        req.items = (0..MANIFEST_MAX_ITEMS + 1).map(|_| item("a.png")).collect();
        assert!(matches!(req.validate(), Err(RequestError::TooMany { .. })));
    }

    #[test]
    fn path_like_name_is_rejected() {
        let mut req = request();
        req.items[0].file_name = "..\\evil.png".to_string();
        assert!(matches!(
            req.validate(),
            Err(RequestError::BadFileName { index: 0 })
        ));
    }

    #[test]
    fn relative_object_path_is_rejected() {
        let mut req = request();
        req.items[0].path = PathBuf::from("objects/x/raw.png");
        assert!(matches!(
            req.validate(),
            Err(RequestError::NotAbsolute { index: 0 })
        ));
    }

    #[test]
    fn malformed_peer_id_and_addr_are_rejected() {
        let mut req = request();
        req.receiver_id = "short".to_string();
        assert_eq!(req.validate(), Err(RequestError::BadPeerId));

        let mut req = request();
        req.receiver_addrs = vec!["not-an-addr".to_string()];
        assert!(matches!(req.validate(), Err(RequestError::BadAddr { .. })));
    }

    #[test]
    fn sender_name_with_control_char_is_rejected() {
        let mut req = request();
        req.sender_name = "bad\tname".to_string();
        assert_eq!(req.validate(), Err(RequestError::BadSenderName));
    }

    #[test]
    fn device_entry_roundtrip_and_validation() {
        let entry = DeviceEntry {
            id: peer_id(),
            name: "桌面机".to_string(),
            addrs: vec!["[fe80::1%12]:40000".to_string()],
        };
        entry.validate().unwrap();
        let json = serde_json::to_string(&entry).unwrap();
        let back: DeviceEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn request_serde_roundtrip() {
        let req = request();
        let json = serde_json::to_string(&req).unwrap();
        let back: SendRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }
}
